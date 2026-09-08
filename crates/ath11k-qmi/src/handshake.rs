//! Firmware boot state machine corresponding to ath11k qmi.c's ordered event work.
use crate::wire::{
    self, BdfDownloadRequest, BdfType, CapabilityResponse, DeviceInfoResponse,
    HostCapabilityRequest, Indication, IndicationRegisterRequest, M3InfoRequest, MemorySegment,
    MemorySegmentResponse, MessageId, RespondMemoryRequest, WlanConfigRequest, WlanIniRequest,
    WlanModeRequest,
};
use crate::{FirmwareReady, Handshake, Incoming, QmiError, Request, Response, Transport};
use alloc::vec::Vec;

const DEVICE_BAR_SIZE: u32 = 0x20_0000;
const BDF_NAME_SIZE: u32 = 64;

/// Firmware data selected by the composition root (board-2.bin parsing remains above QMI).
pub trait FirmwareAssets {
    fn board_data(&mut self, board_id: u32) -> Result<Vec<u8>, QmiError>;
    /// `None` means the caller explicitly authorizes factory-test boot without caldata.
    fn calibration_data(&mut self) -> Result<Option<Vec<u8>>, QmiError>;
    fn regulatory_data(&mut self) -> Result<Option<Vec<u8>>, QmiError>;
    fn m3_firmware(&mut self) -> Result<Option<Vec<u8>>, QmiError>;
}

