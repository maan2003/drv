#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::vec::Vec;
use ath11k_hal::{
    Descriptor, RingFlags, RingId, RingMemory, RingType, Srng, SrngParams,
    descriptors::{
        CeDestinationDescriptor as HalCeDestinationDescriptor, CeDestinationStatusDescriptor,
        CeSourceDescriptor as HalCeSourceDescriptor,
    },
};
use ath11k_platform_backend::{
    Backend, Bidirectional, CoherentDma, Device, FromDevice, MmioRegion, StreamingDma, ToDevice,
};

pub const CE_COUNT: usize = 9;
pub const HTC_ENDPOINT_COUNT: usize = 9;
pub const HTC_HEADER_LEN: usize = 8;
pub const HTC_MAX_LEN: usize = 4096;
pub const HTC_MAX_CTRL_MSG_LEN: usize = 256;
pub const CE_ATTR_BYTE_SWAP_DATA: u32 = 2;
pub const CE_ATTR_DISABLE_INTR: u32 = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum PipeDirection {
    None = 0,
    In = 1,
    Out = 2,
    InOut = 3,
    InOutHostToHost = 4,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompletionHandler {
    None,
    Htc,
    Htt,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostPipeConfig {
    pub flags: u32,
    pub source_entries: u16,
    pub source_size_max: u16,
    pub destination_entries: u16,
    pub send_completion: CompletionHandler,
    pub receive_completion: CompletionHandler,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TargetPipeConfig {
    pub pipe: u8,
    pub direction: PipeDirection,
    pub entries: u16,
    pub bytes_max: u16,
    pub flags: u32,
    pub reserved: u32,
}

impl TargetPipeConfig {
    pub const fn to_le_bytes(self) -> [u8; 24] {
        let words = [
            self.pipe as u32,
            self.direction as u32,
            self.entries as u32,
            self.bytes_max as u32,
            self.flags,
            self.reserved,
        ];
        let mut out = [0; 24];
        let mut i = 0;
        while i < words.len() {
            let bytes = words[i].to_le_bytes();
            out[i * 4] = bytes[0];
            out[i * 4 + 1] = bytes[1];
            out[i * 4 + 2] = bytes[2];
            out[i * 4 + 3] = bytes[3];
            i += 1;
        }
        out
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ServicePipeMap {
    pub service: ServiceId,
    pub direction: PipeDirection,
    pub pipe: u8,
}

impl ServicePipeMap {
    pub const fn to_le_bytes(self) -> [u8; 12] {
        let service = (self.service.0 as u32).to_le_bytes();
        let direction = (self.direction as u32).to_le_bytes();
        let pipe = (self.pipe as u32).to_le_bytes();
        [
            service[0],
            service[1],
            service[2],
            service[3],
            direction[0],
            direction[1],
            direction[2],
            direction[3],
            pipe[0],
            pipe[1],
            pipe[2],
            pipe[3],
        ]
    }
}

pub const WCN6750_HOST_CE_CONFIG: [HostPipeConfig; CE_COUNT] = [
    HostPipeConfig {
        flags: 0,
        source_entries: 16,
        source_size_max: 2048,
        destination_entries: 0,
        send_completion: CompletionHandler::None,
        receive_completion: CompletionHandler::None,
    },
    HostPipeConfig {
        flags: 0,
        source_entries: 0,
        source_size_max: 2048,
        destination_entries: 512,
        send_completion: CompletionHandler::None,
        receive_completion: CompletionHandler::Htc,
    },
    HostPipeConfig {
        flags: 0,
        source_entries: 0,
        source_size_max: 2048,
        destination_entries: 512,
        send_completion: CompletionHandler::None,
        receive_completion: CompletionHandler::Htc,
    },
    HostPipeConfig {
        flags: 0,
        source_entries: 32,
        source_size_max: 2048,
        destination_entries: 0,
        send_completion: CompletionHandler::Htc,
        receive_completion: CompletionHandler::None,
    },
    HostPipeConfig {
        flags: CE_ATTR_DISABLE_INTR,
        source_entries: 2048,
        source_size_max: 256,
        destination_entries: 0,
        send_completion: CompletionHandler::None,
        receive_completion: CompletionHandler::None,
    },
    HostPipeConfig {
        flags: 0,
        source_entries: 0,
        source_size_max: 2048,
        destination_entries: 512,
        send_completion: CompletionHandler::None,
        receive_completion: CompletionHandler::Htt,
    },
    HostPipeConfig {
        flags: CE_ATTR_DISABLE_INTR,
        source_entries: 0,
        source_size_max: 0,
        destination_entries: 0,
        send_completion: CompletionHandler::None,
        receive_completion: CompletionHandler::None,
    },
    HostPipeConfig {
        flags: 0,
        source_entries: 32,
        source_size_max: 2048,
        destination_entries: 0,
        send_completion: CompletionHandler::Htc,
        receive_completion: CompletionHandler::None,
    },
    HostPipeConfig {
        flags: 0,
        source_entries: 0,
        source_size_max: 0,
        destination_entries: 0,
        send_completion: CompletionHandler::None,
        receive_completion: CompletionHandler::None,
    },
];

pub const WCN6750_TARGET_CE_CONFIG: [TargetPipeConfig; CE_COUNT] = [
    TargetPipeConfig {
        pipe: 0,
        direction: PipeDirection::Out,
        entries: 32,
        bytes_max: 2048,
        flags: 0,
        reserved: 0,
    },
    TargetPipeConfig {
        pipe: 1,
        direction: PipeDirection::In,
        entries: 32,
        bytes_max: 2048,
        flags: 0,
        reserved: 0,
    },
    TargetPipeConfig {
        pipe: 2,
        direction: PipeDirection::In,
        entries: 32,
        bytes_max: 2048,
        flags: 0,
        reserved: 0,
    },
    TargetPipeConfig {
        pipe: 3,
        direction: PipeDirection::Out,
        entries: 32,
        bytes_max: 2048,
        flags: 0,
        reserved: 0,
    },
    TargetPipeConfig {
        pipe: 4,
        direction: PipeDirection::Out,
        entries: 256,
        bytes_max: 256,
        flags: CE_ATTR_DISABLE_INTR,
        reserved: 0,
    },
    TargetPipeConfig {
        pipe: 5,
        direction: PipeDirection::In,
        entries: 32,
        bytes_max: 2048,
        flags: 0,
        reserved: 0,
    },
    TargetPipeConfig {
        pipe: 6,
        direction: PipeDirection::InOut,
        entries: 32,
        bytes_max: 16384,
        flags: 0,
        reserved: 0,
    },
    TargetPipeConfig {
        pipe: 7,
        direction: PipeDirection::InOutHostToHost,
        entries: 0,
        bytes_max: 0,
        flags: CE_ATTR_DISABLE_INTR,
        reserved: 0,
    },
    TargetPipeConfig {
        pipe: 8,
        direction: PipeDirection::InOut,
        entries: 32,
        bytes_max: 16384,
        flags: 0,
        reserved: 0,
    },
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ServiceId(pub u16);

impl ServiceId {
    pub const RESERVED: Self = Self(0x0000);
    pub const RESERVED_CONTROL: Self = Self(0x0001);
    pub const WMI_CONTROL: Self = Self(0x0100);
    pub const WMI_DATA_BE: Self = Self(0x0101);
    pub const WMI_DATA_BK: Self = Self(0x0102);
    pub const WMI_DATA_VI: Self = Self(0x0103);
    pub const WMI_DATA_VO: Self = Self(0x0104);
    pub const WMI_CONTROL_MAC1: Self = Self(0x0105);
    pub const WMI_CONTROL_MAC2: Self = Self(0x0106);
    pub const NMI_CONTROL: Self = Self(0x0200);
    pub const NMI_DATA: Self = Self(0x0201);
    pub const HTT_DATA_MSG: Self = Self(0x0300);
    pub const IPA_TX: Self = Self(0x0500);
    pub const PKT_LOG: Self = Self(0x0600);
    pub const TEST_RAW_STREAMS: Self = Self(0xfe00);
}

pub const WCN6750_SERVICE_TO_PIPE: [ServicePipeMap; 14] = [
    service_pipe(ServiceId::WMI_DATA_VO, PipeDirection::Out, 3),
    service_pipe(ServiceId::WMI_DATA_VO, PipeDirection::In, 2),
    service_pipe(ServiceId::WMI_DATA_BK, PipeDirection::Out, 3),
    service_pipe(ServiceId::WMI_DATA_BK, PipeDirection::In, 2),
    service_pipe(ServiceId::WMI_DATA_BE, PipeDirection::Out, 3),
    service_pipe(ServiceId::WMI_DATA_BE, PipeDirection::In, 2),
    service_pipe(ServiceId::WMI_DATA_VI, PipeDirection::Out, 3),
    service_pipe(ServiceId::WMI_DATA_VI, PipeDirection::In, 2),
    service_pipe(ServiceId::WMI_CONTROL, PipeDirection::Out, 3),
    service_pipe(ServiceId::WMI_CONTROL, PipeDirection::In, 2),
    service_pipe(ServiceId::RESERVED_CONTROL, PipeDirection::Out, 0),
    service_pipe(ServiceId::RESERVED_CONTROL, PipeDirection::In, 2),
    service_pipe(ServiceId::HTT_DATA_MSG, PipeDirection::Out, 4),
    service_pipe(ServiceId::HTT_DATA_MSG, PipeDirection::In, 1),
];

const fn service_pipe(service: ServiceId, direction: PipeDirection, pipe: u8) -> ServicePipeMap {
    ServicePipeMap {
        service,
        direction,
        pipe,
    }
}

pub fn map_service_to_pipe(service: ServiceId) -> Option<(u8, u8)> {
    let mut uplink = None;
    let mut downlink = None;
    for entry in WCN6750_SERVICE_TO_PIPE {
        if entry.service != service {
            continue;
        }
        match entry.direction {
            PipeDirection::Out => uplink = Some(entry.pipe),
            PipeDirection::In => downlink = Some(entry.pipe),
            _ => {}
        }
    }
    match (uplink, downlink) {
        (Some(ul), Some(dl)) => Some((ul, dl)),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HtcHeader {
    pub endpoint: u8,
    pub flags: u8,
    pub payload_len: u16,
    pub control_byte_0: u8,
    pub control_byte_1: u8,
}

impl HtcHeader {
    pub const fn encode(self) -> [u8; HTC_HEADER_LEN] {
        let info =
            (self.endpoint as u32) | ((self.flags as u32) << 8) | ((self.payload_len as u32) << 16);
        let control = (self.control_byte_0 as u32) | ((self.control_byte_1 as u32) << 8);
        let a = info.to_le_bytes();
        let b = control.to_le_bytes();
        [a[0], a[1], a[2], a[3], b[0], b[1], b[2], b[3]]
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, CeError> {
        let info = read_u32(bytes, 0)?;
        let control = read_u32(bytes, 4)?;
        Ok(Self {
            endpoint: info as u8,
            flags: (info >> 8) as u8,
            payload_len: (info >> 16) as u16,
            control_byte_0: control as u8,
            control_byte_1: (control >> 8) as u8,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum HtcMessageId {
    Ready = 1,
    ConnectService = 2,
    ConnectServiceResponse = 3,
    SetupComplete = 4,
    SetupCompleteExtended = 5,
    SendSuspendComplete = 6,
    NackSuspend = 7,
    WakeupFromSuspend = 8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadyMessage {
    pub credit_count: u16,
    pub credit_size: u16,
    pub max_endpoints: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadyExtendedMessage {
    pub base: ReadyMessage,
    pub version: u8,
    pub max_messages_per_bundle: u8,
}

impl ReadyExtendedMessage {
    pub fn decode(bytes: &[u8]) -> Result<Self, CeError> {
        let base = ReadyMessage::decode(bytes)?;
        let version_bundle = read_u32(bytes, 8)?;
        Ok(Self {
            base,
            version: version_bundle as u8,
            max_messages_per_bundle: (version_bundle >> 8) as u8,
        })
    }
}

impl ReadyMessage {
    pub fn decode(bytes: &[u8]) -> Result<Self, CeError> {
        let id_credits = read_u32(bytes, 0)?;
        let size_ep = read_u32(bytes, 4)?;
        if id_credits as u16 != HtcMessageId::Ready as u16 {
            return Err(CeError::InvalidFrame);
        }
        Ok(Self {
            credit_count: (id_credits >> 16) as u16,
            credit_size: size_ep as u16,
            max_endpoints: (size_ep >> 16) as u8,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConnectServiceMessage {
    pub service: ServiceId,
    pub flags: u16,
}

impl ConnectServiceMessage {
    pub const fn encode(self) -> [u8; 8] {
        let a = ((self.service.0 as u32) << 16 | HtcMessageId::ConnectService as u32).to_le_bytes();
        let b = (self.flags as u32).to_le_bytes();
        [a[0], a[1], a[2], a[3], b[0], b[1], b[2], b[3]]
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConnectServiceResponse {
    pub service: ServiceId,
    pub status: u8,
    pub endpoint: u8,
    pub max_message_size: u16,
    pub service_metadata_length: u8,
}

impl ConnectServiceResponse {
    pub fn decode(bytes: &[u8]) -> Result<Self, CeError> {
        let msg_service = read_u32(bytes, 0)?;
        let flags_len = read_u32(bytes, 4)?;
        let metadata = read_u32(bytes, 8)?;
        if msg_service as u16 != HtcMessageId::ConnectServiceResponse as u16 {
            return Err(CeError::InvalidFrame);
        }
        Ok(Self {
            service: ServiceId((msg_service >> 16) as u16),
            status: flags_len as u8,
            endpoint: (flags_len >> 8) as u8,
            max_message_size: (flags_len >> 16) as u16,
            service_metadata_length: metadata as u8,
        })
    }
}

pub fn setup_complete_message(credit_flow: bool) -> [u8; 12] {
    let id = (HtcMessageId::SetupCompleteExtended as u32).to_le_bytes();
    let flags = if credit_flow { 0_u32 } else { 2_u32 }.to_le_bytes();
    [
        id[0], id[1], id[2], id[3], flags[0], flags[1], flags[2], flags[3], 0, 0, 0, 0,
    ]
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TxFrame {
    pub service: ServiceId,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RxFrame {
    pub service: ServiceId,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CeError {
    NoCredits,
    InvalidFrame,
    DeviceFault,
}

impl From<ath11k_platform_backend::Error> for CeError {
    fn from(_: ath11k_platform_backend::Error) -> Self {
        Self::DeviceFault
    }
}

impl From<ath11k_hal::HalError> for CeError {
    fn from(_: ath11k_hal::HalError) -> Self {
        Self::DeviceFault
    }
}

/// A non-coherent CE source buffer. Obtaining the publishable address first
/// transfers the written range to the device, matching `dma_map_single(...,
/// DMA_TO_DEVICE)` in `ath11k_htc_send`.
pub struct CeTxBuffer<B: Backend> {
    dma: StreamingDma<B, ToDevice>,
    length: usize,
}

impl<B: Backend> CeTxBuffer<B> {
    pub fn allocate(device: &Device<B>, capacity: usize) -> Result<Self, CeError> {
        Ok(Self {
            dma: device.alloc_streaming(capacity, 4)?,
            length: 0,
        })
    }

    pub fn write(&mut self, bytes: &[u8]) -> Result<(), CeError> {
        self.dma.write(0, bytes)?;
        self.length = bytes.len();
        Ok(())
    }

    pub fn descriptor(&mut self, transfer_id: u16, byte_swap: bool) -> Result<Descriptor, CeError> {
        self.dma.sync_for_device(0, self.length)?;
        let address = self.dma.device_address_at(0)?;
        Ok(HalCeSourceDescriptor::for_transfer(
            &address,
            self.length as u32,
            transfer_id as u32,
            byte_swap,
        )
        .into_descriptor())
    }

    fn descriptor_before_sync(
        &self,
        transfer_id: u16,
        byte_swap: bool,
    ) -> Result<Descriptor, CeError> {
        let address = self.dma.device_address_at(0)?;
        Ok(HalCeSourceDescriptor::for_transfer(
            &address,
            self.length as u32,
            transfer_id as u32,
            byte_swap,
        )
        .into_descriptor())
    }

    fn sync_written_for_device(&mut self) -> Result<(), CeError> {
        self.dma.sync_for_device(0, self.length).map_err(Into::into)
    }

    pub fn len(&self) -> usize {
        self.length
    }

    pub fn is_empty(&self) -> bool {
        self.length == 0
    }
}

/// A non-coherent CE destination buffer. Completion transfers the completed
/// range to the CPU before exposing payload bytes.
pub struct CeRxBuffer<B: Backend> {
    dma: StreamingDma<B, FromDevice>,
}

impl<B: Backend> CeRxBuffer<B> {
    pub fn allocate(device: &Device<B>, capacity: usize) -> Result<Self, CeError> {
        Ok(Self {
            dma: device.alloc_streaming(capacity, 4)?,
        })
    }

    pub fn descriptor(&self) -> Result<Descriptor, CeError> {
        let address = self.dma.device_address_at(0)?;
        Ok(HalCeDestinationDescriptor::from_address(&address).into_descriptor())
    }

    pub fn complete(&mut self, length: usize) -> Result<Vec<u8>, CeError> {
        self.sync_for_cpu()?;
        self.read(length)
    }

    fn sync_for_cpu(&mut self) -> Result<(), CeError> {
        self.dma.sync_for_cpu(0, self.dma.len()).map_err(Into::into)
    }

    fn read(&self, length: usize) -> Result<Vec<u8>, CeError> {
        let mut bytes = alloc::vec![0; length];
        self.dma.read(0, &mut bytes)?;
        Ok(bytes)
    }

    pub fn capacity(&self) -> usize {
        self.dma.len()
    }
}

pub trait Transport {
    fn bind_service(&mut self, service: ServiceId, tx: RingId, rx: RingId) -> Result<(), CeError>;
    fn send(&mut self, frame: TxFrame) -> Result<(), CeError>;
    fn receive(&mut self, deadline_ns: u64) -> Result<Option<RxFrame>, CeError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Endpoint {
    pub service: ServiceId,
    pub endpoint: u8,
    pub max_message_len: u16,
    pub uplink_pipe: u8,
    pub downlink_pipe: u8,
    pub tx_credits: i32,
    pub credit_flow_enabled: bool,
    sequence: u8,
}

const UNUSED_ENDPOINT: Endpoint = Endpoint {
    service: ServiceId::RESERVED,
    endpoint: 0,
    max_message_len: 0,
    uplink_pipe: 0,
    downlink_pipe: 0,
    tx_credits: 0,
    credit_flow_enabled: true,
    sequence: 0,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Htc {
    endpoints: [Endpoint; HTC_ENDPOINT_COUNT],
    service_allocations: [(ServiceId, u8); 8],
    pub total_transmit_credits: u16,
    pub target_credit_size: u16,
    wmi_endpoint_count: u8,
    credit_flow: bool,
    supports_shadow_registers: bool,
}

impl Htc {
    pub fn new(wmi_endpoint_count: u8, credit_flow: bool, supports_shadow_registers: bool) -> Self {
        let mut endpoints = [UNUSED_ENDPOINT; HTC_ENDPOINT_COUNT];
        let mut i = 0;
        while i < endpoints.len() {
            endpoints[i].endpoint = i as u8;
            i += 1;
        }
        Self {
            endpoints,
            service_allocations: [(ServiceId::RESERVED, 0); 8],
            total_transmit_credits: 0,
            target_credit_size: 0,
            wmi_endpoint_count,
            credit_flow,
            supports_shadow_registers,
        }
    }

    pub fn endpoint(&self, id: u8) -> Option<&Endpoint> {
        self.endpoints.get(id as usize)
    }

    pub fn wait_target(&mut self, bytes: &[u8]) -> Result<ReadyMessage, CeError> {
        let ready = ReadyMessage::decode(bytes)?;
        if ready.credit_count == 0 || ready.credit_size == 0 {
            return Err(CeError::InvalidFrame);
        }
        self.total_transmit_credits = if self.supports_shadow_registers {
            1
        } else {
            ready.credit_count
        };
        self.target_credit_size = ready.credit_size;
        // The setup helper rejects this range, but Linux's wait-target caller
        // intentionally ignores that return value.
        if self.wmi_endpoint_count != 0 && self.wmi_endpoint_count <= 3 {
            let services = [
                ServiceId::WMI_CONTROL,
                ServiceId::WMI_CONTROL_MAC1,
                ServiceId::WMI_CONTROL_MAC2,
            ];
            let credits = (self.total_transmit_credits / self.wmi_endpoint_count as u16) as u8;
            let mut i = 0;
            while i < self.wmi_endpoint_count as usize {
                self.service_allocations[i] = (services[i], credits);
                i += 1;
            }
        }
        Ok(ready)
    }

    pub fn connect_request(&self, service: ServiceId) -> ConnectServiceMessage {
        let allocation = self.credit_allocation(service);
        let mut flags = (allocation as u16) << 8;
        if !self.credit_flow
            || !matches!(
                service,
                ServiceId::WMI_CONTROL | ServiceId::WMI_CONTROL_MAC1 | ServiceId::WMI_CONTROL_MAC2
            )
        {
            flags |= 8;
        }
        ConnectServiceMessage { service, flags }
    }

    pub fn connect_service(
        &mut self,
        service: ServiceId,
        response: &[u8],
    ) -> Result<Endpoint, CeError> {
        if service == ServiceId::RESERVED_CONTROL {
            // Linux uses 256 only for its local validity check, then copies
            // the zeroed dummy response's max-message field into endpoint 0.
            return self.install_endpoint(service, 0, 0, true, HTC_MAX_CTRL_MSG_LEN as u16);
        }
        let response = ConnectServiceResponse::decode(response)?;
        if response.status != 0
            || response.endpoint as usize >= HTC_ENDPOINT_COUNT
            || response.max_message_size == 0
        {
            return Err(CeError::InvalidFrame);
        }
        let disable_credit_flow = !self.credit_flow
            || !matches!(
                service,
                ServiceId::WMI_CONTROL | ServiceId::WMI_CONTROL_MAC1 | ServiceId::WMI_CONTROL_MAC2
            );
        self.install_endpoint(
            service,
            response.endpoint,
            response.max_message_size,
            disable_credit_flow,
            response.max_message_size,
        )
    }

    fn install_endpoint(
        &mut self,
        service: ServiceId,
        endpoint: u8,
        max_message_len: u16,
        disable_credit_flow: bool,
        checked_max_message_len: u16,
    ) -> Result<Endpoint, CeError> {
        let allocation = self.credit_allocation(service) as i32;
        let ep = self
            .endpoints
            .get_mut(endpoint as usize)
            .ok_or(CeError::InvalidFrame)?;
        if ep.service != ServiceId::RESERVED || checked_max_message_len == 0 {
            return Err(CeError::InvalidFrame);
        }
        let (uplink_pipe, downlink_pipe) =
            map_service_to_pipe(service).ok_or(CeError::InvalidFrame)?;
        *ep = Endpoint {
            service,
            endpoint,
            max_message_len,
            uplink_pipe,
            downlink_pipe,
            tx_credits: allocation,
            credit_flow_enabled: !disable_credit_flow,
            sequence: 0,
        };
        Ok(*ep)
    }

    fn credit_allocation(&self, service: ServiceId) -> u8 {
        let mut allocation = 0;
        for entry in self.service_allocations {
            if entry.0 == service {
                allocation = entry.1;
            }
        }
        allocation
    }

    pub fn send(&mut self, endpoint: u8, payload: &[u8]) -> Result<Vec<u8>, CeError> {
        let ep = self
            .endpoints
            .get_mut(endpoint as usize)
            .ok_or(CeError::InvalidFrame)?;
        let frame_len = payload.len() + HTC_HEADER_LEN;
        let credits = if self.credit_flow && ep.credit_flow_enabled {
            if self.target_credit_size == 0 {
                return Err(CeError::InvalidFrame);
            }
            frame_len.div_ceil(self.target_credit_size as usize) as i32
        } else {
            0
        };
        if ep.tx_credits < credits {
            return Err(CeError::NoCredits);
        }
        ep.tx_credits -= credits;
        let flags = if ep.credit_flow_enabled { 1 } else { 0 };
        let header = HtcHeader {
            endpoint,
            flags,
            payload_len: payload.len() as u16,
            control_byte_0: 0,
            control_byte_1: ep.sequence,
        };
        ep.sequence = ep.sequence.wrapping_add(1);
        let mut bytes = Vec::with_capacity(frame_len);
        bytes.extend_from_slice(&header.encode());
        bytes.extend_from_slice(payload);
        Ok(bytes)
    }

    pub fn receive(&mut self, frame: &[u8]) -> Result<Option<RxFrame>, CeError> {
        let header = HtcHeader::decode(frame)?;
        if header.endpoint as usize >= HTC_ENDPOINT_COUNT
            || header.payload_len as usize + HTC_HEADER_LEN > HTC_MAX_LEN
        {
            return Err(CeError::InvalidFrame);
        }
        let payload_end = HTC_HEADER_LEN
            .checked_add(header.payload_len as usize)
            .ok_or(CeError::InvalidFrame)?;
        if frame.len() < payload_end {
            return Err(CeError::InvalidFrame);
        }
        let mut data_end = payload_end;
        let trailer_len = if header.flags & 2 != 0 {
            header.control_byte_0 as usize
        } else {
            0
        };
        if trailer_len != 0 {
            if trailer_len < 4 || trailer_len > header.payload_len as usize {
                return Err(CeError::InvalidFrame);
            }
            let trailer_start = payload_end - trailer_len;
            self.process_trailer(&frame[trailer_start..payload_end])?;
            data_end = trailer_start;
        }
        if trailer_len >= header.payload_len as usize {
            return Ok(None);
        }
        let ep = self.endpoints[header.endpoint as usize];
        Ok(Some(RxFrame {
            service: ep.service,
            bytes: frame[HTC_HEADER_LEN..data_end].to_vec(),
        }))
    }

    fn process_trailer(&mut self, mut bytes: &[u8]) -> Result<(), CeError> {
        while !bytes.is_empty() {
            if bytes.len() < 4 {
                return Err(CeError::InvalidFrame);
            }
            let id = bytes[0];
            let len = bytes[1] as usize;
            let total = 4_usize.checked_add(len).ok_or(CeError::InvalidFrame)?;
            if len > bytes.len() || total > bytes.len() {
                return Err(CeError::InvalidFrame);
            }
            if self.credit_flow && id == 1 {
                if len < 4 {
                    return Err(CeError::InvalidFrame);
                }
                for report in bytes[4..total].chunks_exact(4) {
                    let endpoint = report[0] as usize;
                    if endpoint >= HTC_ENDPOINT_COUNT {
                        break;
                    }
                    self.endpoints[endpoint].tx_credits += report[1] as i32;
                }
            }
            bytes = &bytes[total..];
        }
        Ok(())
    }

    /// Port of `ath11k_htc_start`: frame the setup-complete-extended control
    /// message on endpoint zero.
    pub fn start(&mut self) -> Result<Vec<u8>, CeError> {
        self.send(0, &setup_complete_message(self.credit_flow))
    }

    /// Local lifecycle seam used by core's HIF stop path. Linux has no named
    /// `ath11k_htc_stop`; teardown resets the same endpoint-owned state.
    pub fn stop(&mut self) {
        let wmi_endpoint_count = self.wmi_endpoint_count;
        let credit_flow = self.credit_flow;
        let supports_shadow_registers = self.supports_shadow_registers;
        *self = Self::new(wmi_endpoint_count, credit_flow, supports_shadow_registers);
    }

    /// Port of the endpoint lookup performed by
    /// `ath11k_htc_tx_completion_handler`; buffer disposal/callback ownership
    /// stays with the caller.
    pub fn tx_completion(&self, endpoint: u8) -> Option<ServiceId> {
        self.endpoints.get(endpoint as usize).map(|ep| ep.service)
    }
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, CeError> {
    let slice = bytes.get(offset..offset + 4).ok_or(CeError::InvalidFrame)?;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CeMsi {
    pub address: u64,
    pub data: u32,
}

struct AllocatedPipe<B: Backend> {
    source: Option<RingMemory<B>>,
    destination: Option<RingMemory<B>>,
    status: Option<RingMemory<B>>,
    config: HostPipeConfig,
}

/// Ring memory after `ath11k_ce_alloc_pipes` and before register setup.
pub struct CeAllocatedPipes<B: Backend> {
    pipes: Vec<AllocatedPipe<B>>,
}

impl<B: Backend> CeAllocatedPipes<B> {
    pub fn alloc_pipes(device: &Device<B>) -> Result<Self, CeError> {
        let mut pipes = Vec::with_capacity(CE_COUNT);
        for config in WCN6750_HOST_CE_CONFIG {
            let source = allocate_ring(device, config.source_entries, 16)?;
            let destination = allocate_ring(device, config.destination_entries, 8)?;
            let status = allocate_ring(device, config.destination_entries, 16)?;
            pipes.push(AllocatedPipe {
                source,
                destination,
                status,
                config,
            });
        }
        Ok(Self { pipes })
    }

    /// Port of `ath11k_ce_init_pipes`. The caller owns the HAL-wide remote
    /// read-pointer array and CE MSI assignment chosen by HIF.
    pub fn init_pipes(
        self,
        mmio: &MmioRegion<B>,
        remote_read_pointers: &CoherentDma<B, Bidirectional>,
        msi: [Option<CeMsi>; CE_COUNT],
    ) -> Result<CePipes<B>, CeError> {
        let mut initialized = Vec::with_capacity(CE_COUNT);
        for (pipe_number, pipe) in self.pipes.into_iter().enumerate() {
            let source = setup_ring(
                mmio,
                remote_read_pointers,
                pipe.source,
                RingType::CeSource,
                pipe_number as u8,
                pipe.config,
                msi[pipe_number],
            )?;
            let destination = setup_ring(
                mmio,
                remote_read_pointers,
                pipe.destination,
                RingType::CeDestination,
                pipe_number as u8,
                pipe.config,
                msi[pipe_number],
            )?;
            let status = setup_ring(
                mmio,
                remote_read_pointers,
                pipe.status,
                RingType::CeDestinationStatus,
                pipe_number as u8,
                pipe.config,
                msi[pipe_number],
            )?;
            let source_slots = empty_slots(pipe.config.source_entries as usize);
            let destination_slots = empty_slots(pipe.config.destination_entries as usize);
            initialized.push(InitializedPipe {
                source,
                destination,
                status,
                source_slots,
                destination_slots,
                destination_software_index: 0,
                rx_buffers_needed: if pipe.config.destination_entries == 0 {
                    0
                } else {
                    pipe.config.destination_entries - 2
                },
                config: pipe.config,
            });
        }
        Ok(CePipes { pipes: initialized })
    }

    /// Port of `ath11k_ce_free_pipes`; generation-tied DMA is released by
    /// consuming and dropping the allocation.
    pub fn free_pipes(self) {}
}

fn allocate_ring<B: Backend>(
    device: &Device<B>,
    entries: u16,
    entry_bytes: u16,
) -> Result<Option<RingMemory<B>>, CeError> {
    if entries == 0 {
        return Ok(None);
    }
    let bytes = entries as usize * entry_bytes as usize + 8;
    Ok(Some(RingMemory {
        dma: device.alloc_coherent(bytes, 8)?,
        entries,
        entry_bytes,
    }))
}

fn setup_ring<B: Backend>(
    mmio: &MmioRegion<B>,
    remote_read_pointers: &CoherentDma<B, Bidirectional>,
    memory: Option<RingMemory<B>>,
    ring_type: RingType,
    pipe_number: u8,
    config: HostPipeConfig,
    msi: Option<CeMsi>,
) -> Result<Option<Srng<B>>, CeError> {
    let Some(memory) = memory else {
        return Ok(None);
    };
    let interrupt_enabled = config.flags & CE_ATTR_DISABLE_INTR == 0;
    let mut params = SrngParams::default();
    if interrupt_enabled {
        if let Some(msi) = msi {
            params.msi_address = msi.address;
            params.msi_data = msi.data;
            params.flags = params.flags.union(RingFlags::MSI_INTERRUPT);
        }
        match ring_type {
            RingType::CeSource => params.interrupt_batch_entries = 1,
            RingType::CeDestination => {
                params.interrupt_timer_us = 1024;
                params.low_threshold = memory.entries as u32 - 3;
                params.flags = params.flags.union(RingFlags::LOW_THRESHOLD_INTERRUPT);
            }
            RingType::CeDestinationStatus => {
                params.interrupt_batch_entries = 1;
                params.interrupt_timer_us = 0x1000;
            }
            _ => {}
        }
    }
    if ring_type == RingType::CeDestination {
        params.max_buffer_len = config.source_size_max as u32;
    }
    Srng::setup(
        mmio,
        ring_type,
        pipe_number,
        0,
        memory,
        remote_read_pointers,
        params,
    )
    .map(Some)
    .map_err(Into::into)
}

fn empty_slots<T>(length: usize) -> Vec<Option<T>> {
    core::iter::repeat_with(|| None).take(length).collect()
}

struct InitializedPipe<B: Backend> {
    source: Option<Srng<B>>,
    destination: Option<Srng<B>>,
    status: Option<Srng<B>>,
    source_slots: Vec<Option<CeTxBuffer<B>>>,
    destination_slots: Vec<Option<CeRxBuffer<B>>>,
    destination_software_index: usize,
    rx_buffers_needed: u16,
    config: HostPipeConfig,
}

pub struct CeServiceBatch<B: Backend> {
    pub transmitted: Vec<CeTxBuffer<B>>,
    pub received: Vec<Vec<u8>>,
}

/// Initialized WCN6750 CE pipes. It owns all coherent rings and streaming
/// packet buffers, as `struct ath11k_ce` does in Linux.
pub struct CePipes<B: Backend> {
    pipes: Vec<InitializedPipe<B>>,
}

impl<B: Backend> CePipes<B> {
    pub fn get_attr_flags(&self, pipe: usize) -> Result<u32, CeError> {
        self.pipes
            .get(pipe)
            .map(|p| p.config.flags)
            .ok_or(CeError::InvalidFrame)
    }

    pub fn send(
        &mut self,
        mmio: &MmioRegion<B>,
        remote_read_pointers: &mut CoherentDma<B, Bidirectional>,
        pipe: usize,
        mut buffer: CeTxBuffer<B>,
        transfer_id: u16,
    ) -> Result<(), CeError> {
        let pipe = self.pipes.get_mut(pipe).ok_or(CeError::InvalidFrame)?;
        let ring = pipe.source.as_mut().ok_or(CeError::DeviceFault)?;
        ring.access_begin_remote(remote_read_pointers)?;
        let Some(offset) = ring.source_next_reaped() else {
            ring.access_end(mmio)?;
            return Err(CeError::NoCredits);
        };
        let descriptor = buffer
            .descriptor_before_sync(transfer_id, pipe.config.flags & CE_ATTR_BYTE_SWAP_DATA != 0)?;
        ring.memory.dma.write(offset, descriptor.bytes())?;
        // The packet sync is deliberately between the coherent descriptor
        // write and access_end's release head-pointer store.
        buffer.sync_written_for_device()?;
        let slot = offset / ring.entry_size();
        pipe.source_slots[slot] = Some(buffer);
        ring.access_end(mmio).map_err(Into::into)
    }

    pub fn completed_send_next(
        &mut self,
        remote_read_pointers: &mut CoherentDma<B, Bidirectional>,
        pipe: usize,
    ) -> Result<Option<CeTxBuffer<B>>, CeError> {
        let pipe = self.pipes.get_mut(pipe).ok_or(CeError::InvalidFrame)?;
        let ring = pipe.source.as_mut().ok_or(CeError::DeviceFault)?;
        ring.access_begin_remote(remote_read_pointers)?;
        let Some(offset) = ring.source_reap_next() else {
            return Ok(None);
        };
        Ok(pipe.source_slots[offset / ring.entry_size()].take())
    }

    pub fn post_receive(
        &mut self,
        mmio: &MmioRegion<B>,
        remote_read_pointers: &mut CoherentDma<B, Bidirectional>,
        pipe: usize,
        buffer: CeRxBuffer<B>,
    ) -> Result<(), CeError> {
        let pipe = self.pipes.get_mut(pipe).ok_or(CeError::InvalidFrame)?;
        let ring = pipe.destination.as_mut().ok_or(CeError::DeviceFault)?;
        ring.access_begin_remote(remote_read_pointers)?;
        let Some(offset) = ring.source_next() else {
            ring.access_end(mmio)?;
            return Err(CeError::NoCredits);
        };
        let descriptor = buffer.descriptor()?;
        ring.memory.dma.write(offset, descriptor.bytes())?;
        let slot = offset / ring.entry_size();
        pipe.destination_slots[slot] = Some(buffer);
        pipe.rx_buffers_needed -= 1;
        ring.access_end(mmio).map_err(Into::into)
    }

    pub fn rx_post_buf(
        &mut self,
        device: &Device<B>,
        mmio: &MmioRegion<B>,
        remote_read_pointers: &mut CoherentDma<B, Bidirectional>,
    ) -> Result<(), CeError> {
        for pipe_number in 0..self.pipes.len() {
            while self.pipes[pipe_number].rx_buffers_needed != 0 {
                let size = self.pipes[pipe_number].config.source_size_max as usize;
                let buffer = CeRxBuffer::allocate(device, size)?;
                self.post_receive(mmio, remote_read_pointers, pipe_number, buffer)?;
            }
        }
        Ok(())
    }

    pub fn completed_recv_next(
        &mut self,
        mmio: &MmioRegion<B>,
        remote_read_pointers: &mut CoherentDma<B, Bidirectional>,
        pipe: usize,
    ) -> Result<Option<Vec<u8>>, CeError> {
        let pipe = self.pipes.get_mut(pipe).ok_or(CeError::InvalidFrame)?;
        let status = pipe.status.as_mut().ok_or(CeError::DeviceFault)?;
        status.access_begin_remote(remote_read_pointers)?;
        let Some(offset) = status.destination_next() else {
            status.access_end(mmio)?;
            return Ok(None);
        };
        let mut raw = [0; 16];
        status.memory.dma.read(offset, &mut raw)?;
        let mut descriptor =
            CeDestinationStatusDescriptor::from_bytes(&raw).map_err(|_| CeError::InvalidFrame)?;
        let length = descriptor.take_length() as usize;
        status.memory.dma.write(offset, descriptor.as_bytes())?;
        status.access_end(mmio)?;
        let slot = pipe.destination_software_index;
        pipe.destination_software_index = (slot + 1) & (pipe.destination_slots.len() - 1);
        pipe.rx_buffers_needed += 1;
        let mut buffer = pipe.destination_slots[slot]
            .take()
            .ok_or(CeError::DeviceFault)?;
        buffer.sync_for_cpu()?;
        if length == 0 || length > buffer.capacity() {
            return Err(CeError::InvalidFrame);
        }
        buffer.read(length).map(Some)
    }

    pub fn per_engine_service(
        &mut self,
        mmio: &MmioRegion<B>,
        remote_read_pointers: &mut CoherentDma<B, Bidirectional>,
        pipe: usize,
    ) -> Result<CeServiceBatch<B>, CeError> {
        let mut transmitted = Vec::new();
        if self
            .pipes
            .get(pipe)
            .ok_or(CeError::InvalidFrame)?
            .source
            .is_some()
        {
            while let Some(buffer) = self.completed_send_next(remote_read_pointers, pipe)? {
                transmitted.push(buffer);
            }
        }
        let mut received = Vec::new();
        if self.pipes[pipe].status.is_some() {
            while let Some(buffer) = self.completed_recv_next(mmio, remote_read_pointers, pipe)? {
                received.push(buffer);
            }
        }
        Ok(CeServiceBatch {
            transmitted,
            received,
        })
    }

    pub fn free_pipes(self) {}
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use alloc::{vec, vec::Vec};
    use ath11k_platform_backend::{DmaConstraints, DmaDirection, Error, IrqEvent};
    use drv_hardware_backends::{DeterministicBackend, Operation};
    use std::{cell::RefCell, collections::BTreeMap, ops::Range, rc::Rc};

    #[test]
    fn wcn6750_tables_match_qca6390_c_oracle() {
        let target_words: Vec<[u32; 6]> = WCN6750_TARGET_CE_CONFIG
            .iter()
            .map(|p| {
                [
                    p.pipe as u32,
                    p.direction as u32,
                    p.entries as u32,
                    p.bytes_max as u32,
                    p.flags,
                    p.reserved,
                ]
            })
            .collect();
        assert_eq!(
            target_words,
            vec![
                [0, 2, 32, 2048, 0, 0],
                [1, 1, 32, 2048, 0, 0],
                [2, 1, 32, 2048, 0, 0],
                [3, 2, 32, 2048, 0, 0],
                [4, 2, 256, 256, 8, 0],
                [5, 1, 32, 2048, 0, 0],
                [6, 3, 32, 16384, 0, 0],
                [7, 4, 0, 0, 8, 0],
                [8, 3, 32, 16384, 0, 0]
            ]
        );
        let maps: Vec<(u16, u32, u8)> = WCN6750_SERVICE_TO_PIPE
            .iter()
            .map(|m| (m.service.0, m.direction as u32, m.pipe))
            .collect();
        assert_eq!(
            maps,
            vec![
                (0x104, 2, 3),
                (0x104, 1, 2),
                (0x102, 2, 3),
                (0x102, 1, 2),
                (0x101, 2, 3),
                (0x101, 1, 2),
                (0x103, 2, 3),
                (0x103, 1, 2),
                (0x100, 2, 3),
                (0x100, 1, 2),
                (1, 2, 0),
                (1, 1, 2),
                (0x300, 2, 4),
                (0x300, 1, 1)
            ]
        );
    }

    #[test]
    fn htc_layouts_are_byte_exact() {
        assert_eq!(
            HtcHeader {
                endpoint: 3,
                flags: 2,
                payload_len: 0x1234,
                control_byte_0: 8,
                control_byte_1: 9
            }
            .encode(),
            [3, 2, 0x34, 0x12, 8, 9, 0, 0]
        );
        assert_eq!(
            ConnectServiceMessage {
                service: ServiceId::WMI_CONTROL,
                flags: 0x0108
            }
            .encode(),
            [2, 0, 0, 1, 8, 1, 0, 0]
        );
        assert_eq!(
            setup_complete_message(false),
            [5, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0]
        );
        let response =
            ConnectServiceResponse::decode(&[3, 0, 0, 1, 0, 2, 0, 8, 4, 0, 0, 0]).unwrap();
        assert_eq!(
            response,
            ConnectServiceResponse {
                service: ServiceId::WMI_CONTROL,
                status: 0,
                endpoint: 2,
                max_message_size: 2048,
                service_metadata_length: 4
            }
        );
        assert_eq!(
            ReadyExtendedMessage::decode(&[1, 0, 8, 0, 0, 1, 9, 0, 1, 7, 0, 0]).unwrap(),
            ReadyExtendedMessage {
                base: ReadyMessage {
                    credit_count: 8,
                    credit_size: 256,
                    max_endpoints: 9
                },
                version: 1,
                max_messages_per_bundle: 7,
            }
        );
    }

    #[test]
    fn ready_connect_credit_exhaustion_and_report() {
        let mut htc = Htc::new(1, true, false);
        htc.wait_target(&[1, 0, 4, 0, 0, 1, 9, 0]).unwrap();
        assert_eq!(htc.connect_request(ServiceId::WMI_CONTROL).flags, 0x0400);
        let ep = htc
            .connect_service(
                ServiceId::WMI_CONTROL,
                &[3, 0, 0, 1, 0, 1, 0, 8, 0, 0, 0, 0],
            )
            .unwrap();
        assert_eq!(ep.tx_credits, 4);
        for _ in 0..4 {
            htc.send(1, &[0; 1]).unwrap();
        }
        assert_eq!(htc.send(1, &[0]), Err(CeError::NoCredits));
        let credit_only = [1, 2, 8, 0, 8, 0, 0, 0, 1, 4, 0, 0, 1, 2, 0, 0];
        assert_eq!(htc.receive(&credit_only), Ok(None));
        assert_eq!(htc.endpoint(1).unwrap().tx_credits, 2);
    }

    #[test]
    fn pseudo_control_start_and_local_stop_follow_lifecycle() {
        let mut htc = Htc::new(1, true, true);
        htc.wait_target(&[1, 0, 8, 0, 0, 1, 9, 0]).unwrap();
        assert_eq!(htc.total_transmit_credits, 1);
        let control = htc
            .connect_service(ServiceId::RESERVED_CONTROL, &[])
            .unwrap();
        assert_eq!(control.endpoint, 0);
        // This apparently surprising zero is what Linux copies from its
        // zeroed pseudo-service response, despite validating against 256.
        assert_eq!(control.max_message_len, 0);
        let start = htc.start().unwrap();
        assert_eq!(&start[HTC_HEADER_LEN..], &setup_complete_message(true));
        assert_eq!(htc.tx_completion(0), Some(ServiceId::RESERVED_CONTROL));
        htc.stop();
        assert_eq!(htc.tx_completion(0), Some(ServiceId::RESERVED));
    }

    #[test]
    fn malformed_trailers_fail_without_panic() {
        let mut htc = Htc::new(1, true, false);
        for frame in [
            &[0, 2, 4, 0, 4, 0, 0, 0, 1, 4, 0, 0][..],
            &[0, 2, 8, 0, 8, 0, 0, 0, 1, 8, 0, 0, 1, 1, 0, 0][..],
            &[0, 2, 3, 0, 3, 0, 0, 0, 1, 0, 0][..],
        ] {
            assert_eq!(htc.receive(frame), Err(CeError::InvalidFrame));
        }
    }

    #[test]
    fn streaming_buffers_use_hal_addresses_and_hardware_syncs() {
        let (device, operations) = DeterministicBackend::recording_device();
        let mut tx = CeTxBuffer::allocate(&device, 64).unwrap();
        tx.write(&[1, 2, 3, 4]).unwrap();
        let source = tx.descriptor(1, false).unwrap();
        assert_eq!(&source.bytes()[..4], &[0, 0, 0, 0x10]);
        let mut rx = CeRxBuffer::allocate(&device, 64).unwrap();
        let destination = rx.descriptor().unwrap();
        assert_eq!(&destination.bytes()[..4], &[0x40, 0, 0, 0x10]);
        assert_eq!(rx.complete(4).unwrap(), [0, 0, 0, 0]);
        assert_eq!(
            *operations.borrow(),
            vec![
                Operation::SyncForDevice {
                    dma: 1,
                    range: 0..4
                },
                Operation::SyncForCpu {
                    dma: 2,
                    range: 0..64
                },
            ]
        );
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    enum LargeOperation {
        Read(usize),
        Write(usize, u32),
        DmaWrite(u64, Range<usize>),
        SyncDevice(u64, Range<usize>),
        SyncCpu(u64, Range<usize>),
    }

    #[derive(Default)]
    struct LargeState {
        next_id: u64,
        mmio: BTreeMap<usize, u32>,
        dmas: BTreeMap<u64, Vec<u8>>,
        operations: Vec<LargeOperation>,
    }

    struct LargeModel {
        state: Rc<RefCell<LargeState>>,
    }

    impl Backend for LargeModel {
        type Region = ();
        type Dma = u64;
        type Interrupt = ();

        fn generation(&self) -> u64 {
            1
        }
        fn open_region(&mut self, index: u8) -> Result<(), Error> {
            if index == 0 {
                Ok(())
            } else {
                Err(Error::Invalid)
            }
        }
        fn region_len(&self, _: &()) -> usize {
            0x0200_0000
        }
        fn read_u32(&mut self, _: &(), offset: usize) -> Result<u32, Error> {
            let mut state = self.state.borrow_mut();
            state.operations.push(LargeOperation::Read(offset));
            Ok(*state.mmio.get(&offset).unwrap_or(&0))
        }
        fn write_u32(&mut self, _: &(), offset: usize, value: u32) -> Result<(), Error> {
            let mut state = self.state.borrow_mut();
            state.mmio.insert(offset, value);
            state.operations.push(LargeOperation::Write(offset, value));
            Ok(())
        }
        fn write_dma_address(
            &mut self,
            _: &(),
            low: usize,
            high: Option<usize>,
            dma: &u64,
            offset: usize,
        ) -> Result<(), Error> {
            let address = 0x1000_0000 + (*dma << 20) + offset as u64;
            let mut state = self.state.borrow_mut();
            state.mmio.insert(low, address as u32);
            if let Some(high) = high {
                state.mmio.insert(high, (address >> 32) as u32);
            }
            Ok(())
        }
        fn dma_device_address(&self, dma: &u64, offset: usize) -> Result<u64, Error> {
            Ok(0x1000_0000 + (*dma << 20) + offset as u64)
        }
        fn alloc_dma(
            &mut self,
            size: usize,
            _: usize,
            _: DmaDirection,
            _: bool,
        ) -> Result<u64, Error> {
            let mut state = self.state.borrow_mut();
            state.next_id += 1;
            let id = state.next_id;
            state.dmas.insert(id, vec![0; size]);
            Ok(id)
        }
        fn alloc_dma_constrained(
            &mut self,
            size: usize,
            _: DmaConstraints,
            direction: DmaDirection,
            coherent: bool,
        ) -> Result<u64, Error> {
            self.alloc_dma(size, 8, direction, coherent)
        }
        fn dma_read(
            &mut self,
            dma: &u64,
            range: Range<usize>,
            out: &mut [u8],
        ) -> Result<(), Error> {
            let state = self.state.borrow();
            out.copy_from_slice(
                state
                    .dmas
                    .get(dma)
                    .ok_or(Error::StaleHandle)?
                    .get(range)
                    .ok_or(Error::OutOfBounds)?,
            );
            Ok(())
        }
        fn dma_write(&mut self, dma: &u64, range: Range<usize>, bytes: &[u8]) -> Result<(), Error> {
            let mut state = self.state.borrow_mut();
            state
                .dmas
                .get_mut(dma)
                .ok_or(Error::StaleHandle)?
                .get_mut(range.clone())
                .ok_or(Error::OutOfBounds)?
                .copy_from_slice(bytes);
            state.operations.push(LargeOperation::DmaWrite(*dma, range));
            Ok(())
        }
        fn sync_for_cpu(&mut self, dma: &u64, range: Range<usize>) -> Result<(), Error> {
            self.state
                .borrow_mut()
                .operations
                .push(LargeOperation::SyncCpu(*dma, range));
            Ok(())
        }
        fn sync_for_device(&mut self, dma: &u64, range: Range<usize>) -> Result<(), Error> {
            self.state
                .borrow_mut()
                .operations
                .push(LargeOperation::SyncDevice(*dma, range));
            Ok(())
        }
        fn open_interrupt(&mut self, _: u32) -> Result<(), Error> {
            Ok(())
        }
        fn wait_interrupt(&mut self, _: &(), _: u64) -> Result<Option<IrqEvent>, Error> {
            Ok(None)
        }
        fn reset(&mut self) -> Result<u64, Error> {
            Ok(1)
        }
        fn release_region(&mut self, _: ()) {}
        fn release_dma(&mut self, _: u64) {}
        fn release_interrupt(&mut self, _: ()) {}
    }

    #[test]
    fn full_ce_lifecycle_send_receive_and_release_order() {
        let state = Rc::new(RefCell::new(LargeState::default()));
        let device = Device::from_backend(LargeModel {
            state: state.clone(),
        });
        let mmio = device.open_region(0).unwrap();
        let mut rdp = device.alloc_coherent::<Bidirectional>(176 * 4, 8).unwrap();
        let allocated = CeAllocatedPipes::alloc_pipes(&device).unwrap();
        let mut pipes = allocated.init_pipes(&mmio, &rdp, [None; CE_COUNT]).unwrap();
        assert_eq!(pipes.get_attr_flags(4), Ok(CE_ATTR_DISABLE_INTR));

        state.borrow_mut().operations.clear();
        let mut tx = CeTxBuffer::allocate(&device, 64).unwrap();
        tx.write(&[0x5a; 16]).unwrap();
        let tx_id = state.borrow().next_id;
        pipes.send(&mmio, &mut rdp, 0, tx, 0x1234).unwrap();
        let (descriptor_write, packet_sync, head_write) = {
            let state = state.borrow();
            let operations = &state.operations;
            (
                operations.iter().position(|op| matches!(op, LargeOperation::DmaWrite(_, range) if range == &(0..16))).unwrap(),
                operations.iter().position(|op| matches!(op, LargeOperation::SyncDevice(id, range) if *id == tx_id && range == &(0..16))).unwrap(),
                operations.iter().position(|op| matches!(op, LargeOperation::Write(0x01b8_0400, 4))).unwrap(),
            )
        };
        assert!(descriptor_write < packet_sync && packet_sync < head_write);
        state.borrow_mut().dmas.get_mut(&1).unwrap()[128..132]
            .copy_from_slice(&4_u32.to_le_bytes());
        assert!(pipes.completed_send_next(&mut rdp, 0).unwrap().is_some());

        let rx = CeRxBuffer::allocate(&device, 64).unwrap();
        let rx_id = state.borrow().next_id;
        pipes.post_receive(&mmio, &mut rdp, 1, rx).unwrap();
        state.borrow_mut().dmas.get_mut(&rx_id).unwrap()[..5].copy_from_slice(b"hello");
        // Allocation order: RDP=1, CE0 source=2, CE1 destination=3/status=4.
        state.borrow_mut().dmas.get_mut(&4).unwrap()[..4].copy_from_slice(&[0, 0, 5, 0]);
        state.borrow_mut().dmas.get_mut(&1).unwrap()[324..328]
            .copy_from_slice(&4_u32.to_le_bytes());
        assert_eq!(
            pipes.completed_recv_next(&mmio, &mut rdp, 1),
            Ok(Some(b"hello".to_vec()))
        );
        assert!(state.borrow().operations.iter().any(
            |op| matches!(op, LargeOperation::SyncCpu(id, range) if *id == rx_id && range == &(0..64))
        ));
        pipes.free_pipes();
    }
}
