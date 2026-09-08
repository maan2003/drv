// SPDX-License-Identifier: GPL-2.0-only

//! Ath11k client binding for the chip-neutral synchronous SoftMAC contract.

use ath11k_core::{
    ClientRadioControl as _, Device, DeviceState, Lifecycle as _, ManagementFrame, ModelSubsystems,
    RadioControl as _, ScanConfig, ScanId, Subsystems, VdevId, WCN6750, WlanEvent,
};
use fidl_fuchsia_wlan_ieee80211::{ChannelBandwidth, ChannelNumber, WlanBand, WlanPhyType};
use wlan_softmac_host::{
    ClientRuntimeDriver, DiscoverySupport, JoinBssRequest, MacSublayerSupport, SecuritySupport,
    SpectrumManagementSupport, WlanAssociationConfig, WlanKeyConfiguration, WlanRxInfo,
    WlanSoftmac, WlanSoftmacBaseCancelScanRequest, WlanSoftmacBaseClearAssociationRequest,
    WlanSoftmacBaseSetChannelRequest, WlanSoftmacBaseStartActiveScanResponse,
    WlanSoftmacBaseStartPassiveScanRequest, WlanSoftmacBaseStartPassiveScanResponse,
    WlanSoftmacBaseUpdateWmmParametersRequest, WlanSoftmacLifecycle, WlanSoftmacQueryResponse,
    WlanSoftmacStartActiveScanRequest, WlanSoftmacUpcalls, WlanTxInfoFlags, WlanTxResult,
};

const SCAN_EVENT_COMPLETED: u32 = 1 << 1;
const DP_WORK_BUDGET: usize = 64;
const DP_RECEIVE_BUDGET: usize = 1;
const MGMT_TX_PENDING_MAX: u32 = 512;

fn status(error: ath11k_core::CoreError) -> zx::Status {
    match error {
        ath11k_core::CoreError::WrongState => zx::Status::BAD_STATE,
        ath11k_core::CoreError::NoResources => zx::Status::NO_RESOURCES,
        ath11k_core::CoreError::NotFound => zx::Status::NOT_FOUND,
        ath11k_core::CoreError::Protocol => zx::Status::IO_INVALID,
        ath11k_core::CoreError::DeviceFault => zx::Status::IO,
    }
}

fn channel_frequency(channel: ChannelNumber) -> Result<u16, zx::Status> {
    match channel.band {
        WlanBand::TwoGhz if channel.number == 14 => Ok(2484),
        WlanBand::TwoGhz if (1..=13).contains(&channel.number) => {
            Ok(2407 + 5 * u16::from(channel.number))
        }
        WlanBand::FiveGhz if channel.number != 0 => Ok(5000 + 5 * u16::from(channel.number)),
        _ => Err(zx::Status::INVALID_ARGS),
    }
}

fn frequency_channel(frequency: u16) -> ChannelNumber {
    if frequency == 2484 {
        ChannelNumber {
            band: WlanBand::TwoGhz,
            number: 14,
        }
    } else if frequency < 3000 {
        ChannelNumber {
            band: WlanBand::TwoGhz,
            number: frequency.saturating_sub(2407).saturating_div(5) as u8,
        }
    } else {
        ChannelNumber {
            band: WlanBand::FiveGhz,
            number: frequency.saturating_sub(5000).saturating_div(5) as u8,
        }
    }
}

/// Owns an ath11k client device and its installed host callbacks.
///
/// The same type composes the deterministic [`ModelSubsystems`] and the real
/// `Wcn6750Subsystems`; chip-private resources stay below `Device<B>`.
pub struct Ath11kClientDevice<B: Subsystems> {
    device: Device<B>,
    mac: [u8; 6],
    vdev: Option<VdevId>,
    peer: Option<[u8; 6]>,
    upcalls: Option<Box<dyn WlanSoftmacUpcalls>>,
    next_scan_id: u32,
    active_scan: Option<u32>,
    next_mgmt_buffer_id: u32,
    pending_mgmt_tx: Vec<(u32, [u8; 6])>,
    deferred_mgmt_rx: Option<DeferredManagementRx>,
    deterministic_scan_completion: bool,
    regulatory_channels: Vec<ath11k_core::RegulatoryChannel>,
}

