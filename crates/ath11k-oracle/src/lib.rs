#![deny(unsafe_op_in_unsafe_fn)]

//! Differential bindings to the pinned Linux ath11k QMI codec.
//!
//! C protocol structs and element tables are extracted verbatim from the
//! pinned `qmi.[ch]` into the C translation unit by `build.rs`. Only valid,
//! typed values cross this crate's FFI boundary.

use ath11k_qmi::wire::{
    BdfDownloadRequest, HostCapabilityRequest, IndicationRegisterRequest, M3InfoRequest,
    RespondMemoryRequest, WlanConfigRequest, WlanIniRequest,
    WlanModeRequest,
};
use core::ffi::{c_int, c_uint};
use core::mem::size_of;

const HOST_CAP_MAX: usize = 261;
const IND_REGISTER_MAX: usize = 54;
const RESPOND_MEMORY_MAX: usize = 888;
const BDF_MAX: usize = 6182;
const M3_MAX: usize = 18;
const WLAN_MODE_MAX: usize = 11;
const WLAN_CONFIG_MAX: usize = 803;
const WLAN_INI_MAX: usize = 4;
const MAX_MEMORY_SEGMENTS: usize = 52;
const MAX_DATA: usize = 6144;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CHostCapability {
    num_clients_valid: u8, num_clients: u32,
    wake_msi_valid: u8, wake_msi: u32,
    gpios_valid: u8, gpios_len: u32, gpios: [u32; 32],
    nm_modem_valid: u8, nm_modem: u8,
    bdf_support_valid: u8, bdf_support: u8,
    bdf_cache_support_valid: u8, bdf_cache_support: u8,
    m3_support_valid: u8, m3_support: u8,
    m3_cache_support_valid: u8, m3_cache_support: u8,
    cal_filesys_support_valid: u8, cal_filesys_support: u8,
    cal_cache_support_valid: u8, cal_cache_support: u8,
    cal_done_valid: u8, cal_done: u8,
    mem_bucket_valid: u8, mem_bucket: u32,
    mem_cfg_mode_valid: u8, mem_cfg_mode: u8,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CIndicationRegister {
    fw_ready_valid: u8, fw_ready: u8,
    cal_download_valid: u8, cal_download: u8,
    cal_update_valid: u8, cal_update: u8,
    msa_ready_valid: u8, msa_ready: u8,
    pin_result_valid: u8, pin_result: u8,
    client_id_valid: u8, client_id: u32,
    request_memory_valid: u8, request_memory: u8,
    fw_memory_ready_valid: u8, fw_memory_ready: u8,
    fw_init_done_valid: u8, fw_init_done: u8,
    rejuvenate_valid: u8, rejuvenate: u32,
    xo_cal_valid: u8, xo_cal: u8,
    cal_done_valid: u8, cal_done: u8,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CMemorySegmentResponse { address: u64, size: u32, kind: i32, restore: u8 }

#[repr(C)]
#[derive(Clone, Copy)]
struct CRespondMemory { len: u32, segments: [CMemorySegmentResponse; MAX_MEMORY_SEGMENTS] }
impl Default for CRespondMemory {
    fn default() -> Self { Self { len: 0, segments: [CMemorySegmentResponse::default(); MAX_MEMORY_SEGMENTS] } }
}

#[repr(C)]
struct CBdfDownload {
    valid: u8,
    file_id_valid: u8, file_id: i32,
    total_size_valid: u8, total_size: u32,
    segment_id_valid: u8, segment_id: u32,
    data_valid: u8, data_len: u32, data: [u8; MAX_DATA],
    end_valid: u8, end: u8,
    bdf_type_valid: u8, bdf_type: u8,
}
impl Default for CBdfDownload {
    fn default() -> Self {
        Self { valid: 0, file_id_valid: 0, file_id: 0, total_size_valid: 0,
            total_size: 0, segment_id_valid: 0, segment_id: 0, data_valid: 0,
            data_len: 0, data: [0; MAX_DATA], end_valid: 0, end: 0,
            bdf_type_valid: 0, bdf_type: 0 }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CM3Info { address: u64, size: u32 }
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CWlanMode { mode: u32, hardware_debug_valid: u8, hardware_debug: u8 }
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CTargetPipe { pipe_num: u32, direction: u32, entries: u32, max_bytes: u32, flags: u32 }
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CServicePipe { service_id: u32, direction: u32, pipe_num: u32 }
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CShadowRegister { id: u16, offset: u16 }
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CShadowRegisterV2 { address: u32 }
#[repr(C)]
#[derive(Clone, Copy)]
struct CWlanConfig {
    host_version_valid: u8, host_version: [u8; 17],
    target_valid: u8, target_len: u32, target: [CTargetPipe; 12],
    service_valid: u8, service_len: u32, service: [CServicePipe; 24],
    shadow_valid: u8, shadow_len: u32, shadow: [CShadowRegister; 24],
    shadow_v2_valid: u8, shadow_v2_len: u32, shadow_v2: [CShadowRegisterV2; 36],
}
impl Default for CWlanConfig {
    fn default() -> Self {
        Self { host_version_valid: 0, host_version: [0; 17], target_valid: 0,
            target_len: 0, target: [CTargetPipe::default(); 12], service_valid: 0,
            service_len: 0, service: [CServicePipe::default(); 24], shadow_valid: 0,
            shadow_len: 0, shadow: [CShadowRegister::default(); 24], shadow_v2_valid: 0,
            shadow_v2_len: 0, shadow_v2: [CShadowRegisterV2::default(); 36] }
    }
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CWlanIni { valid: u8, value: u8 }

unsafe extern "C" {
    fn oracle_qmi_host_cap_encode(input: *const CHostCapability, out: *mut u8, capacity: usize) -> c_int;
    fn oracle_qmi_ind_register_encode(input: *const CIndicationRegister, out: *mut u8, capacity: usize) -> c_int;
    fn oracle_qmi_respond_memory_encode(input: *const CRespondMemory, out: *mut u8, capacity: usize) -> c_int;
    fn oracle_qmi_bdf_download_encode(input: *const CBdfDownload, out: *mut u8, capacity: usize) -> c_int;
    fn oracle_qmi_m3_info_encode(input: *const CM3Info, out: *mut u8, capacity: usize) -> c_int;
    fn oracle_qmi_wlan_mode_encode(input: *const CWlanMode, out: *mut u8, capacity: usize) -> c_int;
    fn oracle_qmi_wlan_config_encode(input: *const CWlanConfig, out: *mut u8, capacity: usize) -> c_int;
    fn oracle_qmi_wlan_ini_encode(input: *const CWlanIni, out: *mut u8, capacity: usize) -> c_int;
    fn oracle_qmi_empty_encode(kind: c_uint, out: *mut u8, capacity: usize) -> c_int;
    fn oracle_qmi_decode_reencode(body: *const u8, body_len: usize, kind: c_uint,
        output: *mut u8, output_len: usize, encoded: *mut u8, capacity: usize) -> c_int;
}

fn option<T: Copy + Default>(value: Option<T>) -> (u8, T) {
    (u8::from(value.is_some()), value.unwrap_or_default())
}
fn encoded<T>(input: &T, maximum: usize, encode: unsafe extern "C" fn(*const T, *mut u8, usize) -> c_int) -> Result<Vec<u8>, i32> {
    let mut bytes = vec![0; maximum];
    // SAFETY: `input` is a valid C-layout object and `bytes` is writable for
    // `maximum` bytes. Each wrapper passes the matching pinned elem_info table.
    let result = unsafe { encode(input, bytes.as_mut_ptr(), bytes.len()) };
    if result < 0 { return Err(result); }
    bytes.truncate(result as usize);
    Ok(bytes)
}

pub fn c_host_capability(input: &HostCapabilityRequest) -> Result<Vec<u8>, i32> {
    let (num_clients_valid, num_clients) = option(input.num_clients);
    let (wake_msi_valid, wake_msi) = option(input.wake_msi);
    let (nm_modem_valid, nm_modem) = option(input.nm_modem);
    let (bdf_support_valid, bdf_support) = option(input.bdf_support);
    let (bdf_cache_support_valid, bdf_cache_support) = option(input.bdf_cache_support);
    let (m3_support_valid, m3_support) = option(input.m3_support);
    let (m3_cache_support_valid, m3_cache_support) = option(input.m3_cache_support);
    let (cal_filesys_support_valid, cal_filesys_support) = option(input.cal_filesys_support);
    let (cal_cache_support_valid, cal_cache_support) = option(input.cal_cache_support);
    let (cal_done_valid, cal_done) = option(input.cal_done);
    let (mem_bucket_valid, mem_bucket) = option(input.mem_bucket);
    let (mem_cfg_mode_valid, mem_cfg_mode) = option(input.mem_cfg_mode);
    let mut c = CHostCapability { num_clients_valid, num_clients, wake_msi_valid, wake_msi,
        nm_modem_valid, nm_modem, bdf_support_valid, bdf_support, bdf_cache_support_valid,
        bdf_cache_support, m3_support_valid, m3_support, m3_cache_support_valid,
        m3_cache_support, cal_filesys_support_valid, cal_filesys_support,
        cal_cache_support_valid, cal_cache_support, cal_done_valid, cal_done,
        mem_bucket_valid, mem_bucket, mem_cfg_mode_valid, mem_cfg_mode, ..Default::default() };
    if let Some(gpios) = &input.gpios {
        c.gpios_valid = 1; c.gpios_len = gpios.len() as u32;
        c.gpios[..gpios.len()].copy_from_slice(gpios);
    }
    encoded(&c, HOST_CAP_MAX, oracle_qmi_host_cap_encode)
}

pub fn c_indication_register(input: &IndicationRegisterRequest) -> Result<Vec<u8>, i32> {
    let (fw_ready_valid, fw_ready) = option(input.fw_ready);
    let (cal_download_valid, cal_download) = option(input.initiate_cal_download);
    let (cal_update_valid, cal_update) = option(input.initiate_cal_update);
    let (msa_ready_valid, msa_ready) = option(input.msa_ready);
    let (pin_result_valid, pin_result) = option(input.pin_connect_result);
    let (client_id_valid, client_id) = option(input.client_id);
    let (request_memory_valid, request_memory) = option(input.request_memory);
    let (fw_memory_ready_valid, fw_memory_ready) = option(input.fw_memory_ready);
    let (fw_init_done_valid, fw_init_done) = option(input.fw_init_done);
    let (rejuvenate_valid, rejuvenate8) = option(input.rejuvenate);
    let (xo_cal_valid, xo_cal) = option(input.xo_cal);
    let (cal_done_valid, cal_done) = option(input.cal_done);
    encoded(&CIndicationRegister { fw_ready_valid, fw_ready, cal_download_valid, cal_download,
        cal_update_valid, cal_update, msa_ready_valid, msa_ready, pin_result_valid, pin_result,
        client_id_valid, client_id, request_memory_valid, request_memory, fw_memory_ready_valid,
        fw_memory_ready, fw_init_done_valid, fw_init_done, rejuvenate_valid,
        rejuvenate: u32::from(rejuvenate8), xo_cal_valid, xo_cal, cal_done_valid, cal_done },
        IND_REGISTER_MAX, oracle_qmi_ind_register_encode)
}

pub fn c_respond_memory(input: &RespondMemoryRequest) -> Result<Vec<u8>, i32> {
    let mut c = CRespondMemory { len: input.segments.len() as u32, ..Default::default() };
    for (out, segment) in c.segments.iter_mut().zip(&input.segments) {
        *out = CMemorySegmentResponse { address: segment.address, size: segment.size,
            kind: segment.kind.0, restore: segment.restore };
    }
    encoded(&c, RESPOND_MEMORY_MAX, oracle_qmi_respond_memory_encode)
}

pub fn c_bdf_download(input: &BdfDownloadRequest) -> Result<Vec<u8>, i32> {
    let (file_id_valid, file_id) = option(input.file_id);
    let (total_size_valid, total_size) = option(input.total_size);
    let (segment_id_valid, segment_id) = option(input.segment_id);
    let (end_valid, end) = option(input.end);
    let (bdf_type_valid, bdf_type) = option(input.bdf_type);
    let mut c = CBdfDownload { valid: input.valid, file_id_valid, file_id, total_size_valid,
        total_size, segment_id_valid, segment_id, end_valid, end, bdf_type_valid, bdf_type,
        ..Default::default() };
    if let Some(data) = &input.data {
        c.data_valid = 1; c.data_len = data.len() as u32; c.data[..data.len()].copy_from_slice(data);
    }
    encoded(&c, BDF_MAX, oracle_qmi_bdf_download_encode)
}

pub fn c_m3_info(input: M3InfoRequest) -> Result<Vec<u8>, i32> {
    encoded(&CM3Info { address: input.address, size: input.size }, M3_MAX, oracle_qmi_m3_info_encode)
}
pub fn c_wlan_mode(input: WlanModeRequest) -> Result<Vec<u8>, i32> {
    let (hardware_debug_valid, hardware_debug) = option(input.hardware_debug);
    encoded(&CWlanMode { mode: input.mode, hardware_debug_valid, hardware_debug }, WLAN_MODE_MAX, oracle_qmi_wlan_mode_encode)
}
pub fn c_wlan_ini(input: WlanIniRequest) -> Result<Vec<u8>, i32> {
    let (valid, value) = option(input.enable_firmware_log);
    encoded(&CWlanIni { valid, value }, WLAN_INI_MAX, oracle_qmi_wlan_ini_encode)
}

pub fn c_wlan_config(input: &WlanConfigRequest) -> Result<Vec<u8>, i32> {
    let mut c = CWlanConfig::default();
    if let Some(version) = &input.host_version {
        c.host_version_valid = 1;
        let bytes = version.as_bytes();
        c.host_version[..bytes.len()].copy_from_slice(bytes);
    }
    if let Some(values) = &input.target_pipes {
        c.target_valid = 1; c.target_len = values.len() as u32;
        for (out, value) in c.target.iter_mut().zip(values) {
            *out = CTargetPipe { pipe_num: value.pipe_num, direction: value.direction as u32,
                entries: value.entries, max_bytes: value.max_bytes, flags: value.flags };
        }
    }
    if let Some(values) = &input.service_pipes {
        c.service_valid = 1; c.service_len = values.len() as u32;
        for (out, value) in c.service.iter_mut().zip(values) {
            *out = CServicePipe { service_id: value.service_id, direction: value.direction as u32,
                pipe_num: value.pipe_num };
        }
    }
    if let Some(values) = &input.shadow_registers {
        c.shadow_valid = 1; c.shadow_len = values.len() as u32;
        for (out, value) in c.shadow.iter_mut().zip(values) {
            *out = CShadowRegister { id: value.id, offset: value.offset };
        }
    }
    if let Some(values) = &input.shadow_registers_v2 {
        c.shadow_v2_valid = 1; c.shadow_v2_len = values.len() as u32;
        for (out, &address) in c.shadow_v2.iter_mut().zip(values) { out.address = address; }
    }
    encoded(&c, WLAN_CONFIG_MAX, oracle_qmi_wlan_config_encode)
}

pub fn c_empty_request(device_info: bool) -> Result<Vec<u8>, i32> {
    let mut bytes = Vec::<u8>::new();
    // SAFETY: a zero capacity output is valid because both selected pinned
    // elem_info tables encode an empty body and therefore write no body bytes.
    let result = unsafe { oracle_qmi_empty_encode(u32::from(device_info), bytes.as_mut_ptr(), 0) };
    if result < 0 { Err(result) } else { Ok(bytes) }
}

/// Decodes a valid body with the pinned element table and re-encodes the C
/// struct. Equality with the original canonical body proves the C-decoded
/// field values, presence flags, counts and element order all match.
pub fn c_decode_reencode(kind: u32, body: &[u8]) -> Result<Vec<u8>, i32> {
    // The largest decoded object is request-memory (52 segments, each with two
    // memory configurations). u64 backing gives the C object suitable alignment.
    let mut output = vec![0_u64; 512];
    let mut encoded = vec![0_u8; 8192];
    // SAFETY: both byte buffers are valid for their supplied lengths. `output`
    // is u64-aligned and large enough for every selected pinned protocol struct.
    let result = unsafe { oracle_qmi_decode_reencode(body.as_ptr(), body.len(), kind,
        output.as_mut_ptr().cast(), output.len() * size_of::<u64>(),
        encoded.as_mut_ptr(), encoded.len()) };
    if result < 0 { return Err(result); }
    encoded.truncate(result as usize);
    Ok(encoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ath11k_qmi::wire::{
        CapabilityRequest, CapabilityResponse, DeviceInfoRequest, DeviceInfoResponse, Indication,
        IndicationRegisterResponse, MemoryType, PipeDirection, QmiString, MemorySegmentResponse,
        MessageId, RequestMemoryIndication, ServicePipeConfig, ShadowRegister, StandardResponse,
        TargetPipeConfig,
    };
    use ath11k_qmi::{Response, TransactionId};
    use ath11k_qmi::trace::{self, TraceEvent as QmiTraceEvent, TraceSink};
    use proptest::collection::vec;
    use proptest::option;
    use proptest::prelude::*;

    #[derive(Clone, Debug, Eq, PartialEq)]
    enum Event { Tlv { tag: u8, len: usize, offset: usize }, Field(String, u64), Branch(String) }
    #[derive(Default)]
    struct Sink(Vec<Event>);
    impl TraceSink for Sink {
        fn event(&mut self, event: QmiTraceEvent<'_>) {
            match event {
                QmiTraceEvent::Elem { tlv_type, offset, len } =>
                    self.0.push(Event::Tlv { tag: tlv_type, len, offset }),
                QmiTraceEvent::Field { name, value } => self.0.push(Event::Field(name.into(), value)),
                QmiTraceEvent::Branch { name } => self.0.push(Event::Branch(name.into())),
                QmiTraceEvent::Reject { .. } => panic!("valid typed input produced a rejection trace"),
            }
        }
    }
    fn c_trace(id: MessageId, bytes: &[u8]) -> Vec<Event> {
        let mut sink = Sink::default();
        assert!(trace::trace_request(id, bytes, &mut sink));
        sink.0
    }

    fn direction(value: u8) -> PipeDirection {
        match value % 4 { 0 => PipeDirection::None, 1 => PipeDirection::In,
            2 => PipeDirection::Out, _ => PipeDirection::InOut }
    }

    proptest! {
        #[test]
        fn host_capability_encode_matches_c(
            num_clients in option::of(any::<u32>()), wake_msi in option::of(any::<u32>()),
            gpios in option::of(vec(any::<u32>(), 0..=32)), nm_modem in option::of(any::<u8>()),
            bdf_support in option::of(any::<u8>()), bdf_cache_support in option::of(any::<u8>()),
            m3_support in option::of(any::<u8>()), m3_cache_support in option::of(any::<u8>()),
            cal_filesys_support in option::of(any::<u8>()), cal_cache_support in option::of(any::<u8>()),
            cal_done in option::of(any::<u8>()), mem_bucket in option::of(any::<u32>()),
            mem_cfg_mode in option::of(any::<u8>()),
        ) {
            let message = HostCapabilityRequest { num_clients, wake_msi, gpios, nm_modem,
                bdf_support, bdf_cache_support, m3_support, m3_cache_support,
                cal_filesys_support, cal_cache_support, cal_done, mem_bucket, mem_cfg_mode };
            let mut trace = Sink::default();
            let rust = message.encode_with_trace(Some(&mut trace)).unwrap().bytes().to_vec();
            let c = c_host_capability(&message).unwrap();
            prop_assert_eq!(&rust, &c);
            prop_assert_eq!(trace.0, c_trace(MessageId::HostCapability, &c));
        }

        #[test]
        fn indication_register_encode_matches_c(values in vec(option::of(any::<u8>()), 12), client_id in option::of(any::<u32>())) {
            let message = IndicationRegisterRequest { fw_ready: values[0], initiate_cal_download: values[1],
                initiate_cal_update: values[2], msa_ready: values[3], pin_connect_result: values[4], client_id,
                request_memory: values[5], fw_memory_ready: values[6], fw_init_done: values[7],
                rejuvenate: values[8], xo_cal: values[9], cal_done: values[10] };
            let mut trace = Sink::default();
            let rust = message.encode_with_trace(Some(&mut trace)).unwrap().bytes().to_vec();
            let c = c_indication_register(&message).unwrap();
            prop_assert_eq!(&rust, &c);
            prop_assert_eq!(trace.0, c_trace(MessageId::IndicationRegister, &c));
        }

        #[test]
        fn bdf_download_encode_matches_c(valid: u8, file_id in option::of(any::<i32>()),
            total_size in option::of(any::<u32>()), segment_id in option::of(any::<u32>()),
            data in option::of(vec(any::<u8>(), 0..=6144)), end in option::of(any::<u8>()),
            bdf_type in option::of(any::<u8>())) {
            let message = BdfDownloadRequest { valid, file_id, total_size, segment_id, data, end, bdf_type };
            let mut trace = Sink::default();
            let rust = message.encode_with_trace(Some(&mut trace)).unwrap().bytes().to_vec();
            let c = c_bdf_download(&message).unwrap();
            prop_assert_eq!(&rust, &c);
            prop_assert_eq!(trace.0, c_trace(MessageId::BdfDownload, &c));
        }

        #[test]
        fn scalar_requests_encode_match_c(address: u64, size: u32, mode: u32,
            debug in option::of(any::<u8>()), fw_log in option::of(any::<u8>())) {
            let m3 = M3InfoRequest { address, size };
            let mut trace = Sink::default();
            let rust = m3.encode_with_trace(Some(&mut trace)).unwrap().bytes().to_vec();
            let c = c_m3_info(m3).unwrap();
            prop_assert_eq!(&rust, &c);
            prop_assert_eq!(trace.0, c_trace(MessageId::M3Info, &c));
            let mode = WlanModeRequest { mode, hardware_debug: debug };
            let mut trace = Sink::default();
            let rust = mode.encode_with_trace(Some(&mut trace)).unwrap().bytes().to_vec();
            let c = c_wlan_mode(mode).unwrap();
            prop_assert_eq!(&rust, &c);
            prop_assert_eq!(trace.0, c_trace(MessageId::WlanMode, &c));
            let ini = WlanIniRequest { enable_firmware_log: fw_log };
            let mut trace = Sink::default();
            let rust = ini.encode_with_trace(Some(&mut trace)).unwrap().bytes().to_vec();
            let c = c_wlan_ini(ini).unwrap();
            prop_assert_eq!(&rust, &c);
            prop_assert_eq!(trace.0, c_trace(MessageId::WlanIni, &c));
        }

        #[test]
        fn respond_memory_encode_matches_c(segments in vec((any::<u64>(), any::<u32>(), any::<i32>(), any::<u8>()), 0..=52)) {
            let message = RespondMemoryRequest { segments: segments.into_iter().map(|(address,size,kind,restore)|
                MemorySegmentResponse { address, size, kind: MemoryType(kind), restore }).collect() };
            let mut trace = Sink::default();
            let rust = message.encode_with_trace(Some(&mut trace)).unwrap().bytes().to_vec();
            let c = c_respond_memory(&message).unwrap();
            prop_assert_eq!(&rust, &c);
            prop_assert_eq!(trace.0, c_trace(MessageId::RespondMemory, &c));
        }

        #[test]
        fn wlan_config_encode_matches_c(
            version in option::of(vec(any::<u8>(), 0..=16)),
            targets in option::of(vec((any::<u32>(), any::<u8>(), any::<u32>(), any::<u32>(), any::<u32>()), 0..=12)),
            services in option::of(vec((any::<u32>(), any::<u8>(), any::<u32>()), 0..=24)),
            shadows in option::of(vec((any::<u16>(), any::<u16>()), 0..=24)),
            shadows_v2 in option::of(vec(any::<u32>(), 0..=36)),
        ) {
            let message = WlanConfigRequest {
                host_version: version.map(|bytes| QmiString::new(bytes, 16).unwrap()),
                target_pipes: targets.map(|values| values.into_iter().map(|(pipe_num, dir, entries, max_bytes, flags)|
                    TargetPipeConfig { pipe_num, direction: direction(dir), entries, max_bytes, flags }).collect()),
                service_pipes: services.map(|values| values.into_iter().map(|(service_id, dir, pipe_num)|
                    ServicePipeConfig { service_id, direction: direction(dir), pipe_num }).collect()),
                shadow_registers: shadows.map(|values| values.into_iter().map(|(id, offset)| ShadowRegister { id, offset }).collect()),
                shadow_registers_v2: shadows_v2,
            };
            let mut trace = Sink::default();
            let rust = message.encode_with_trace(Some(&mut trace)).unwrap().bytes().to_vec();
            let c = c_wlan_config(&message).unwrap();
            prop_assert_eq!(&rust, &c);
            prop_assert_eq!(trace.0, c_trace(MessageId::WlanConfig, &c));
        }
    }

    fn response(id: MessageId, body: &[u8]) -> Response {
        Response::checked(TransactionId::new(1), id, body.to_vec()).unwrap()
    }

    #[test]
    fn all_wcn6750_bringup_response_and_indication_fixtures_match_c() {
        let standard = [2, 4, 0, 0, 0, 0, 0];
        let c = c_decode_reencode(0, &standard).unwrap();
        assert_eq!(c, standard);
        let mut rust_trace = Sink::default();
        let decoded = StandardResponse::decode_with_trace(
            &response(MessageId::HostCapability, &standard), Some(&mut rust_trace)).unwrap();
        let mut c_trace = Sink::default();
        StandardResponse::decode_with_trace(
            &response(MessageId::HostCapability, &c), Some(&mut c_trace)).unwrap();
        assert_eq!(rust_trace.0, c_trace.0);
        assert_eq!(decoded.response.result, 0);

        let registration = [2, 4, 0, 0, 0, 0, 0, 0x10, 8, 0, 8, 7, 6, 5, 4, 3, 2, 1];
        let c = c_decode_reencode(1, &registration).unwrap();
        assert_eq!(c, registration);
        let mut rust_trace = Sink::default();
        let decoded = IndicationRegisterResponse::decode_with_trace(
            &response(MessageId::IndicationRegister, &registration), Some(&mut rust_trace)).unwrap();
        let mut c_trace = Sink::default();
        IndicationRegisterResponse::decode_with_trace(
            &response(MessageId::IndicationRegister, &c), Some(&mut c_trace)).unwrap();
        assert_eq!(rust_trace.0, c_trace.0);
        assert_eq!(decoded.firmware_status, Some(0x0102_0304_0506_0708));

        let capability = [2, 4, 0, 0, 0, 0, 0, 0x10, 8, 0, 1, 0, 0, 0, 2, 0, 0, 0,
            0x11, 4, 0, 0xff, 0, 0, 0, 0x13, 9, 0, 0x78, 0x56, 0x34, 0x12, 4, b'1', b'2', b'3', b'4'];
        let c = c_decode_reencode(2, &capability).unwrap();
        assert_eq!(c, capability);
        let mut rust_trace = Sink::default();
        let decoded = CapabilityResponse::decode_with_trace(
            &response(MessageId::Capability, &capability), Some(&mut rust_trace)).unwrap();
        let mut c_trace = Sink::default();
        CapabilityResponse::decode_with_trace(
            &response(MessageId::Capability, &c), Some(&mut c_trace)).unwrap();
        assert_eq!(rust_trace.0, c_trace.0);
        assert_eq!(decoded.chip.unwrap().chip_family, 2);
        assert_eq!(decoded.firmware.unwrap().version, 0x1234_5678);

        let device = [2, 4, 0, 0, 0, 0, 0, 0x10, 8, 0, 8, 7, 6, 5, 4, 3, 2, 1,
            0x11, 4, 0, 0x00, 0x00, 0x20, 0x00];
        let c = c_decode_reencode(3, &device).unwrap();
        assert_eq!(c, device);
        let mut rust_trace = Sink::default();
        let decoded = DeviceInfoResponse::decode_with_trace(
            &response(MessageId::DeviceInfo, &device), Some(&mut rust_trace)).unwrap();
        let mut c_trace = Sink::default();
        DeviceInfoResponse::decode_with_trace(
            &response(MessageId::DeviceInfo, &c), Some(&mut c_trace)).unwrap();
        assert_eq!(rust_trace.0, c_trace.0);
        assert_eq!(decoded.bar_address, Some(0x0102_0304_0506_0708));
        assert_eq!(decoded.bar_size, Some(0x20_0000));

        let memory = [1, 19, 0, 2, 4, 0, 0, 0, 1, 0, 0, 0, 0, 8, 0, 0, 0, 4, 0, 0, 0, 0];
        let c = c_decode_reencode(4, &memory).unwrap();
        assert_eq!(c, memory);
        let mut rust_trace = Sink::default();
        let decoded = RequestMemoryIndication::decode_with_trace(&memory, Some(&mut rust_trace)).unwrap();
        let mut c_trace = Sink::default();
        RequestMemoryIndication::decode_with_trace(&c, Some(&mut c_trace)).unwrap();
        assert_eq!(rust_trace.0, c_trace.0);
        assert_eq!(decoded.segments.len(), 2);
        assert_eq!(decoded.segments[1].kind, MemoryType::CAL);

        for (kind, id) in [(5, MessageId::FirmwareMemoryReady), (6, MessageId::FirmwareReady),
            (7, MessageId::ColdBootCalibrationDone), (8, MessageId::FirmwareInitDone)] {
            assert_eq!(c_decode_reencode(kind, &[]).unwrap(), []);
            assert!(Indication::decode(id, &[]).is_ok());
        }
    }

    #[test]
    fn all_wcn6750_bringup_request_fixtures_match_c() {
        assert_eq!(CapabilityRequest.encode().unwrap().bytes(), c_empty_request(false).unwrap());
        assert_eq!(DeviceInfoRequest.encode().unwrap().bytes(), c_empty_request(true).unwrap());
        let registration = IndicationRegisterRequest::wcn6750();
        assert_eq!(registration.encode().unwrap().bytes(), c_indication_register(&registration).unwrap());
        let host = HostCapabilityRequest { num_clients: Some(1), bdf_support: Some(1),
            m3_support: Some(1), m3_cache_support: Some(1), cal_done: Some(0),
            mem_cfg_mode: Some(0), ..Default::default() };
        assert_eq!(host.encode().unwrap().bytes(), c_host_capability(&host).unwrap());
        let bdf = BdfDownloadRequest { valid: 1, file_id: Some(0), total_size: Some(4),
            segment_id: Some(0), data: Some(vec![1, 2, 3, 4]), end: Some(1), bdf_type: Some(0) };
        assert_eq!(bdf.encode().unwrap().bytes(), c_bdf_download(&bdf).unwrap());
        let memory = RespondMemoryRequest { segments: vec![MemorySegmentResponse {
            address: 0x8800_0000, size: 0x20_0000, kind: MemoryType::DDR, restore: 0 }] };
        assert_eq!(memory.encode().unwrap().bytes(), c_respond_memory(&memory).unwrap());
        let config = WlanConfigRequest { host_version: Some(QmiString::new(b"WIN".to_vec(), 16).unwrap()),
            target_pipes: Some(vec![TargetPipeConfig { pipe_num: 1, direction: PipeDirection::Out,
                entries: 32, max_bytes: 2048, flags: 0 }]),
            service_pipes: Some(vec![ServicePipeConfig { service_id: 0x100, direction: PipeDirection::In,
                pipe_num: 2 }]), shadow_registers: Some(vec![ShadowRegister { id: 3, offset: 0x40 }]),
            shadow_registers_v2: Some(vec![0x1234]) };
        assert_eq!(config.encode().unwrap().bytes(), c_wlan_config(&config).unwrap());
    }
}
