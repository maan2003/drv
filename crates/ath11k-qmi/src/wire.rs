//! QMI WLFW v01 TLV bodies from Linux ath11k `qmi.[ch]`.
//!
//! QMI TLVs use a one-byte type, a little-endian 16-bit length, then value.
//! The QMI service/message/transaction header remains the transport's concern.

use crate::{QmiError, Request, Response};
use alloc::vec::Vec;

pub const SERVICE_VERSION: u32 = 1;
pub const WCN6750_SERVICE_INSTANCE: u32 = 3;
pub const RESPONSE_MAX_LEN: usize = 8192;
pub const MAX_MEMORY_SEGMENTS: usize = 52;
pub const MAX_MEMORY_CONFIGS: usize = 2;
pub const MAX_DATA_SIZE: usize = 6144;
pub const MAX_GPIOS: usize = 32;
pub const MAX_TARGET_PIPES: usize = 12;
pub const MAX_SERVICE_PIPES: usize = 24;
pub const MAX_SHADOW_REGS: usize = 24;
pub const MAX_SHADOW_REGS_V2: usize = 36;
pub const CLIENT_ID: u32 = 0x4b4e454c;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum MessageId {
    IndicationRegister = 0x0020,
    FirmwareReady = 0x0021,
    WlanMode = 0x0022,
    WlanConfig = 0x0023,
    Capability = 0x0024,
    BdfDownload = 0x0025,
    WlanIni = 0x002f,
    HostCapability = 0x0034,
    RequestMemory = 0x0035,
    RespondMemory = 0x0036,
    FirmwareMemoryReady = 0x0037,
    FirmwareInitDone = 0x0038,
    M3Info = 0x003c,
    ColdBootCalibrationDone = 0x003e,
    DeviceInfo = 0x004c,
}