impl<B: Subsystems> Ath11kClientDevice<B> {
    pub fn new(device: Device<B>, mac: [u8; 6]) -> Self {
        Self {
            device,
            mac,
            vdev: None,
            peer: None,
            upcalls: None,
            next_scan_id: 1,
            active_scan: None,
            next_mgmt_buffer_id: 0,
            pending_mgmt_tx: Vec::new(),
            deferred_mgmt_rx: None,
            deterministic_scan_completion: false,
            regulatory_channels: Vec::new(),
        }
    }

    /// Install channel facts projected from the platform regulatory table.
    pub fn with_regulatory_channels(
        mut self,
        channels: Vec<ath11k_core::RegulatoryChannel>,
    ) -> Self {
        self.regulatory_channels = channels;
        self
    }

    pub fn into_device(self) -> Device<B> {
        self.device
    }

    fn ready_vdev(&self) -> Result<VdevId, zx::Status> {
        if self.upcalls.is_none() {
            return Err(zx::Status::BAD_STATE);
        }
        self.vdev.ok_or(zx::Status::BAD_STATE)
    }

    fn deliver_management_rx(&mut self, received: DeferredManagementRx) {
        let primary = frequency_channel(received.channel_mhz as u16);
        self.upcalls.as_mut().unwrap().recv(
            received.frame,
            WlanRxInfo {
                rx_flags: fidl_fuchsia_wlan_softmac::WlanRxInfoFlags::empty(),
                valid_fields: fidl_fuchsia_wlan_softmac::WlanRxInfoValid::RSSI,
                phy: WlanPhyType::Ofdm,
                data_rate: 0,
                primary,
                bandwidth: ChannelBandwidth::Cbw20,
                vht_secondary_80_channel: ChannelNumber {
                    band: primary.band,
                    number: 0,
                },
                mcs: 0,
                rssi_dbm: received.rssi.clamp(i32::from(i8::MIN), i32::from(i8::MAX)) as i8,
                snr_dbh: 0,
            },
        );
    }
}

impl Ath11kClientDevice<ModelSubsystems> {
    /// Deterministic backend used by the shared host conformance runner.
    pub fn deterministic(mac: [u8; 6]) -> Self {
        let mut device = Self::new(WCN6750.device(ModelSubsystems::default()), mac);
        device.deterministic_scan_completion = true;
        device
            .regulatory_channels
            .push(ath11k_core::RegulatoryChannel {
                frequency_mhz: 2437,
                max_power_dbm: 0,
                max_reg_power_dbm: 0,
                max_antenna_gain_dbi: 0,
                passive: false,
                radar: false,
                allow_ht: true,
                allow_vht: true,
                allow_he: true,
            });
        device
    }
}

impl<B: Subsystems> WlanSoftmacLifecycle for Ath11kClientDevice<B> {
    fn start(&mut self, upcalls: Box<dyn WlanSoftmacUpcalls>) -> Result<(), zx::Status> {
        if self.upcalls.is_some() || self.device.state() != DeviceState::Allocated {
            return Err(zx::Status::BAD_STATE);
        }
        if let Err(error) = self.device.probe() {
            self.device.abort_startup();
            return Err(status(error));
        }
        if let Err(error) = self.device.attach_firmware() {
            self.device.abort_startup();
            return Err(status(error));
        }
        if let Err(error) = self.device.start_radio() {
            self.device.abort_startup();
            return Err(status(error));
        }
        match self.device.create_client_vdev(self.mac) {
            Ok(vdev) => self.vdev = Some(vdev),
            Err(error) => {
                self.device.abort_startup();
                return Err(status(error));
            }
        }
        self.upcalls = Some(upcalls);
        Ok(())
    }