/// Platform memory effects. Returned addresses are device-visible addresses.
pub trait MemoryProvider {
    fn provision(
        &mut self,
        requested: &[MemorySegment],
    ) -> Result<Vec<MemorySegmentResponse>, QmiError>;
    fn load_m3(&mut self, firmware: &[u8]) -> Result<MemoryRegion, QmiError>;
    fn map_device_bar(&mut self, address: u64, size: u32) -> Result<(), QmiError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemoryRegion {
    pub device_address: u64,
    pub size: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DriverEvent {
    ServerArrived,
    ServerExited,
    RequestMemory,
    FirmwareMemoryReady,
    FirmwareReady(FirmwareReady),
    FirmwareInitDone(FirmwareReady),
    ColdBootCalibrationDone,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HandshakeConfig {
    pub target_mem_mode: u32,
    pub cal_done: bool,
    pub fixed_firmware_memory: bool,
    pub hybrid_bus: bool,
    pub supports_regdb: bool,
    pub m3_support: bool,
    pub cold_boot_calibration: bool,
    pub timeout_ns: u64,
    pub cold_boot_timeout_ns: u64,
}

impl Default for HandshakeConfig {
    fn default() -> Self {
        // WCN6750 hw1.0 values in the pinned core.c hw_params table.
        Self {
            target_mem_mode: 0,
            cal_done: false,
            fixed_firmware_memory: true,
            hybrid_bus: true,
            supports_regdb: true,
            m3_support: false,
            cold_boot_calibration: true,
            timeout_ns: 10_000_000_000,
            cold_boot_timeout_ns: 60_000_000_000,
        }
    }
}

pub struct Wcn6750Handshake<'a> {
    config: HandshakeConfig,
    assets: &'a mut dyn FirmwareAssets,
    memory: &'a mut dyn MemoryProvider,
    firmware_version: u32,
    board_id: u32,
    eeprom_caldata: bool,
    pending: Vec<crate::RawIndication>,
}

impl<'a> Wcn6750Handshake<'a> {
    pub fn new(
        config: HandshakeConfig,
        assets: &'a mut dyn FirmwareAssets,
        memory: &'a mut dyn MemoryProvider,
    ) -> Self {
        Self {
            config,
            assets,
            memory,
            firmware_version: 0,
            board_id: 0xff,
            eeprom_caldata: false,
            pending: Vec::new(),
        }
    }

    /// Portable replacement for `ath11k_qmi_init_service`.
    pub fn init_service(&mut self, transport: &mut dyn Transport) -> Result<(), QmiError> {
        transport.start_service(wire::SERVICE_VERSION, wire::WCN6750_SERVICE_INSTANCE)
    }

    /// Portable replacement for `ath11k_qmi_deinit_service`.
    pub fn deinit_service(&mut self, transport: &mut dyn Transport) {
        transport.stop_service();
        self.pending.clear();
    }

    /// The QMI-owned portion of `ath11k_qmi_firmware_start`.
    pub fn firmware_start(
        &mut self,
        transport: &mut dyn Transport,
        config: &WlanConfigRequest,
        mode: u32,
        firmware_diagnostics: bool,
    ) -> Result<(), QmiError> {
        if firmware_diagnostics {
            self.exchange(
                transport,
                WlanIniRequest {
                    enable_firmware_log: Some(1),
                }
                .encode()?,
            )?;
        }
        self.exchange(transport, config.encode()?)?;
        self.exchange(
            transport,
            WlanModeRequest {
                mode,
                hardware_debug: Some(0),
            }
            .encode()?,
        )?;
        Ok(())
    }

    /// The QMI-owned portion of `ath11k_qmi_firmware_stop`.
    pub fn firmware_stop(&mut self, transport: &mut dyn Transport) -> Result<(), QmiError> {
        let request = WlanModeRequest {
            mode: 4,
            hardware_debug: Some(0),
        }
        .encode()?;
        let expected = request.message_id();
        let transaction = transport.send(request)?;
        let deadline = transport.now_ns().saturating_add(self.config.timeout_ns);
        loop {
            let remaining = deadline.saturating_sub(transport.now_ns());
            if remaining == 0 {
                return Err(QmiError::Timeout);
            }
            match transport.receive(remaining) {
                Ok(Incoming::Response(response)) if response.transaction_id() == transaction => {
                    if response.message_id() != expected {
                        return Err(QmiError::Malformed);
                    }
                    let status = wire::decode_response(response.bytes())?;
                    return if status.is_success() {
                        Ok(())
                    } else {
                        Err(QmiError::Protocol(status))
                    };
                }
                Ok(Incoming::Response(_)) => continue,
                Ok(Incoming::Indication(indication)) => self.pending.push(indication),
                Ok(Incoming::ServerExited) | Err(QmiError::Disconnected) => return Ok(()),
                Ok(Incoming::ServerArrived) => continue,
                Err(error) => return Err(error),
            }
        }
    }

    /// Start the source `ATH11K_FIRMWARE_MODE_COLD_BOOT` calibration pass.
    pub fn start_cold_boot_calibration(
        &mut self,
        transport: &mut dyn Transport,
    ) -> Result<(), QmiError> {
        self.exchange(
            transport,
            WlanModeRequest {
                mode: 7,
                hardware_debug: Some(0),
            }
            .encode()?,
        )?;
        Ok(())
    }

    fn server_arrived(&mut self, transport: &mut dyn Transport) -> Result<(), QmiError> {
        self.exchange(
            transport,
            IndicationRegisterRequest {
                request_memory: (!self.config.fixed_firmware_memory).then_some(1),
                fw_memory_ready: (!self.config.fixed_firmware_memory).then_some(1),
                ..IndicationRegisterRequest::wcn6750()
            }
            .encode()?,
        )?;
        self.exchange(
            transport,
            HostCapabilityRequest {
                num_clients: Some(1),
                bdf_support: Some(1),
                m3_support: self.config.m3_support.then_some(1),
                m3_cache_support: self.config.m3_support.then_some(1),
                cal_done: Some(u8::from(self.config.cal_done)),
                mem_cfg_mode: Some(self.config.target_mem_mode as u8),
                ..HostCapabilityRequest::default()
            }
            .encode()?,
        )?;
        if self.config.fixed_firmware_memory {
            self.load_bdf(transport, false)?;
        }
        Ok(())
    }

    /// Process one native QMI service/indication event and perform the QMI-owned
    /// response work. Core consumes the returned source-shaped lifecycle event.
    pub fn process_next_event(
        &mut self,
        transport: &mut dyn Transport,
    ) -> Result<DriverEvent, QmiError> {
        self.process_next_event_with_timeout(transport, self.config.timeout_ns)
    }

    fn process_next_event_with_timeout(
        &mut self,
        transport: &mut dyn Transport,
        timeout_ns: u64,
    ) -> Result<DriverEvent, QmiError> {
        if !self.pending.is_empty() {
            return self.process_indication(transport, timeout_ns);
        }
        match transport.receive(timeout_ns)? {
            Incoming::ServerArrived => {
                self.server_arrived(transport)?;
                Ok(DriverEvent::ServerArrived)
            }
            Incoming::ServerExited => Ok(DriverEvent::ServerExited),
            Incoming::Indication(indication) => {
                self.pending.push(indication);
                self.process_indication(transport, timeout_ns)
            }
            Incoming::Response(_) => Err(QmiError::Malformed),
        }
    }

    fn ready(&self) -> FirmwareReady {
        FirmwareReady {
            firmware_version: self.firmware_version,
            target_mem_mode: self.config.target_mem_mode,
        }
    }

    fn process_indication(
        &mut self,
        transport: &mut dyn Transport,
        timeout_ns: u64,
    ) -> Result<DriverEvent, QmiError> {
        match self.next_indication(transport, timeout_ns)? {
            Indication::RequestMemory(request) => {
                let segments = self.memory.provision(&request.segments)?;
                self.exchange(transport, RespondMemoryRequest { segments }.encode()?)?;
                Ok(DriverEvent::RequestMemory)
            }
            Indication::FirmwareMemoryReady => {
                self.load_bdf(transport, true)?;
                Ok(DriverEvent::FirmwareMemoryReady)
            }
            Indication::FirmwareReady => {
                self.config.cal_done = true;
                Ok(DriverEvent::FirmwareReady(self.ready()))
            }
            Indication::FirmwareInitDone => Ok(DriverEvent::FirmwareInitDone(self.ready())),
            Indication::ColdBootCalibrationDone => {
                self.config.cal_done = true;
                Ok(DriverEvent::ColdBootCalibrationDone)
            }
        }
    }

    fn exchange(
        &mut self,
        transport: &mut dyn Transport,
        request: Request,
    ) -> Result<Response, QmiError> {
        let expected = request.message_id();
        let transaction = transport.send(request)?;
        let deadline = transport.now_ns().saturating_add(self.config.timeout_ns);
        loop {
            let remaining = deadline.saturating_sub(transport.now_ns());
            if remaining == 0 {
                return Err(QmiError::Timeout);
            }
            match transport.receive(remaining)? {
                Incoming::Response(response) if response.transaction_id() == transaction => {
                    if response.message_id() != expected {
                        return Err(QmiError::Malformed);
                    }
                    let status = wire::decode_response(response.bytes())?;
                    if !status.is_success() {
                        return Err(QmiError::Protocol(status));
                    }
                    return Ok(response);
                }
                Incoming::Response(_) => continue,
                Incoming::Indication(indication) => self.pending.push(indication),
                Incoming::ServerArrived => continue,
                Incoming::ServerExited => return Err(QmiError::Disconnected),
            }
        }
    }

    fn capabilities(&mut self, transport: &mut dyn Transport) -> Result<(), QmiError> {
        let response = self.exchange(transport, wire::empty_request()?)?;
        let cap = CapabilityResponse::decode(&response)?;
        if let Some(fw) = cap.firmware {
            self.firmware_version = fw.version;
        }
        self.board_id = cap.board_id.unwrap_or(0xff);
        self.eeprom_caldata = uses_eeprom_caldata(cap.eeprom_read_timeout);

        if self.config.hybrid_bus {
            let request = Request::from_tlv_bytes(MessageId::DeviceInfo, Vec::new())?;
            let response = self.exchange(transport, request)?;
            let info = DeviceInfoResponse::decode(&response)?;
            let (Some(address), Some(size)) = (info.bar_address, info.bar_size) else {
                return Err(QmiError::Malformed);
            };
            // These are the two explicit qmi.c device-info checks.
            if address == 0 || size != DEVICE_BAR_SIZE {
                return Err(QmiError::Malformed);
            }
            self.memory.map_device_bar(address, size)?;
        }
        Ok(())
    }

    fn download(
        &mut self,
        transport: &mut dyn Transport,
        bytes: &[u8],
        kind: u8,
    ) -> Result<(), QmiError> {
        let mut remaining = bytes;
        let mut segment_id = 0;
        while !remaining.is_empty() {
            let count = remaining.len().min(wire::MAX_DATA_SIZE);
            let end = u8::from(count == remaining.len());
            let request = BdfDownloadRequest {
                valid: 1,
                file_id: Some(self.board_id as i32),
                // qmi.c sends the remaining size, not the original total.
                total_size: Some(
                    u32::try_from(remaining.len()).map_err(|_| QmiError::MessageTooLong)?,
                ),
                segment_id: Some(segment_id),
                data: Some(remaining[..count].to_vec()),
                end: Some(end),
                bdf_type: Some(kind),
            };
            self.exchange(transport, request.encode()?)?;
            remaining = &remaining[count..];
            segment_id += 1;
        }
        Ok(())
    }

    fn load_bdf(&mut self, transport: &mut dyn Transport, send_m3: bool) -> Result<(), QmiError> {
        self.capabilities(transport)?;

        if self.config.supports_regdb {
            // qmi.c deliberately ignores the regdb download return value.
            if let Ok(Some(regdb)) = self.assets.regulatory_data() {
                let _ = self.download(transport, &regdb, BdfType::RegDb as u8);
            }
        }

        let board = self.assets.board_data(self.board_id)?;
        let board_kind = if board.starts_with(b"\x7fELF") {
            BdfType::Elf
        } else {
            BdfType::Bin
        };
        self.download(transport, &board, board_kind as u8)?;

        // ELF boards, like regdb, carry no separate calibration file.
        if board_kind == BdfType::Bin {
            if self.eeprom_caldata {
                let request = BdfDownloadRequest {
                    valid: 1,
                    file_id: Some(self.board_id as i32),
                    total_size: Some(BDF_NAME_SIZE),
                    segment_id: Some(0),
                    data: None,
                    end: Some(1),
                    bdf_type: Some(wire::FileType::Eeprom as u8),
                };
                self.exchange(transport, request.encode()?)?;
            } else if let Some(cal) = self.assets.calibration_data()? {
                self.download(transport, &cal, wire::FileType::CalData as u8)?;
            }
        }

        if send_m3 {
            let region = if self.config.m3_support {
                let Some(m3) = self.assets.m3_firmware()? else {
                    return Err(QmiError::Transport);
                };
                self.memory.load_m3(&m3)?
            } else {
                MemoryRegion {
                    device_address: 0,
                    size: 0,
                }
            };
            self.exchange(
                transport,
                M3InfoRequest {
                    address: region.device_address,
                    size: region.size,
                }
                .encode()?,
            )?;
        }
        Ok(())
    }

    fn next_indication(
        &mut self,
        transport: &mut dyn Transport,
        timeout_ns: u64,
    ) -> Result<Indication, QmiError> {
        let raw = if self.pending.is_empty() {
            let deadline = transport.now_ns().saturating_add(timeout_ns);
            loop {
                let remaining = deadline.saturating_sub(transport.now_ns());
                if remaining == 0 {
                    return Err(QmiError::Timeout);
                }
                match transport.receive(remaining)? {
                    Incoming::Indication(indication) => break indication,
                    Incoming::Response(_) => return Err(QmiError::Malformed),
                    Incoming::ServerArrived => continue,
                    Incoming::ServerExited => return Err(QmiError::Disconnected),
                }
            }
        } else {
            self.pending.remove(0)
        };
        Indication::decode(raw.message_id(), raw.bytes())
    }
}

fn uses_eeprom_caldata(timeout: Option<u32>) -> bool {
    timeout.unwrap_or(0) != 0
}

impl Handshake for Wcn6750Handshake<'_> {
    fn start(&mut self, transport: &mut dyn Transport) -> Result<FirmwareReady, QmiError> {
        self.init_service(transport)?;
        loop {
            match self.process_next_event(transport)? {
                DriverEvent::ServerArrived => break,
                DriverEvent::ServerExited => return Err(QmiError::Transport),
                _ => {}
            }
        }
        let mut cold_boot_deadline: Option<u64> = None;
        loop {
            let timeout = if let Some(deadline) = cold_boot_deadline {
                let remaining = deadline.saturating_sub(transport.now_ns());
                if remaining == 0 {
                    return Err(QmiError::Timeout);
                }
                remaining
            } else {
                self.config.timeout_ns
            };
            match self.process_next_event_with_timeout(transport, timeout)? {
                DriverEvent::FirmwareReady(ready) => return Ok(ready),
                DriverEvent::FirmwareInitDone(ready) => {
                    if self.config.cal_done || !self.config.cold_boot_calibration {
                        return Ok(ready);
                    }
                    self.start_cold_boot_calibration(transport)?;
                    cold_boot_deadline = Some(
                        transport
                            .now_ns()
                            .saturating_add(self.config.cold_boot_timeout_ns),
                    );
                }
                DriverEvent::ServerExited => return Err(QmiError::Transport),
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{wire::QmiString, RawIndication};
    use alloc::{collections::VecDeque, vec};

    struct Assets;
    impl FirmwareAssets for Assets {
        fn board_data(&mut self, _: u32) -> Result<Vec<u8>, QmiError> {
            Ok(vec![1, 2, 3])
        }
        fn calibration_data(&mut self) -> Result<Option<Vec<u8>>, QmiError> {
            Ok(None)
        }
        fn regulatory_data(&mut self) -> Result<Option<Vec<u8>>, QmiError> {
            Ok(None)
        }
        fn m3_firmware(&mut self) -> Result<Option<Vec<u8>>, QmiError> {
            Ok(None)
        }
    }

    #[derive(Default)]
    struct Memory {
        mapped: Option<(u64, u32)>,
    }
    impl MemoryProvider for Memory {
        fn provision(
            &mut self,
            _: &[MemorySegment],
        ) -> Result<Vec<MemorySegmentResponse>, QmiError> {
            Ok(Vec::new())
        }
        fn load_m3(&mut self, _: &[u8]) -> Result<MemoryRegion, QmiError> {
            Err(QmiError::Transport)
        }
        fn map_device_bar(&mut self, address: u64, size: u32) -> Result<(), QmiError> {
            self.mapped = Some((address, size));
            Ok(())
        }
    }

    struct MockTransport {
        incoming: VecDeque<Incoming>,
        sent: Vec<MessageId>,
        service: Option<(u32, u32)>,
        next_transaction: u16,
        received_timeouts: Vec<u64>,
    }
    impl Transport for MockTransport {
        fn start_service(&mut self, version: u32, instance: u32) -> Result<(), QmiError> {
            self.service = Some((version, instance));
            Ok(())
        }
        fn stop_service(&mut self) {
            self.service = None;
        }
        fn send(&mut self, request: Request) -> Result<crate::TransactionId, QmiError> {
            self.sent.push(request.message_id());
            self.next_transaction += 1;
            Ok(crate::TransactionId::new(self.next_transaction))
        }
        fn now_ns(&self) -> u64 {
            1
        }
        fn receive(&mut self, timeout_ns: u64) -> Result<Incoming, QmiError> {
            self.received_timeouts.push(timeout_ns);
            self.incoming.pop_front().ok_or(QmiError::Timeout)
        }
    }

    fn success(transaction: u16, id: MessageId) -> Incoming {
        Incoming::Response(
            Response::checked(
                crate::TransactionId::new(transaction),
                id,
                vec![2, 4, 0, 0, 0, 0, 0],
            )
            .unwrap(),
        )
    }

    #[test]
    fn wcn6750_fixed_memory_handshake_matches_source_order() {
        let cap = vec![
            2, 4, 0, 0, 0, 0, 0, 0x11, 4, 0, 7, 0, 0, 0, 0x13, 9, 0, 0x44, 0x33, 0x22, 0x11, 4,
            b't', b'i', b'm', b'e',
        ];
        let device = vec![
            2, 4, 0, 0, 0, 0, 0, 0x10, 8, 0, 0, 0, 0, 0x10, 0, 0, 0, 0, 0x11, 4, 0, 0, 0, 0x20, 0,
        ];
        let mut transport = MockTransport {
            incoming: VecDeque::from(vec![
                Incoming::ServerArrived,
                success(1, MessageId::IndicationRegister),
                success(2, MessageId::HostCapability),
                Incoming::Response(
                    Response::checked(crate::TransactionId::new(3), MessageId::Capability, cap)
                        .unwrap(),
                ),
                Incoming::Response(
                    Response::checked(crate::TransactionId::new(4), MessageId::DeviceInfo, device)
                        .unwrap(),
                ),
                success(5, MessageId::BdfDownload),
                Incoming::Indication(
                    RawIndication::checked(MessageId::FirmwareInitDone, Vec::new()).unwrap(),
                ),
                success(6, MessageId::WlanMode),
                Incoming::Indication(
                    RawIndication::checked(MessageId::FirmwareReady, Vec::new()).unwrap(),
                ),
            ]),
            sent: Vec::new(),
            service: None,
            next_transaction: 0,
            received_timeouts: Vec::new(),
        };
        let mut assets = Assets;
        let mut memory = Memory::default();
        let mut handshake =
            Wcn6750Handshake::new(HandshakeConfig::default(), &mut assets, &mut memory);
        let ready = handshake.start(&mut transport).unwrap();
        assert_eq!(ready.firmware_version, 0x11223344);
        assert_eq!(transport.received_timeouts.last(), Some(&60_000_000_000));
        assert!(handshake.config.cal_done);
        assert_eq!(transport.service, Some((1, 3)));
        assert_eq!(
            transport.sent,
            [
                MessageId::IndicationRegister,
                MessageId::HostCapability,
                MessageId::Capability,
                MessageId::DeviceInfo,
                MessageId::BdfDownload,
                MessageId::WlanMode,
            ]
        );
        drop(handshake);
        assert_eq!(memory.mapped, Some((0x10000000, DEVICE_BAR_SIZE)));
        let _ = QmiString::new(b"WIN".to_vec(), 16).unwrap();
    }

    #[test]
    fn firmware_start_and_stop_order() {
        let mut transport = MockTransport {
            incoming: VecDeque::from(vec![
                success(1, MessageId::WlanIni),
                success(2, MessageId::WlanConfig),
                success(3, MessageId::WlanMode),
                success(4, MessageId::WlanMode),
            ]),
            sent: Vec::new(),
            service: None,
            next_transaction: 0,
            received_timeouts: Vec::new(),
        };
        let mut assets = Assets;
        let mut memory = Memory::default();
        let mut handshake =
            Wcn6750Handshake::new(HandshakeConfig::default(), &mut assets, &mut memory);
        handshake
            .firmware_start(&mut transport, &WlanConfigRequest::default(), 0, true)
            .unwrap();
        handshake.firmware_stop(&mut transport).unwrap();
        assert_eq!(
            transport.sent,
            [
                MessageId::WlanIni,
                MessageId::WlanConfig,
                MessageId::WlanMode,
                MessageId::WlanMode
            ]
        );
    }

    #[test]
    fn eeprom_caldata_uses_value_not_presence() {
        assert!(!uses_eeprom_caldata(None));
        assert!(!uses_eeprom_caldata(Some(0)));
        assert!(uses_eeprom_caldata(Some(1)));
    }

    #[test]
    fn stale_transaction_response_does_not_complete_exchange() {
        let mut transport = MockTransport {
            incoming: VecDeque::from(vec![
                success(99, MessageId::WlanConfig),
                success(1, MessageId::WlanConfig),
                success(2, MessageId::WlanMode),
            ]),
            sent: Vec::new(),
            service: None,
            next_transaction: 0,
            received_timeouts: Vec::new(),
        };
        let mut assets = Assets;
        let mut memory = Memory::default();
        let mut handshake =
            Wcn6750Handshake::new(HandshakeConfig::default(), &mut assets, &mut memory);
        handshake
            .firmware_start(&mut transport, &WlanConfigRequest::default(), 0, false)
            .unwrap();
    }

    #[test]
    fn firmware_stop_accepts_service_disconnect() {
        let mut transport = MockTransport {
            incoming: VecDeque::from(vec![Incoming::ServerExited]),
            sent: Vec::new(),
            service: None,
            next_transaction: 0,
            received_timeouts: Vec::new(),
        };
        let mut assets = Assets;
        let mut memory = Memory::default();
        let mut handshake =
            Wcn6750Handshake::new(HandshakeConfig::default(), &mut assets, &mut memory);
        assert_eq!(handshake.firmware_stop(&mut transport), Ok(()));
    }
}