/// The C QMI decoder accepts the full signed-enum range; named constants are
/// the values currently defined by WLFW v01.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemoryType(pub i32);
impl MemoryType {
    pub const MSA: Self = Self(0);
    pub const DDR: Self = Self(1);
    pub const BDF: Self = Self(2);
    pub const M3: Self = Self(3);
    pub const CAL: Self = Self(4);
    pub const DPD: Self = Self(5);
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QmiString(Vec<u8>);
impl QmiString {
    pub fn new(bytes: Vec<u8>, maximum: usize) -> Result<Self, QmiError> {
        if bytes.len() > maximum {
            return Err(QmiError::MessageTooLong);
        }
        Ok(Self(bytes))
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
    pub fn as_str(&self) -> Option<&str> {
        core::str::from_utf8(&self.0).ok()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(i32)]
pub enum PipeDirection {
    None = 0,
    In = 1,
    Out = 2,
    InOut = 3,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum BdfType {
    Bin = 0,
    Elf = 1,
    RegDb = 4,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum FileType {
    BdfGolden = 0,
    CalData = 2,
    Eeprom = 3,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CalibrationTemperatureId(pub i32);
impl CalibrationTemperatureId {
    pub const INDEX_0: Self = Self(0);
    pub const INDEX_1: Self = Self(1);
    pub const INDEX_2: Self = Self(2);
    pub const INDEX_3: Self = Self(3);
    pub const INDEX_4: Self = Self(4);
    pub const MAX: Self = Self(0xff);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemoryRegionInfo {
    pub region_address: u64,
    pub size: u32,
    pub secure: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QmiResponse {
    pub result: u16,
    pub error: u16,
}

impl QmiResponse {
    pub const fn is_success(self) -> bool {
        self.result == 0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StandardResponse {
    pub response: QmiResponse,
    raw: Vec<u8>,
}
impl StandardResponse {
    pub fn decode(response: &Response) -> Result<Self, QmiError> {
        Ok(Self {
            response: decode_response(response.bytes())?,
            raw: response.bytes().to_vec(),
        })
    }
    pub fn encode(&self, message_id: MessageId) -> Response {
        Response::checked(message_id, self.raw.clone())
            .expect("a decoded response remains a valid TLV body")
    }
}

pub type HostCapabilityResponse = StandardResponse;
pub type RespondMemoryResponse = StandardResponse;
pub type BdfDownloadResponse = StandardResponse;
pub type M3InfoResponse = StandardResponse;
pub type WlanModeResponse = StandardResponse;
pub type WlanConfigResponse = StandardResponse;
pub type WlanIniResponse = StandardResponse;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CapabilityRequest;
impl CapabilityRequest {
    pub fn encode(self) -> Result<Request, QmiError> {
        empty_request()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DeviceInfoRequest;
impl DeviceInfoRequest {
    pub fn encode(self) -> Result<Request, QmiError> {
        Request::from_tlv_bytes(MessageId::DeviceInfo, Vec::new())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndicationRegisterResponse {
    pub response: QmiResponse,
    pub firmware_status: Option<u64>,
    raw: Vec<u8>,
}
impl IndicationRegisterResponse {
    pub fn decode(response: &Response) -> Result<Self, QmiError> {
        Ok(Self {
            response: decode_response(response.bytes())?,
            firmware_status: optional_u64(response.bytes(), 0x10)?,
            raw: response.bytes().to_vec(),
        })
    }
    pub fn encode(&self) -> Response {
        Response::checked(MessageId::IndicationRegister, self.raw.clone())
            .expect("a decoded response remains a valid TLV body")
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HostCapabilityRequest {
    pub num_clients: Option<u32>,
    pub wake_msi: Option<u32>,
    pub gpios: Option<Vec<u32>>,
    pub nm_modem: Option<u8>,
    pub bdf_support: Option<u8>,
    pub bdf_cache_support: Option<u8>,
    pub m3_support: Option<u8>,
    pub m3_cache_support: Option<u8>,
    pub cal_filesys_support: Option<u8>,
    pub cal_cache_support: Option<u8>,
    pub cal_done: Option<u8>,
    pub mem_bucket: Option<u32>,
    pub mem_cfg_mode: Option<u8>,
}

impl HostCapabilityRequest {
    pub fn encode(&self) -> Result<Request, QmiError> {
        let mut b = Vec::new();
        opt_u32(&mut b, 0x10, self.num_clients);
        opt_u32(&mut b, 0x11, self.wake_msi);
        if let Some(v) = &self.gpios {
            if v.len() > MAX_GPIOS {
                return Err(QmiError::MessageTooLong);
            }
            let mut data = Vec::with_capacity(1 + v.len() * 4);
            data.push(v.len() as u8);
            for n in v {
                data.extend_from_slice(&n.to_le_bytes());
            }
            tlv(&mut b, 0x12, &data)?;
        }
        opt_u8(&mut b, 0x13, self.nm_modem);
        opt_u8(&mut b, 0x14, self.bdf_support);
        opt_u8(&mut b, 0x15, self.bdf_cache_support);
        opt_u8(&mut b, 0x16, self.m3_support);
        opt_u8(&mut b, 0x17, self.m3_cache_support);
        opt_u8(&mut b, 0x18, self.cal_filesys_support);
        opt_u8(&mut b, 0x19, self.cal_cache_support);
        opt_u8(&mut b, 0x1a, self.cal_done);
        opt_u32(&mut b, 0x1b, self.mem_bucket);
        opt_u8(&mut b, 0x1c, self.mem_cfg_mode);
        Request::from_tlv_bytes(MessageId::HostCapability, b)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct IndicationRegisterRequest {
    pub fw_ready: Option<u8>,
    pub initiate_cal_download: Option<u8>,
    pub initiate_cal_update: Option<u8>,
    pub msa_ready: Option<u8>,
    pub pin_connect_result: Option<u8>,
    pub client_id: Option<u32>,
    pub request_memory: Option<u8>,
    pub fw_memory_ready: Option<u8>,
    pub fw_init_done: Option<u8>,
    pub rejuvenate: Option<u32>,
    pub xo_cal: Option<u8>,
    pub cal_done: Option<u8>,
}

impl IndicationRegisterRequest {
    pub fn wcn6750() -> Self {
        Self {
            fw_ready: Some(1),
            client_id: Some(CLIENT_ID),
            fw_init_done: Some(1),
            cal_done: Some(1),
            ..Self::default()
        }
    }
    pub fn encode(&self) -> Result<Request, QmiError> {
        let mut b = Vec::new();
        opt_u8(&mut b, 0x10, self.fw_ready);
        opt_u8(&mut b, 0x11, self.initiate_cal_download);
        opt_u8(&mut b, 0x12, self.initiate_cal_update);
        opt_u8(&mut b, 0x13, self.msa_ready);
        opt_u8(&mut b, 0x14, self.pin_connect_result);
        opt_u32(&mut b, 0x15, self.client_id);
        opt_u8(&mut b, 0x16, self.request_memory);
        opt_u8(&mut b, 0x17, self.fw_memory_ready);
        opt_u8(&mut b, 0x18, self.fw_init_done);
        opt_u32(&mut b, 0x19, self.rejuvenate);
        opt_u8(&mut b, 0x1a, self.xo_cal);
        opt_u8(&mut b, 0x1b, self.cal_done);
        Request::from_tlv_bytes(MessageId::IndicationRegister, b)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemoryConfig {
    pub offset: u64,
    pub size: u32,
    pub secure: u8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemorySegment {
    pub size: u32,
    pub kind: MemoryType,
    pub configs: Vec<MemoryConfig>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequestMemoryIndication {
    pub segments: Vec<MemorySegment>,
    raw: Vec<u8>,
}

impl RequestMemoryIndication {
    pub fn decode(bytes: &[u8]) -> Result<Self, QmiError> {
        validate_tlvs(bytes)?;
        reject_unknown_mandatory(bytes, &[0x01])?;
        let Some(data) = find_tlv(bytes, 0x01)? else {
            return Ok(Self {
                segments: Vec::new(),
                raw: bytes.to_vec(),
            });
        };
        let (&count, mut rest) = data.split_first().ok_or(QmiError::Malformed)?;
        if count as usize > MAX_MEMORY_SEGMENTS {
            return Err(QmiError::Malformed);
        }
        let mut segments = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let size = take_u32(&mut rest)?;
            let kind = MemoryType(take_i32(&mut rest)?);
            let cfg_count = take_u8(&mut rest)? as usize;
            if cfg_count > MAX_MEMORY_CONFIGS {
                return Err(QmiError::Malformed);
            }
            let mut configs = Vec::with_capacity(cfg_count);
            for _ in 0..cfg_count {
                configs.push(MemoryConfig {
                    offset: take_u64(&mut rest)?,
                    size: take_u32(&mut rest)?,
                    secure: take_u8(&mut rest)?,
                });
            }
            segments.push(MemorySegment {
                size,
                kind,
                configs,
            });
        }
        if !rest.is_empty() {
            return Err(QmiError::Malformed);
        }
        Ok(Self {
            segments,
            raw: bytes.to_vec(),
        })
    }
    pub fn encode(&self) -> Request {
        Request::from_tlv_bytes(MessageId::RequestMemory, self.raw.clone())
            .expect("a decoded indication remains a valid TLV body")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemorySegmentResponse {
    pub address: u64,
    pub size: u32,
    pub kind: MemoryType,
    pub restore: u8,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RespondMemoryRequest {
    pub segments: Vec<MemorySegmentResponse>,
}

impl RespondMemoryRequest {
    pub fn encode(&self) -> Result<Request, QmiError> {
        if self.segments.len() > MAX_MEMORY_SEGMENTS {
            return Err(QmiError::MessageTooLong);
        }
        let mut data = Vec::with_capacity(1 + self.segments.len() * 17);
        data.push(self.segments.len() as u8);
        for s in &self.segments {
            data.extend_from_slice(&s.address.to_le_bytes());
            data.extend_from_slice(&s.size.to_le_bytes());
            data.extend_from_slice(&s.kind.0.to_le_bytes());
            data.push(s.restore);
        }
        let mut b = Vec::new();
        tlv(&mut b, 0x01, &data)?;
        Request::from_tlv_bytes(MessageId::RespondMemory, b)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChipInfo {
    pub chip_id: u32,
    pub chip_family: u32,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FirmwareVersion {
    pub version: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityResponse {
    pub response: QmiResponse,
    pub chip: Option<ChipInfo>,
    pub board_id: Option<u32>,
    pub soc_id: Option<u32>,
    pub firmware: Option<FirmwareVersion>,
    pub firmware_timestamp: Option<QmiString>,
    pub firmware_build_id: Option<QmiString>,
    pub num_macs: Option<u8>,
    pub voltage_mv: Option<u32>,
    pub time_frequency_hz: Option<u32>,
    pub otp_version: Option<u32>,
    pub eeprom_read_timeout: Option<u32>,
    raw: Vec<u8>,
}

impl CapabilityResponse {
    pub fn decode(response: &Response) -> Result<Self, QmiError> {
        let bytes = response.bytes();
        validate_tlvs(bytes)?;
        let response = decode_response(bytes)?;
        let chip = optional(bytes, 0x10, |v| {
            if v.len() != 8 {
                return Err(QmiError::Malformed);
            }
            Ok(ChipInfo {
                chip_id: u32::from_le_bytes(v[0..4].try_into().map_err(|_| QmiError::Malformed)?),
                chip_family: u32::from_le_bytes(
                    v[4..8].try_into().map_err(|_| QmiError::Malformed)?,
                ),
            })
        })?;
        let board_id = optional_u32(bytes, 0x11)?;
        let soc_id = optional_u32(bytes, 0x12)?;
        let fw = optional(bytes, 0x13, |v| {
            if v.len() < 5 {
                return Err(QmiError::Malformed);
            }
            let version = u32::from_le_bytes(v[0..4].try_into().map_err(|_| QmiError::Malformed)?);
            let timestamp = decode_nested_string(&v[4..], 32)?;
            Ok((FirmwareVersion { version }, timestamp))
        })?;
        let (firmware, firmware_timestamp) = fw.map_or((None, None), |(a, b)| (Some(a), Some(b)));
        Ok(Self {
            response,
            chip,
            board_id,
            soc_id,
            firmware,
            firmware_timestamp,
            firmware_build_id: optional(bytes, 0x14, |v| decode_string(v, 128))?,
            num_macs: optional_u8(bytes, 0x15)?,
            voltage_mv: optional_u32(bytes, 0x16)?,
            time_frequency_hz: optional_u32(bytes, 0x17)?,
            otp_version: optional_u32(bytes, 0x18)?,
            eeprom_read_timeout: optional_u32(bytes, 0x19)?,
            raw: bytes.to_vec(),
        })
    }
    pub fn encode(&self) -> Response {
        Response::checked(MessageId::Capability, self.raw.clone())
            .expect("a decoded response remains a valid TLV body")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceInfoResponse {
    pub response: QmiResponse,
    pub bar_address: Option<u64>,
    pub bar_size: Option<u32>,
    raw: Vec<u8>,
}
impl DeviceInfoResponse {
    pub fn decode(r: &Response) -> Result<Self, QmiError> {
        Ok(Self {
            response: decode_response(r.bytes())?,
            bar_address: optional_u64(r.bytes(), 0x10)?,
            bar_size: optional_u32(r.bytes(), 0x11)?,
            raw: r.bytes().to_vec(),
        })
    }
    pub fn encode(&self) -> Response {
        Response::checked(MessageId::DeviceInfo, self.raw.clone())
            .expect("a decoded response remains a valid TLV body")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BdfDownloadRequest {
    pub valid: u8,
    pub file_id: Option<i32>,
    pub total_size: Option<u32>,
    pub segment_id: Option<u32>,
    pub data: Option<Vec<u8>>,
    pub end: Option<u8>,
    pub bdf_type: Option<u8>,
}
impl BdfDownloadRequest {
    pub fn encode(&self) -> Result<Request, QmiError> {
        if self.data.as_ref().is_some_and(|d| d.len() > MAX_DATA_SIZE) {
            return Err(QmiError::MessageTooLong);
        }
        let mut b = Vec::new();
        tlv(&mut b, 0x01, &[self.valid])?;
        opt_i32(&mut b, 0x10, self.file_id);
        opt_u32(&mut b, 0x11, self.total_size);
        opt_u32(&mut b, 0x12, self.segment_id);
        if let Some(d) = &self.data {
            let mut v = Vec::with_capacity(2 + d.len());
            v.extend_from_slice(&(d.len() as u16).to_le_bytes());
            v.extend_from_slice(d);
            tlv(&mut b, 0x13, &v)?;
        }
        opt_u8(&mut b, 0x14, self.end);
        opt_u8(&mut b, 0x15, self.bdf_type);
        Request::from_tlv_bytes(MessageId::BdfDownload, b)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct M3InfoRequest {
    pub address: u64,
    pub size: u32,
}
impl M3InfoRequest {
    pub fn encode(self) -> Result<Request, QmiError> {
        let mut b = Vec::new();
        tlv(&mut b, 1, &self.address.to_le_bytes())?;
        tlv(&mut b, 2, &self.size.to_le_bytes())?;
        Request::from_tlv_bytes(MessageId::M3Info, b)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WlanModeRequest {
    pub mode: u32,
    pub hardware_debug: Option<u8>,
}
impl WlanModeRequest {
    pub fn encode(self) -> Result<Request, QmiError> {
        let mut b = Vec::new();
        tlv(&mut b, 1, &self.mode.to_le_bytes())?;
        opt_u8(&mut b, 0x10, self.hardware_debug);
        Request::from_tlv_bytes(MessageId::WlanMode, b)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TargetPipeConfig {
    pub pipe_num: u32,
    pub direction: PipeDirection,
    pub entries: u32,
    pub max_bytes: u32,
    pub flags: u32,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ServicePipeConfig {
    pub service_id: u32,
    pub direction: PipeDirection,
    pub pipe_num: u32,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShadowRegister {
    pub id: u16,
    pub offset: u16,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WlanConfigRequest {
    pub host_version: Option<QmiString>,
    pub target_pipes: Option<Vec<TargetPipeConfig>>,
    pub service_pipes: Option<Vec<ServicePipeConfig>>,
    pub shadow_registers: Option<Vec<ShadowRegister>>,
    pub shadow_registers_v2: Option<Vec<u32>>,
}
impl WlanConfigRequest {
    pub fn encode(&self) -> Result<Request, QmiError> {
        let mut b = Vec::new();
        if let Some(s) = &self.host_version {
            if s.as_bytes().len() > 16 {
                return Err(QmiError::MessageTooLong);
            }
            tlv(&mut b, 0x10, s.as_bytes())?
        }
        if let Some(v) = &self.target_pipes {
            if v.len() > MAX_TARGET_PIPES {
                return Err(QmiError::MessageTooLong);
            }
            let mut d = Vec::with_capacity(1 + v.len() * 20);
            d.push(v.len() as u8);
            for x in v {
                d.extend_from_slice(&x.pipe_num.to_le_bytes());
                d.extend_from_slice(&(x.direction as i32).to_le_bytes());
                d.extend_from_slice(&x.entries.to_le_bytes());
                d.extend_from_slice(&x.max_bytes.to_le_bytes());
                d.extend_from_slice(&x.flags.to_le_bytes());
            }
            tlv(&mut b, 0x11, &d)?
        }
        if let Some(v) = &self.service_pipes {
            if v.len() > MAX_SERVICE_PIPES {
                return Err(QmiError::MessageTooLong);
            }
            let mut d = Vec::with_capacity(1 + v.len() * 12);
            d.push(v.len() as u8);
            for x in v {
                d.extend_from_slice(&x.service_id.to_le_bytes());
                d.extend_from_slice(&(x.direction as i32).to_le_bytes());
                d.extend_from_slice(&x.pipe_num.to_le_bytes());
            }
            tlv(&mut b, 0x12, &d)?
        }
        if let Some(v) = &self.shadow_registers {
            if v.len() > MAX_SHADOW_REGS {
                return Err(QmiError::MessageTooLong);
            }
            let mut d = Vec::with_capacity(1 + v.len() * 4);
            d.push(v.len() as u8);
            for x in v {
                d.extend_from_slice(&x.id.to_le_bytes());
                d.extend_from_slice(&x.offset.to_le_bytes());
            }
            tlv(&mut b, 0x13, &d)?
        }
        if let Some(v) = &self.shadow_registers_v2 {
            if v.len() > MAX_SHADOW_REGS_V2 {
                return Err(QmiError::MessageTooLong);
            }
            let mut d = Vec::with_capacity(1 + v.len() * 4);
            d.push(v.len() as u8);
            for x in v {
                d.extend_from_slice(&x.to_le_bytes());
            }
            tlv(&mut b, 0x14, &d)?
        }
        Request::from_tlv_bytes(MessageId::WlanConfig, b)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WlanIniRequest {
    pub enable_firmware_log: Option<u8>,
}
impl WlanIniRequest {
    pub fn encode(self) -> Result<Request, QmiError> {
        let mut b = Vec::new();
        opt_u8(&mut b, 0x10, self.enable_firmware_log);
        Request::from_tlv_bytes(MessageId::WlanIni, b)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Indication {
    RequestMemory(RequestMemoryIndication),
    FirmwareMemoryReady,
    FirmwareReady,
    ColdBootCalibrationDone,
    FirmwareInitDone,
}
impl Indication {
    pub fn decode(id: MessageId, bytes: &[u8]) -> Result<Self, QmiError> {
        match id {
            MessageId::RequestMemory => {
                Ok(Self::RequestMemory(RequestMemoryIndication::decode(bytes)?))
            }
            MessageId::FirmwareMemoryReady => {
                empty(bytes)?;
                Ok(Self::FirmwareMemoryReady)
            }
            MessageId::FirmwareReady => {
                empty(bytes)?;
                Ok(Self::FirmwareReady)
            }
            MessageId::ColdBootCalibrationDone => {
                empty(bytes)?;
                Ok(Self::ColdBootCalibrationDone)
            }
            MessageId::FirmwareInitDone => {
                empty(bytes)?;
                Ok(Self::FirmwareInitDone)
            }
            _ => Err(QmiError::Malformed),
        }
    }
    pub fn encode(&self) -> crate::RawIndication {
        let (id, bytes) = match self {
            Self::RequestMemory(message) => {
                return crate::RawIndication::checked(
                    MessageId::RequestMemory,
                    message.encode().bytes().to_vec(),
                )
                .expect("a decoded indication remains valid")
            }
            Self::FirmwareMemoryReady => (MessageId::FirmwareMemoryReady, Vec::new()),
            Self::FirmwareReady => (MessageId::FirmwareReady, Vec::new()),
            Self::ColdBootCalibrationDone => (MessageId::ColdBootCalibrationDone, Vec::new()),
            Self::FirmwareInitDone => (MessageId::FirmwareInitDone, Vec::new()),
        };
        crate::RawIndication::checked(id, bytes).expect("empty indication is valid")
    }
}

pub fn decode_response(bytes: &[u8]) -> Result<QmiResponse, QmiError> {
    validate_tlvs(bytes)?;
    reject_unknown_mandatory(bytes, &[0x02])?;
    let Some(v) = find_tlv(bytes, 2)? else {
        return Ok(QmiResponse {
            result: 0,
            error: 0,
        });
    };
    if v.len() != 4 {
        return Err(QmiError::Malformed);
    }
    Ok(QmiResponse {
        result: u16::from_le_bytes([v[0], v[1]]),
        error: u16::from_le_bytes([v[2], v[3]]),
    })
}
pub fn empty_request() -> Result<Request, QmiError> {
    Request::from_tlv_bytes(MessageId::Capability, Vec::new())
}

pub(crate) fn validate_tlvs(mut bytes: &[u8]) -> Result<(), QmiError> {
    while !bytes.is_empty() {
        if bytes.len() < 3 {
            return Err(QmiError::Malformed);
        }
        let len = u16::from_le_bytes([bytes[1], bytes[2]]) as usize;
        if bytes.len() < 3 + len {
            return Err(QmiError::Malformed);
        }
        bytes = &bytes[3 + len..];
    }
    Ok(())
}
fn tlv(out: &mut Vec<u8>, kind: u8, value: &[u8]) -> Result<(), QmiError> {
    let len = u16::try_from(value.len()).map_err(|_| QmiError::MessageTooLong)?;
    out.push(kind);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(value);
    Ok(())
}
fn opt_u8(b: &mut Vec<u8>, t: u8, v: Option<u8>) {
    if let Some(v) = v {
        let _ = tlv(b, t, &[v]);
    }
}
fn opt_u32(b: &mut Vec<u8>, t: u8, v: Option<u32>) {
    if let Some(v) = v {
        let _ = tlv(b, t, &v.to_le_bytes());
    }
}
fn opt_i32(b: &mut Vec<u8>, t: u8, v: Option<i32>) {
    if let Some(v) = v {
        let _ = tlv(b, t, &v.to_le_bytes());
    }
}
fn find_tlv(mut b: &[u8], kind: u8) -> Result<Option<&[u8]>, QmiError> {
    while !b.is_empty() {
        if b.len() < 3 {
            return Err(QmiError::Malformed);
        }
        let len = u16::from_le_bytes([b[1], b[2]]) as usize;
        if b.len() < 3 + len {
            return Err(QmiError::Malformed);
        }
        if b[0] == kind {
            return Ok(Some(&b[3..3 + len]));
        }
        b = &b[3 + len..];
    }
    Ok(None)
}
fn optional<T, F: FnOnce(&[u8]) -> Result<T, QmiError>>(
    b: &[u8],
    t: u8,
    f: F,
) -> Result<Option<T>, QmiError> {
    find_tlv(b, t)?.map(f).transpose()
}
fn optional_u8(b: &[u8], t: u8) -> Result<Option<u8>, QmiError> {
    optional(b, t, |v| {
        if v.len() == 1 {
            Ok(v[0])
        } else {
            Err(QmiError::Malformed)
        }
    })
}
fn optional_u32(b: &[u8], t: u8) -> Result<Option<u32>, QmiError> {
    optional(b, t, |v| {
        Ok(u32::from_le_bytes(
            v.try_into().map_err(|_| QmiError::Malformed)?,
        ))
    })
}
fn optional_u64(b: &[u8], t: u8) -> Result<Option<u64>, QmiError> {
    optional(b, t, |v| {
        Ok(u64::from_le_bytes(
            v.try_into().map_err(|_| QmiError::Malformed)?,
        ))
    })
}
fn decode_string(v: &[u8], max: usize) -> Result<QmiString, QmiError> {
    if v.len() > max {
        return Err(QmiError::Malformed);
    }
    let end = v.iter().position(|&x| x == 0).unwrap_or(v.len());
    QmiString::new(v[..end].to_vec(), max)
}
fn decode_nested_string(v: &[u8], max: usize) -> Result<QmiString, QmiError> {
    let (&len, bytes) = v.split_first().ok_or(QmiError::Malformed)?;
    if len as usize != bytes.len() || bytes.len() > max {
        return Err(QmiError::Malformed);
    }
    QmiString::new(bytes.to_vec(), max)
}

fn reject_unknown_mandatory(bytes: &[u8], known: &[u8]) -> Result<(), QmiError> {
    let mut rest = bytes;
    while !rest.is_empty() {
        let kind = rest[0];
        let len = u16::from_le_bytes([rest[1], rest[2]]) as usize;
        if kind < 0x10 && !known.contains(&kind) {
            return Err(QmiError::Malformed);
        }
        rest = &rest[3 + len..];
    }
    Ok(())
}
fn take_u8(b: &mut &[u8]) -> Result<u8, QmiError> {
    let (&v, r) = b.split_first().ok_or(QmiError::Malformed)?;
    *b = r;
    Ok(v)
}
fn take_u32(b: &mut &[u8]) -> Result<u32, QmiError> {
    if b.len() < 4 {
        return Err(QmiError::Malformed);
    }
    let v = u32::from_le_bytes(b[..4].try_into().map_err(|_| QmiError::Malformed)?);
    *b = &b[4..];
    Ok(v)
}
fn take_i32(b: &mut &[u8]) -> Result<i32, QmiError> {
    take_u32(b).map(|v| v as i32)
}
fn take_u64(b: &mut &[u8]) -> Result<u64, QmiError> {
    if b.len() < 8 {
        return Err(QmiError::Malformed);
    }
    let v = u64::from_le_bytes(b[..8].try_into().map_err(|_| QmiError::Malformed)?);
    *b = &b[8..];
    Ok(v)
}
fn empty(bytes: &[u8]) -> Result<(), QmiError> {
    validate_tlvs(bytes)?;
    reject_unknown_mandatory(bytes, &[])
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    #[test]
    fn source_host_cap_fixture() {
        let r = HostCapabilityRequest {
            num_clients: Some(1),
            bdf_support: Some(1),
            m3_support: Some(1),
            m3_cache_support: Some(1),
            cal_done: Some(0),
            mem_cfg_mode: Some(0),
            ..Default::default()
        }
        .encode()
        .unwrap();
        assert_eq!(
            r.bytes(),
            &[
                0x10, 4, 0, 1, 0, 0, 0, 0x14, 1, 0, 1, 0x16, 1, 0, 1, 0x17, 1, 0, 1, 0x1a, 1, 0, 0,
                0x1c, 1, 0, 0
            ]
        );
    }
    #[test]
    fn bdf_data_uses_u16_count() {
        let r = BdfDownloadRequest {
            valid: 1,
            file_id: Some(7),
            total_size: Some(3),
            segment_id: Some(0),
            data: Some(vec![1, 2, 3]),
            end: Some(1),
            bdf_type: Some(0),
        }
        .encode()
        .unwrap();
        assert!(r
            .bytes()
            .windows(8)
            .any(|w| w == [0x13, 5, 0, 3, 0, 1, 2, 3]));
    }
    #[test]
    fn memory_indication_round_trip() {
        let bytes = [
            1, 19, 0, 2, 4, 0, 0, 0, 1, 0, 0, 0, 0, 8, 0, 0, 0, 4, 0, 0, 0, 0,
        ];
        let m = RequestMemoryIndication::decode(&bytes).unwrap();
        assert_eq!(m.segments.len(), 2);
        assert_eq!(m.encode().bytes(), bytes);
    }
    #[test]
    fn response_and_cap_round_trip() {
        let raw = vec![
            2, 4, 0, 0, 0, 0, 0, 0x10, 8, 0, 1, 0, 0, 0, 2, 0, 0, 0, 0x11, 4, 0, 0xff, 0, 0, 0,
            0x13, 9, 0, 0x78, 0x56, 0x34, 0x12, 4, b'1', b'2', b'3', b'4',
        ];
        let r = Response::checked(MessageId::Capability, raw.clone()).unwrap();
        let c = CapabilityResponse::decode(&r).unwrap();
        assert_eq!(c.firmware.unwrap().version, 0x12345678);
        assert_eq!(c.encode().bytes(), raw);
    }
    #[test]
    fn every_prefix_of_nested_message_fails() {
        let good = [
            1, 18, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        for n in 0..good.len() {
            if n == 0 {
                continue;
            }
            assert!(RequestMemoryIndication::decode(&good[..n]).is_err());
        }
    }

    #[test]
    fn wcn6750_registration_fixture() {
        let request = IndicationRegisterRequest::wcn6750().encode().unwrap();
        assert_eq!(
            request.bytes(),
            [0x10, 1, 0, 1, 0x15, 4, 0, 0x4c, 0x45, 0x4e, 0x4b, 0x18, 1, 0, 1, 0x1b, 1, 0, 1,]
        );
    }

    #[test]
    fn scalar_request_elem_info_fixtures() {
        assert_eq!(CapabilityRequest.encode().unwrap().bytes(), []);
        assert_eq!(DeviceInfoRequest.encode().unwrap().bytes(), []);
        assert_eq!(
            M3InfoRequest {
                address: 0x0807_0605_0403_0201,
                size: 0x0c0b_0a09
            }
            .encode()
            .unwrap()
            .bytes(),
            [1, 8, 0, 1, 2, 3, 4, 5, 6, 7, 8, 2, 4, 0, 9, 10, 11, 12]
        );
        assert_eq!(
            WlanModeRequest {
                mode: 3,
                hardware_debug: Some(0)
            }
            .encode()
            .unwrap()
            .bytes(),
            [1, 4, 0, 3, 0, 0, 0, 0x10, 1, 0, 0]
        );
        assert_eq!(
            WlanIniRequest {
                enable_firmware_log: Some(1)
            }
            .encode()
            .unwrap()
            .bytes(),
            [0x10, 1, 0, 1]
        );
    }

    #[test]
    fn unknown_memory_enum_is_preserved_like_c_decoder() {
        let bytes = [1, 10, 0, 1, 4, 0, 0, 0, 99, 0, 0, 0, 0];
        let decoded = RequestMemoryIndication::decode(&bytes).unwrap();
        assert_eq!(decoded.segments[0].kind, MemoryType(99));
        assert_eq!(decoded.encode().bytes(), bytes);
    }

    #[test]
    fn arbitrary_short_inputs_never_panic() {
        let mut bytes = [0u8; 64];
        let mut state = 0x1234_5678u32;
        for len in 0..bytes.len() {
            for byte in &mut bytes[..len] {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                *byte = (state >> 24) as u8;
            }
            let _ = validate_tlvs(&bytes[..len]);
            let _ = RequestMemoryIndication::decode(&bytes[..len]);
            if let Ok(response) = Response::checked(MessageId::Capability, bytes[..len].to_vec()) {
                let _ = CapabilityResponse::decode(&response);
            }
        }
    }
}