    fn stop(&mut self) -> Result<(), zx::Status> {
        self.upcalls = None;
        self.active_scan = None;
        self.pending_mgmt_tx.clear();
        self.deferred_mgmt_rx = None;
        self.peer = None;
        self.vdev = None;
        self.device.stop().map_err(status)
    }
}

#[derive(Default)]
struct DpDeliveries {
    rx: Vec<ath11k_dp::tx::HostRxFrame>,
    tx: Vec<ath11k_dp::tx::TxResult>,
}

struct DeferredManagementRx {
    channel_mhz: u32,
    rssi: i32,
    frame: Vec<u8>,
}

impl ath11k_dp::tx::DpHost for DpDeliveries {
    fn receive(&mut self, frame: ath11k_dp::tx::HostRxFrame) {
        self.rx.push(frame);
    }

    fn tx_complete(&mut self, result: ath11k_dp::tx::TxResult) {
        self.tx.push(result);
    }
}

impl<B: Subsystems> ClientRuntimeDriver for Ath11kClientDevice<B> {
    fn drive(&mut self) -> Result<bool, zx::Status> {
        self.ready_vdev()?;
        if self.deterministic_scan_completion
            && let Some(scan_id) = self.active_scan.take()
        {
            self.upcalls
                .as_mut()
                .unwrap()
                .notify_scan_complete(zx::Status::OK, u64::from(scan_id));
            return Ok(true);
        }

        let deferred_mgmt_rx = self.deferred_mgmt_rx.take();
        let mut rx_slot_consumed = deferred_mgmt_rx.is_some();
        if let Some(received) = deferred_mgmt_rx {
            self.deliver_management_rx(received);
        }

        let mut deliveries = DpDeliveries::default();
        let serviced = self
            .device
            .service_dp_host(
                DP_WORK_BUDGET,
                if rx_slot_consumed {
                    0
                } else {
                    DP_RECEIVE_BUDGET
                },
                &mut deliveries,
            )
            .map_err(status)?;
        let mut progressed = rx_slot_consumed
            || serviced.tx_delivered != 0
            || serviced.tx_malformed != 0
            || serviced.rx_delivered != 0
            || serviced.rx_dropped != Default::default();
        rx_slot_consumed |= !deliveries.rx.is_empty();
        for frame in deliveries.rx {
            let frequency = frame.info.phy_metadata as u16;
            self.upcalls.as_mut().unwrap().recv(
                frame.bytes,
                WlanRxInfo {
                    rx_flags: fidl_fuchsia_wlan_softmac::WlanRxInfoFlags::empty(),
                    valid_fields: fidl_fuchsia_wlan_softmac::WlanRxInfoValid::MCS,
                    phy: WlanPhyType::Ofdm,
                    data_rate: 0,
                    primary: frequency_channel(frequency),
                    bandwidth: ChannelBandwidth::Cbw20,
                    vht_secondary_80_channel: ChannelNumber {
                        band: frequency_channel(frequency).band,
                        number: 0,
                    },
                    mcs: frame.info.mcs,
                    rssi_dbm: 0,
                    snr_dbh: 0,
                },
            );
        }
        for completion in deliveries.tx {
            let peer_addr = self.peer.ok_or(zx::Status::BAD_STATE)?;
            let mut entries = [fidl_fuchsia_wlan_softmac::WlanTxResultEntry {
                tx_vector_idx: 0,
                attempts: 0,
            };
                fidl_fuchsia_wlan_softmac::WLAN_TX_RESULT_MAX_ENTRY as usize];
            entries[0].attempts = 1;
            self.upcalls
                .as_mut()
                .unwrap()
                .report_tx_result(WlanTxResult {
                    tx_result_entry: entries,
                    peer_addr,
                    result_code: if completion.acknowledged {
                        fidl_fuchsia_wlan_softmac::WlanTxResultCode::Success
                    } else {
                        fidl_fuchsia_wlan_softmac::WlanTxResultCode::Failed
                    },
                });
        }

        let (event, control_progressed) = self
            .device
            .poll_wlan_event(DP_WORK_BUDGET)
            .map_err(status)?;
        progressed |= control_progressed;
        if let Some(event) = event {
            match event {
                WlanEvent::ManagementReceived {
                    channel_mhz,
                    rssi,
                    frame,
                    ..
                } => {
                    let received = DeferredManagementRx {
                        channel_mhz,
                        rssi,
                        frame,
                    };
                    if rx_slot_consumed {
                        self.deferred_mgmt_rx = Some(received);
                    } else {
                        self.deliver_management_rx(received);
                    }
                }
                WlanEvent::ManagementTxCompleted {
                    buffer_id, status, ..
                } => {
                    let Some(index) = self
                        .pending_mgmt_tx
                        .iter()
                        .position(|(pending, _)| *pending == buffer_id)
                    else {
                        return Ok(true);
                    };
                    let (_, peer_addr) = self.pending_mgmt_tx.remove(index);
                    // WMI completion carries no rate/retry history. Keep every
                    // entry at zero rather than inventing a TX vector.
                    let entries = [fidl_fuchsia_wlan_softmac::WlanTxResultEntry {
                        tx_vector_idx: 0,
                        attempts: 0,
                    };
                        fidl_fuchsia_wlan_softmac::WLAN_TX_RESULT_MAX_ENTRY as usize];
                    self.upcalls
                        .as_mut()
                        .unwrap()
                        .report_tx_result(WlanTxResult {
                            tx_result_entry: entries,
                            peer_addr,
                            result_code: if status == 0 && peer_addr[0] & 1 == 0 {
                                fidl_fuchsia_wlan_softmac::WlanTxResultCode::Success
                            } else {
                                fidl_fuchsia_wlan_softmac::WlanTxResultCode::Failed
                            },
                        });
                }
                WlanEvent::Scan {
                    event_type,
                    reason,
                    scan_id,
                    ..
                } if self.active_scan == Some(scan_id)
                    && event_type & SCAN_EVENT_COMPLETED != 0 =>
                {
                    self.active_scan = None;
                    self.upcalls.as_mut().unwrap().notify_scan_complete(
                        if reason == 0 {
                            zx::Status::OK
                        } else {
                            zx::Status::IO
                        },
                        u64::from(scan_id),
                    );
                }
                _ => {}
            }
        }
        Ok(progressed)
    }

    fn set_link_up(&mut self, _up: bool) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }

    fn reset(&mut self) -> Result<(), zx::Status> {
        self.stop()
    }
}

impl<B: Subsystems> WlanSoftmac for Ath11kClientDevice<B> {
    fn query(&mut self) -> Result<WlanSoftmacQueryResponse, zx::Status> {
        Ok(WlanSoftmacQueryResponse {
            sta_addr: Some(self.mac),
            ..Default::default()
        })
    }
    fn query_discovery_support(&mut self) -> Result<DiscoverySupport, zx::Status> {
        Ok(Default::default())
    }
    fn query_mac_sublayer_support(&mut self) -> Result<MacSublayerSupport, zx::Status> {
        Ok(Default::default())
    }
    fn query_security_support(&mut self) -> Result<SecuritySupport, zx::Status> {
        Ok(Default::default())
    }
    fn query_spectrum_management_support(
        &mut self,
    ) -> Result<SpectrumManagementSupport, zx::Status> {
        Ok(Default::default())
    }

    fn set_channel(&mut self, request: WlanSoftmacBaseSetChannelRequest) -> Result<(), zx::Status> {
        let primary = request.primary.ok_or(zx::Status::INVALID_ARGS)?;
        if request.bandwidth != Some(ChannelBandwidth::Cbw20)
            || request.vht_secondary_80_channel.is_none()
        {
            return Err(zx::Status::INVALID_ARGS);
        }
        let vdev = self.ready_vdev()?;
        let frequency = channel_frequency(primary)?;
        let channel = self
            .regulatory_channels
            .iter()
            .find(|channel| channel.frequency_mhz == frequency)
            .copied()
            .ok_or(zx::Status::NOT_FOUND)?;
        self.device.start_vdev(vdev, channel).map_err(status)
    }

    fn join_bss(&mut self, request: JoinBssRequest) -> Result<(), zx::Status> {
        let _ = request;
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn install_key(&mut self, _configuration: WlanKeyConfiguration) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn notify_association_complete(
        &mut self,
        _configuration: WlanAssociationConfig,
    ) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn clear_association(
        &mut self,
        _request: WlanSoftmacBaseClearAssociationRequest,
    ) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }

    fn start_passive_scan(
        &mut self,
        request: WlanSoftmacBaseStartPassiveScanRequest,
    ) -> Result<WlanSoftmacBaseStartPassiveScanResponse, zx::Status> {
        if self.active_scan.is_some() {
            return Err(zx::Status::BAD_STATE);
        }
        let channels = request.channels.ok_or(zx::Status::INVALID_ARGS)?;
        if channels.is_empty() {
            return Err(zx::Status::INVALID_ARGS);
        }
        let channels_mhz = channels
            .into_iter()
            .map(channel_frequency)
            .collect::<Result<Vec<_>, _>>()?;
        let scan_id = self.next_scan_id;
        self.next_scan_id = self
            .next_scan_id
            .checked_add(1)
            .ok_or(zx::Status::NO_RESOURCES)?;
        self.device
            .start_scan(ScanConfig {
                vdev: self.ready_vdev()?,
                id: ScanId(scan_id),
                active: false,
                channels_mhz,
                ssids: Vec::new(),
            })
            .map_err(status)?;
        self.active_scan = Some(scan_id);
        Ok(WlanSoftmacBaseStartPassiveScanResponse {
            scan_id: Some(u64::from(scan_id)),
        })
    }
    fn start_active_scan(
        &mut self,
        _request: WlanSoftmacStartActiveScanRequest,
    ) -> Result<WlanSoftmacBaseStartActiveScanResponse, zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn cancel_scan(&mut self, request: WlanSoftmacBaseCancelScanRequest) -> Result<(), zx::Status> {
        let scan_id = u32::try_from(request.scan_id.ok_or(zx::Status::INVALID_ARGS)?)
            .map_err(|_| zx::Status::INVALID_ARGS)?;
        if self.active_scan != Some(scan_id) {
            return Err(zx::Status::NOT_FOUND);
        }
        self.device
            .stop_scan(self.ready_vdev()?, ScanId(scan_id))
            .map_err(status)?;
        self.active_scan = None;
        Ok(())
    }
    fn update_wmm_parameters(
        &mut self,
        _request: WlanSoftmacBaseUpdateWmmParametersRequest,
    ) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn queue_tx(&mut self, bytes: &[u8], flags: WlanTxInfoFlags) -> Result<(), zx::Status> {
        if bytes.len() < 24 {
            return Err(zx::Status::INVALID_ARGS);
        }
        let frame_control = u16::from_le_bytes([bytes[0], bytes[1]]);
        if frame_control & 0x0003 != 0 {
            return Err(zx::Status::INVALID_ARGS);
        }
        if frame_control & 0x000c != 0 {
            return Err(zx::Status::NOT_SUPPORTED);
        }
        // Protected robust-management frames need the pinned C cipher/MIC
        // expansion path; do not silently send them as plaintext.
        if flags.contains(WlanTxInfoFlags::PROTECTED) || frame_control & 0x4000 != 0 {
            return Err(zx::Status::NOT_SUPPORTED);
        }
        if self.pending_mgmt_tx.len() >= MGMT_TX_PENDING_MAX as usize {
            return Err(zx::Status::NO_RESOURCES);
        }
        let buffer_id = (0..MGMT_TX_PENDING_MAX)
            .map(|offset| (self.next_mgmt_buffer_id + offset) % MGMT_TX_PENDING_MAX)
            .find(|candidate| {
                !self
                    .pending_mgmt_tx
                    .iter()
                    .any(|(pending, _)| pending == candidate)
            })
            .ok_or(zx::Status::NO_RESOURCES)?;
        let peer_addr = bytes[4..10].try_into().unwrap();
        self.device
            .transmit_management(ManagementFrame {
                vdev: self.ready_vdev()?,
                buffer_id,
                bytes: bytes.to_vec(),
            })
            .map_err(status)?;
        self.pending_mgmt_tx.push((buffer_id, peer_addr));
        self.next_mgmt_buffer_id = (buffer_id + 1) % MGMT_TX_PENDING_MAX;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ath11k_core::Operation;
    use std::sync::{Arc, Mutex};
    use wlan_softmac_host::conformance::{expected_client_conformance, run_client_conformance};

    const CLIENT: [u8; 6] = [2, 0, 0, 0, 0, 1];

    struct NoopUpcalls;
    impl WlanSoftmacUpcalls for NoopUpcalls {
        fn recv(&mut self, _: Vec<u8>, _: WlanRxInfo) {}
        fn report_tx_result(&mut self, _: WlanTxResult) {}
        fn notify_scan_complete(&mut self, _: zx::Status, _: u64) {}
    }

    #[derive(Default)]
    struct RecordedUpcalls {
        received: Vec<Vec<u8>>,
        tx: Vec<([u8; 6], fidl_fuchsia_wlan_softmac::WlanTxResultCode)>,
    }

    struct Recorder(Arc<Mutex<RecordedUpcalls>>);
    impl WlanSoftmacUpcalls for Recorder {
        fn recv(&mut self, bytes: Vec<u8>, _: WlanRxInfo) {
            self.0.lock().unwrap().received.push(bytes);
        }
        fn report_tx_result(&mut self, result: WlanTxResult) {
            self.0
                .lock()
                .unwrap()
                .tx
                .push((result.peer_addr, result.result_code));
        }
        fn notify_scan_complete(&mut self, _: zx::Status, _: u64) {}
    }

    struct SimultaneousRxSubsystems {
        model: ModelSubsystems,
        dp_rx: Option<ath11k_dp::tx::HostRxFrame>,
        receive_budgets: Arc<Mutex<Vec<usize>>>,
    }

    impl Subsystems for SimultaneousRxSubsystems {
        fn execute(&mut self, operation: Operation) -> Result<(), ath11k_core::CoreError> {
            self.model.execute(operation)
        }

        fn wait_for_firmware_ready(
            &mut self,
        ) -> Result<ath11k_qmi::FirmwareReady, ath11k_core::CoreError> {
            self.model.wait_for_firmware_ready()
        }

        fn next_wlan_event(&mut self) -> Result<Option<WlanEvent>, ath11k_core::CoreError> {
            self.model.next_wlan_event()
        }

        fn client_nss(&self) -> Result<u8, ath11k_core::CoreError> {
            self.model.client_nss()
        }

        fn service_dp_host<H: ath11k_dp::tx::DpHost>(
            &mut self,
            _work_budget: usize,
            receive_budget: usize,
            host: &mut H,
        ) -> Result<ath11k_dp::tx::HostServiceResult, ath11k_core::CoreError> {
            self.receive_budgets.lock().unwrap().push(receive_budget);
            let delivered = if receive_budget != 0 {
                self.dp_rx.take().map(|frame| host.receive(frame)).is_some()
            } else {
                false
            };
            Ok(ath11k_dp::tx::HostServiceResult {
                tx_delivered: 0,
                tx_malformed: 0,
                rx_delivered: usize::from(delivered),
                rx_dropped: Default::default(),
            })
        }
    }

    fn failed_start(operation: Operation) -> Device<ModelSubsystems> {
        let mut backend = ModelSubsystems::default();
        backend.fail_once(operation);
        let mut adapter = Ath11kClientDevice::new(WCN6750.device(backend), CLIENT);
        assert!(adapter.start(Box::new(NoopUpcalls)).is_err());
        adapter.into_device()
    }

    #[test]
    fn deterministic_ath11k_passes_the_generic_client_contract() {
        let channel = ChannelNumber {
            band: WlanBand::TwoGhz,
            number: 6,
        };
        assert_eq!(
            run_client_conformance(Ath11kClientDevice::deterministic(CLIENT), channel).unwrap(),
            expected_client_conformance(CLIENT, 1)
        );
    }

    #[test]
    fn management_tx_and_rx_cross_the_host_seam_with_buffer_correlation() {
        let records = Arc::new(Mutex::new(RecordedUpcalls::default()));
        let mut adapter = Ath11kClientDevice::deterministic(CLIENT);
        adapter.start(Box::new(Recorder(records.clone()))).unwrap();
        adapter
            .set_channel(WlanSoftmacBaseSetChannelRequest {
                primary: Some(ChannelNumber {
                    band: WlanBand::TwoGhz,
                    number: 6,
                }),
                bandwidth: Some(ChannelBandwidth::Cbw20),
                vht_secondary_80_channel: Some(ChannelNumber {
                    band: WlanBand::TwoGhz,
                    number: 0,
                }),
            })
            .unwrap();
        let mut frame = vec![0; 24];
        frame[0] = 0xb0;
        frame[4..10].copy_from_slice(&[2, 0, 0, 0, 0, 2]);
        adapter.queue_tx(&frame, WlanTxInfoFlags::empty()).unwrap();
        let vdev = adapter.vdev.unwrap();
        assert!(
            adapter
                .device
                .backend()
                .operations()
                .contains(&Operation::WmiMgmtTx(ManagementFrame {
                    vdev,
                    buffer_id: 0,
                    bytes: frame.clone(),
                }))
        );
        adapter
            .device
            .backend_mut()
            .push_event(WlanEvent::ManagementTxCompleted {
                buffer_id: 0,
                status: 0,
                ack_rssi: 42,
            });
        adapter
            .device
            .backend_mut()
            .push_event(WlanEvent::ManagementReceived {
                pdev_id: 0,
                channel_mhz: 2437,
                snr: 0,
                rssi: -48,
                flags: 0,
                frame: frame.clone(),
            });
        assert!(adapter.drive().unwrap());
        assert!(adapter.drive().unwrap());
        let mut group = frame.clone();
        group[4..10].copy_from_slice(&[0xff; 6]);
        adapter.queue_tx(&group, WlanTxInfoFlags::empty()).unwrap();
        adapter
            .device
            .backend_mut()
            .push_event(WlanEvent::ManagementTxCompleted {
                buffer_id: 1,
                status: 0,
                ack_rssi: 0,
            });
        assert!(adapter.drive().unwrap());
        let records = records.lock().unwrap();
        assert_eq!(records.received, [frame]);
        assert_eq!(
            records.tx,
            [
                (
                    [2, 0, 0, 0, 0, 2],
                    fidl_fuchsia_wlan_softmac::WlanTxResultCode::Success
                ),
                (
                    [0xff; 6],
                    fidl_fuchsia_wlan_softmac::WlanTxResultCode::Failed
                )
            ]
        );
    }

    #[test]
    fn simultaneous_dp_and_management_rx_share_one_receive_slot() {
        let records = Arc::new(Mutex::new(RecordedUpcalls::default()));
        let receive_budgets = Arc::new(Mutex::new(Vec::new()));
        let dp_frame = vec![0x08, 0x00, 1];
        let mgmt_frame = vec![0x80, 0x00, 2];
        let mut model = ModelSubsystems::default();
        model.push_event(WlanEvent::ManagementReceived {
            pdev_id: 0,
            channel_mhz: 2437,
            snr: 0,
            rssi: -48,
            flags: 0,
            frame: mgmt_frame.clone(),
        });
        let backend = SimultaneousRxSubsystems {
            model,
            dp_rx: Some(ath11k_dp::tx::HostRxFrame {
                bytes: dp_frame.clone(),
                info: ath11k_dp::tx::HostRxInfo {
                    decap_type: ath11k_dp::tx::RxDecapType::Raw,
                    peer: None,
                    tid: 0,
                    decrypt_status: ath11k_dp::tx::RxDecryptStatus::NotDecrypted,
                    phy_metadata: 2437,
                    bandwidth: 0,
                    mcs: 0,
                    packet_type: 0,
                    nss: 1,
                    phy_ppdu_id: 0,
                },
            }),
            receive_budgets: receive_budgets.clone(),
        };
        let mut adapter = Ath11kClientDevice::new(WCN6750.device(backend), CLIENT);
        adapter.start(Box::new(Recorder(records.clone()))).unwrap();

        assert!(adapter.drive().unwrap());
        assert_eq!(
            records.lock().unwrap().received.as_slice(),
            std::slice::from_ref(&dp_frame)
        );
        assert!(adapter.drive().unwrap());
        assert_eq!(records.lock().unwrap().received, [dp_frame, mgmt_frame]);
        assert_eq!(*receive_budgets.lock().unwrap(), [1, 0]);
    }

    #[test]
    fn management_tx_rejects_unsupported_frame_control() {
        let mut adapter = Ath11kClientDevice::deterministic(CLIENT);
        adapter.start(Box::new(NoopUpcalls)).unwrap();
        let mut frame = vec![0; 24];
        frame[0] = 0xb0;

        assert_eq!(
            adapter.queue_tx(&frame, WlanTxInfoFlags::PROTECTED),
            Err(zx::Status::NOT_SUPPORTED)
        );
        frame[1] |= 0x40;
        assert_eq!(
            adapter.queue_tx(&frame, WlanTxInfoFlags::empty()),
            Err(zx::Status::NOT_SUPPORTED)
        );
        frame[1] &= !0x40;
        frame[0] |= 1;
        assert_eq!(
            adapter.queue_tx(&frame, WlanTxInfoFlags::empty()),
            Err(zx::Status::INVALID_ARGS)
        );
    }

    #[test]
    fn failed_firmware_attach_unwinds_probed_device() {
        let device = failed_start(Operation::QmiWaitFirmwareReady);
        assert_eq!(device.state(), DeviceState::Stopped);
        assert!(device.backend().operations().ends_with(&[
            Operation::HifPowerDown,
            Operation::RegFree,
            Operation::QmiDeinitService,
        ]));
    }

    #[test]
    fn failed_hif_power_up_unwinds_allocated_device() {
        let device = failed_start(Operation::HifPowerUp);
        assert_eq!(device.state(), DeviceState::Stopped);
        assert!(
            device
                .backend()
                .operations()
                .contains(&Operation::WmiDetach)
        );
    }

    #[test]
    fn failed_dp_allocation_releases_partial_transport() {
        let device = failed_start(Operation::DpAllocate);
        assert_eq!(device.state(), DeviceState::Stopped);
        assert!(
            device
                .backend()
                .operations()
                .contains(&Operation::WmiDetach)
        );
    }

    #[test]
    fn failed_radio_start_unwinds_ready_device() {
        let device = failed_start(Operation::RadioStart);
        assert_eq!(device.state(), DeviceState::Stopped);
        assert!(
            device
                .backend()
                .operations()
                .contains(&Operation::QmiFirmwareStop)
        );
        assert!(device.backend().operations().ends_with(&[
            Operation::DpFree,
            Operation::RegFree,
            Operation::QmiDeinitService,
        ]));
    }
}
