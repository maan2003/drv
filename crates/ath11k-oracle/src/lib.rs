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
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CHttSrng { pdev_id: u8, ring_id: u8, ring_type: u8, entry_words: u8,
    base: u64, head: u64, tail: u64, msi: u64, size_words: u16, batch_words: u16,
    timer: u16, low: u16, msi_data: u32, msi_swap: u8, host_swap: u8,
    tlv_swap: u8, low_enable: u8 }
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CHttRxSelect { pdev_id: u8, ring_id: u8, status_swap: u8, packet_swap: u8,
    buffer_size: u16, tlvs: u32, management_0: u32, management_1: u32, control: u32, data: u32 }
#[repr(C)]
#[cfg(test)]
#[derive(Clone, Copy, Default)]
struct CHttEvent { kind: u8, major: u8, minor: u8, vdev_id: u8, peer_id: u16,
    address: [u8; 6], ast_hash: u16, hardware_peer_id: u16, v2: u8 }
#[repr(C)]
#[cfg(test)]
#[derive(Clone, Copy, Default)]
struct CHttCompletion { status: u8, reinject_reason: u8, ack_rssi: i8,
    peer_valid: u8, peer_id: u16 }
#[repr(C)]
#[cfg(test)]
#[derive(Clone, Copy, Default)]
struct CQcnRx { first_msdu: u8, last_msdu: u8, l3_padding: u8, msdu_done: u8,
    msdu_length_error: u8, fcs_error: u8, decrypt_error: u8, tkip_mic_error: u8,
    multicast_broadcast: u8, decrypted: u8, msdu_length: u16, decap_type: u8,
    ldpc: u8, sgi: u8, mcs: u8, bandwidth: u8, packet_type: u8,
    spatial_stream_bitmap: u8, nss: u8, frequency: u32, tid: u8, peer: u16,
    sequence_valid: u8, frame_valid: u8, sequence_number: u16,
    encryption_valid: u8, encryption_type: u8, phy_ppdu_id: u16 }

#[repr(C)]
#[derive(Clone, Copy)]
struct CWmiCapture { id: u32, len: usize, bytes: [u8; 8192] }
impl Default for CWmiCapture {
    fn default() -> Self { Self { id: 0, len: 0, bytes: [0; 8192] } }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WmiCapture { pub id: u32, pub bytes: Vec<u8> }
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CWmiTlv { tag: u16, len: u16, offset: usize }
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WmiTlv { pub tag: u16, pub len: usize, pub offset: usize }
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WmiEventKind {
    ServiceReady,
    ServiceReadyExt,
    ServiceReadyExt2,
    ServiceAvailable,
    Ready,
    Scan,
    VdevStartResponse,
    VdevStopped,
    VdevDeleteResponse,
    PeerAssocConfirmation,
    PeerDeleteResponse,
    InstallKeyCompletion,
    MgmtRx,
    MgmtTxCompletion,
    FirmwareMemoryDumpComplete,
    RoamCapabilityReport,
    PeerCreateConfirmation,
    WlanFrequencyAvoid,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CWmiTraceField {
    name: u16,
    value: u64,
}
#[repr(C)]
struct CWmiEventParse {
    fields: [u64; 192],
    field_count: usize,
    tlvs: [CWmiTlv; 64],
    tlv_count: usize,
    trace_fields: [CWmiTraceField; 16],
    trace_field_count: usize,
}
impl Default for CWmiEventParse {
    fn default() -> Self {
        Self {
            fields: [0; 192],
            field_count: 0,
            tlvs: [CWmiTlv::default(); 64],
            tlv_count: 0,
            trace_fields: [CWmiTraceField::default(); 16],
            trace_field_count: 0,
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WmiEventTrace {
    Tlv { tag: u16, len: usize, offset: usize },
    Field { name: &'static str, value: u64 },
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WmiEventParse {
    pub fields: Vec<u64>,
    pub trace: Vec<WmiEventTrace>,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CWmiVdevStart {
    restart: u8, hidden: u8, pmf: u8, crypto_disabled: u8,
    vdev_id: u32, beacon_interval: u32, dtim_period: u32, ssid_len: u32,
    ssid: [u8; 32], bcn_tx_rate: u32, noa: u32, tx: u32, rx: u32,
    he_ops: u32, cac: u32, regdomain: u32, mbssid_flags: u32,
    mbssid_tx_vdev_id: u32, channel: [u32; 6],
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CInitChunk { request_id: u32, address: u32, size: u32 }
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CInitBand { pdev_id: u32, start_freq: u32, end_freq: u32 }
#[repr(C)]
#[derive(Clone, Copy)]
struct CWmiInit { words: [u32; 72], chunk_len: u32, chunks: [CInitChunk; 32],
    mode_valid: u8, mode: u32, band_len: u32, bands: [CInitBand; 3] }
impl Default for CWmiInit {
    fn default() -> Self { Self { words: [0; 72], chunk_len: 0,
        chunks: [CInitChunk::default(); 32], mode_valid: 0, mode: 0, band_len: 0,
        bands: [CInitBand::default(); 3] } }
}

unsafe extern "C" {
    fn oracle_qmi_host_cap_encode(input: *const CHostCapability, out: *mut u8, capacity: usize) -> c_int;
    fn oracle_htt_version(out: *mut u8);
    fn oracle_htt_srng_encode(input: *const CHttSrng, out: *mut u8);
    fn oracle_htt_rx_select_encode(input: *const CHttRxSelect, out: *mut u8);
    #[cfg(test)]
    fn oracle_htt_event_decode(bytes: *const u8, len: usize, out: *mut CHttEvent) -> c_int;
    #[cfg(test)]
    fn oracle_htt_completion_decode(bytes: *const u8, len: usize, out: *mut CHttCompletion) -> c_int;
    #[cfg(test)]
    fn oracle_qcn9074_rx_decode(bytes: *const u8, len: usize, out: *mut CQcnRx) -> c_int;
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
    fn oracle_wmi_vdev_id(kind: u32, vdev_id: u32, out: *mut CWmiCapture) -> c_int;
    fn oracle_wmi_pdev_set_param(pdev_id: u32, param_id: u32, value: u32,
        out: *mut CWmiCapture) -> c_int;
    fn oracle_wmi_peer(kind: u32, vdev_id: u32, address: *const u8, peer_type: u32,
        param_id: u32, value: u32, out: *mut CWmiCapture) -> c_int;
    fn oracle_wmi_vdev_up(vdev_id: u32, assoc_id: u32, bssid: *const u8,
        tx_bssid: *const u8, profile_idx: u32, profile_cnt: u32,
        out: *mut CWmiCapture) -> c_int;
    fn oracle_wmi_tlv_iter(bytes: *const u8, length: usize, events: *mut CWmiTlv,
        event_count: *mut usize) -> c_int;
    fn oracle_wmi_vdev_create(vdev_id: u32, kind: u32, subtype: u32,
        address: *const u8, pdev_id: u32, mbssid_flags: u32, mbssid_tx_vdev_id: u32,
        tx2: u32, rx2: u32, tx5: u32, rx5: u32, out: *mut CWmiCapture) -> c_int;
    fn oracle_wmi_vdev_start(input: *const CWmiVdevStart, out: *mut CWmiCapture) -> c_int;
    fn oracle_wmi_scan_stop(requester: u32, scan_id: u32, cancel_type: u32,
        vdev_id: u32, pdev_id: u32, out: *mut CWmiCapture) -> c_int;
    fn oracle_wmi_scan_start(values: *const u32, event_flags: u32, control_inputs: u32,
        adaptive_dwell: u32, mac_addr: *const u8, mac_mask: *const u8,
        channels: *const u32, channel_len: usize, ssid_lengths: *const u8,
        ssids: *const u8, ssid_len: usize, bssids: *const u8, bssid_len: usize,
        extra_ie: *const u8, extra_ie_len: usize, short_hints: *const u32,
        short_hint_len: usize, bssid_hint_freqs: *const u32, bssid_hint_len: usize,
        out: *mut CWmiCapture) -> c_int;
    fn oracle_wmi_install_key(vdev_id: u32, address: *const u8, key_idx: u32,
        key_flags: u32, cipher: u32, rsc_low: u32, rsc_high: u32, key: *const u8,
        key_len: usize, txmic: u32, rxmic: u32, out: *mut CWmiCapture) -> c_int;
    fn oracle_wmi_peer_assoc(values: *const u32, address: *const u8, ppet: *const u32,
        legacy: *const u8, legacy_len: usize, ht: *const u8, ht_len: usize,
        he: *const u32, he_len: usize, flags: u32, out: *mut CWmiCapture) -> c_int;
    fn oracle_wmi_mgmt_send(vdev_id: u32, desc_id: u32, freq: u32, paddr: u64,
        frame: *const u8, frame_len: usize, params_valid: u8, out: *mut CWmiCapture) -> c_int;
    fn oracle_wmi_init(input: *const CWmiInit, out: *mut CWmiCapture) -> c_int;
    fn oracle_wmi_event_parse(
        kind: u32,
        bytes: *const u8,
        length: usize,
        out: *mut CWmiEventParse,
    ) -> c_int;
}

pub fn c_wmi_event_parse(kind: WmiEventKind, bytes: &[u8]) -> Result<WmiEventParse, i32> {
    let mut out = CWmiEventParse::default();
    // SAFETY: `bytes` is readable for `len`, and `out` matches the writable C layout.
    let result =
        unsafe { oracle_wmi_event_parse(kind as u32, bytes.as_ptr(), bytes.len(), &mut out) };
    if result < 0 {
        return Err(result);
    }
    let mut trace = out.tlvs[..out.tlv_count]
        .iter()
        .map(|tlv| WmiEventTrace::Tlv {
            tag: tlv.tag,
            len: usize::from(tlv.len),
            offset: tlv.offset,
        })
        .collect::<Vec<_>>();
    trace.extend(
        out.trace_fields[..out.trace_field_count]
            .iter()
            .map(|field| WmiEventTrace::Field {
                name: wmi_trace_field_name(kind, field.name),
                value: field.value,
            }),
    );
    Ok(WmiEventParse {
        fields: out.fields[..out.field_count].to_vec(),
        trace,
    })
}

fn wmi_trace_field_name(kind: WmiEventKind, id: u16) -> &'static str {
    const NAMES: &[&str] = &[
        "wmi.ServiceReadyFixed.phy_capability",
        "wmi.ServiceReadyFixed.max_supported_macs",
        "wmi.ServiceReadyFixed.num_dbs_hw_modes",
        "wmi.ServiceReadyExt.array_groups.len",
        "wmi.ServiceReadyExt.hw_modes.len",
        "wmi.ServiceReadyExt2.dma_ring_capabilities.len",
        "wmi.Ready.extra_mac_addresses.len",
        "wmi.Ready.status",
        "wmi.Scan.event_type",
        "wmi.Scan.reason",
        "wmi.Scan.channel_freq",
        "wmi.Scan.scan_request_id",
        "wmi.Scan.scan_id",
        "wmi.Scan.vdev_id",
        "wmi.Scan.tsf_timestamp",
        "wmi.VdevStartResponse.vdev_id",
        "wmi.VdevStartResponse.requestor_id",
        "wmi.VdevStartResponse.response_type",
        "wmi.VdevStartResponse.status",
        "wmi.VdevStartResponse.chain_mask",
        "wmi.VdevStartResponse.smps_mode",
        "wmi.VdevStartResponse.mac_id",
        "wmi.VdevStartResponse.configured_tx_streams",
        "wmi.VdevStartResponse.configured_rx_streams",
        "wmi.VdevStartResponse.max_allowed_tx_power",
        "wmi.VdevStopped.vdev_id",
        "wmi.MgmtTxCompletion.descriptor_id",
        "wmi.MgmtTxCompletion.status",
        "wmi.MgmtTxCompletion.pdev_id",
        "wmi.MgmtTxCompletion.ppdu_id",
        "wmi.MgmtTxCompletion.ack_rssi",
        "wmi.FirmwareMemoryDumpComplete.request_id",
        "wmi.FirmwareMemoryDumpComplete.fw_mem_dump_complete",
        "wmi.RoamCapabilityReport.scoring_capability_bitmap",
    ];
    if id == 25 && kind == WmiEventKind::VdevDeleteResponse {
        "wmi.VdevDeleteResponse.vdev_id"
    } else {
        NAMES[usize::from(id)]
    }
}

fn wmi_capture(call: impl FnOnce(*mut CWmiCapture) -> c_int) -> Result<WmiCapture, i32> {
    let mut capture = CWmiCapture::default();
    let result = call(&mut capture);
    if result < 0 { return Err(result); }
    Ok(WmiCapture { id: capture.id, bytes: capture.bytes[..capture.len].to_vec() })
}
pub fn c_wmi_vdev_id(kind: u32, vdev_id: u32) -> Result<WmiCapture, i32> {
    wmi_capture(|out| {
        // SAFETY: `out` points to the writable capture owned by `wmi_capture`.
        unsafe { oracle_wmi_vdev_id(kind, vdev_id, out) }
    })
}
pub fn c_wmi_pdev_set_param(pdev_id: u32, param_id: u32, value: u32) -> Result<WmiCapture, i32> {
    wmi_capture(|out| {
        // SAFETY: `out` points to the writable capture owned by `wmi_capture`.
        unsafe { oracle_wmi_pdev_set_param(pdev_id, param_id, value, out) }
    })
}
pub fn c_wmi_peer(kind: u32, vdev_id: u32, address: &[u8; 6], peer_type: u32,
    param_id: u32, value: u32) -> Result<WmiCapture, i32> {
    wmi_capture(|out| {
        // SAFETY: the address is readable for six bytes and `out` is writable.
        unsafe { oracle_wmi_peer(kind, vdev_id, address.as_ptr(), peer_type, param_id, value, out) }
    })
}
pub fn c_wmi_vdev_up(vdev_id: u32, assoc_id: u32, bssid: &[u8; 6],
    tx_bssid: Option<&[u8; 6]>, profile_idx: u32, profile_cnt: u32) -> Result<WmiCapture, i32> {
    wmi_capture(|out| {
        // SAFETY: both present addresses are readable for six bytes and `out` is writable.
        unsafe { oracle_wmi_vdev_up(vdev_id, assoc_id, bssid.as_ptr(),
            tx_bssid.map_or(core::ptr::null(), |x| x.as_ptr()), profile_idx, profile_cnt, out) }
    })
}
pub fn c_wmi_tlv_iter(bytes: &[u8]) -> Result<Vec<WmiTlv>, i32> {
    let mut events = [CWmiTlv::default(); 64];
    let mut count = events.len();
    // SAFETY: the input is readable and the event allocation is writable for
    // the supplied lengths; C checks its event capacity before every write.
    let result = unsafe { oracle_wmi_tlv_iter(bytes.as_ptr(), bytes.len(),
        events.as_mut_ptr(), &mut count) };
    if result < 0 { return Err(result); }
    Ok(events[..count].iter().map(|event| WmiTlv { tag: event.tag,
        len: usize::from(event.len), offset: event.offset }).collect())
}
#[allow(clippy::too_many_arguments)]
pub fn c_wmi_vdev_create(vdev_id: u32, kind: u32, subtype: u32, address: &[u8; 6],
    pdev_id: u32, mbssid_flags: u32, mbssid_tx_vdev_id: u32,
    tx2: u32, rx2: u32, tx5: u32, rx5: u32) -> Result<WmiCapture, i32> {
    wmi_capture(|out| {
        // SAFETY: `address` is readable for six bytes and `out` is writable.
        unsafe { oracle_wmi_vdev_create(vdev_id, kind, subtype, address.as_ptr(), pdev_id,
            mbssid_flags, mbssid_tx_vdev_id, tx2, rx2, tx5, rx5, out) }
    })
}
pub fn c_wmi_vdev_start(input: &ath11k_wmi::cmd::VdevStart) -> Result<WmiCapture, i32> {
    let mut c = CWmiVdevStart { restart: u8::from(input.restart), hidden: u8::from(input.hidden_ssid),
        pmf: u8::from(input.pmf_enabled), crypto_disabled: u8::from(input.hw_crypto_disabled),
        vdev_id: input.vdev_id, beacon_interval: input.beacon_interval, dtim_period: input.dtim_period,
        ssid_len: input.ssid.as_ref().map_or(0, |ssid| ssid.len() as u32), bcn_tx_rate: input.bcn_tx_rate,
        noa: input.num_noa_descriptors, tx: input.preferred_tx_streams, rx: input.preferred_rx_streams,
        he_ops: input.he_ops, cac: input.cac_duration_ms, regdomain: input.regdomain,
        mbssid_flags: input.mbssid_flags, mbssid_tx_vdev_id: input.mbssid_tx_vdev_id,
        channel: [input.channel.mhz, input.channel.band_center_freq1, input.channel.band_center_freq2,
            input.channel.info, input.channel.reg_info_1, input.channel.reg_info_2], ..Default::default() };
    if let Some(ssid) = &input.ssid { c.ssid[..ssid.len()].copy_from_slice(ssid); }
    wmi_capture(|out| {
        // SAFETY: both pointers refer to valid matching C-layout objects.
        unsafe { oracle_wmi_vdev_start(&c, out) }
    })
}
pub fn c_wmi_scan_stop(requester: u32, scan_id: u32, cancel_type: u32,
    vdev_id: u32, pdev_id: u32) -> Result<WmiCapture, i32> {
    wmi_capture(|out| {
        // SAFETY: `out` points to the writable capture owned by `wmi_capture`.
        unsafe { oracle_wmi_scan_stop(requester, scan_id, cancel_type, vdev_id, pdev_id, out) }
    })
}
pub fn c_wmi_scan_start(input: &ath11k_wmi::cmd::ScanStart) -> Result<WmiCapture, i32> {
    let v = [input.scan_id, input.scan_requester_id, input.vdev_id, input.scan_priority,
        input.notify_scan_events, input.dwell_time_active, input.dwell_time_passive,
        input.min_rest_time, input.max_rest_time, input.repeat_probe_time,
        input.probe_spacing_time, input.idle_time, input.max_scan_time, input.probe_delay,
        input.burst_duration, input.n_probes, input.control_flags_ext,
        input.dwell_time_active_2ghz, input.dwell_time_active_6ghz,
        input.dwell_time_passive_6ghz];
    let e = input.event_flags;
    let event_flags = [e.started, e.completed, e.bss_channel, e.foreign_channel, e.dequeued,
        e.preempted, e.start_failed, e.restarted, e.foreign_channel_exit, e.suspended, e.resumed]
        .iter().enumerate().fold(0, |bits, (i, set)| bits | (u32::from(*set) << i));
    let c = input.control_flags;
    let control_inputs = [c.passive, c.strict_passive, c.promiscuous, c.capture_phy_error,
        c.half_rate, c.quarter_rate, c.cck_rates, c.ofdm_rates, c.channel_stat_event,
        c.filter_probe_request, c.broadcast_probe, c.offchannel_mgmt_tx, c.offchannel_data_tx,
        c.force_active_dfs, c.add_tpc_ie, c.add_ds_ie, c.spoofed_mac, c.random_sequence,
        c.ie_whitelist].iter().enumerate().fold(0, |bits, (i, set)|
            bits | (u32::from(*set) << i));
    let ssid_lengths = input.ssids.iter().map(|ssid| ssid.len() as u8).collect::<Vec<_>>();
    let mut ssids = vec![0; input.ssids.len() * 32];
    for (slot, ssid) in ssids.chunks_exact_mut(32).zip(&input.ssids) {
        slot[..ssid.len()].copy_from_slice(ssid);
    }
    let bssids = input.bssids.iter().flatten().copied().collect::<Vec<_>>();
    let short_hints = input.short_ssid_hints.iter().flat_map(|hint|
        [hint.freq_flags, hint.short_ssid]).collect::<Vec<_>>();
    let bssid_hint_freqs = input.bssid_hints.iter().map(|hint| hint.freq_flags).collect::<Vec<_>>();
    wmi_capture(|out| {
        // SAFETY: all slices remain alive and readable for the supplied lengths.
        unsafe { oracle_wmi_scan_start(v.as_ptr(), event_flags, control_inputs,
            c.adaptive_dwell_mode, input.mac_addr.as_ptr(), input.mac_mask.as_ptr(),
            input.channels.as_ptr(), input.channels.len(), ssid_lengths.as_ptr(), ssids.as_ptr(),
            input.ssids.len(), bssids.as_ptr(), input.bssids.len(), input.extra_ie.as_ptr(),
            input.extra_ie.len(), short_hints.as_ptr(), input.short_ssid_hints.len(),
            bssid_hint_freqs.as_ptr(), input.bssid_hints.len(), out) }
    })
}
pub fn c_wmi_install_key(input: &ath11k_wmi::cmd::VdevInstallKey) -> Result<WmiCapture, i32> {
    wmi_capture(|out| {
        // SAFETY: address/key are readable for their stated lengths and `out` is writable.
        unsafe { oracle_wmi_install_key(input.vdev_id, input.peer_addr.as_ptr(), input.key_idx,
            input.key_flags, input.key_cipher, input.key_rsc_counter.low,
            input.key_rsc_counter.high, input.key_data.as_ptr(), input.key_data.len(),
            input.key_txmic_len, input.key_rxmic_len, out) }
    })
}
pub fn c_wmi_peer_assoc(input: &ath11k_wmi::cmd::PeerAssoc) -> Result<WmiCapture, i32> {
    let p = &input.params;
    let values = [p.vdev_id, p.peer_new_assoc, p.peer_associd, p.peer_rate_caps,
        p.peer_caps, p.peer_listen_intval, p.peer_ht_caps, p.peer_max_mpdu,
        p.peer_mpdu_density, p.peer_vht_caps, p.peer_phymode, p.peer_nss,
        p.peer_bw_rxnss_override, p.rx_max_rate, p.rx_mcs_set, p.tx_max_rate,
        p.tx_mcs_set, u32::from(p.min_data_rate), p.peer_he_cap_macinfo[0],
        p.peer_he_cap_macinfo[1], p.peer_he_cap_macinfo_internal, p.peer_he_caps_6ghz,
        p.peer_he_ops, p.peer_he_cap_phyinfo[0], p.peer_he_cap_phyinfo[1],
        p.peer_he_cap_phyinfo[2], p.peer_ppet.numss_m1, p.peer_ppet.ru_bit_mask];
    let bools = [p.vht_capable, p.is_pmf_enabled, p.is_wme_set, p.qos_flag, p.apsd_flag,
        p.ht_flag, p.bw_40, p.bw_80, p.bw_160, p.stbc_flag, p.ldpc_flag,
        p.static_mimops_flag, p.dynamic_mimops_flag, p.spatial_mux_flag, p.vht_flag,
        p.he_flag, p.twt_requester, p.twt_responder, p.auth_flag, p.need_ptk_4_way,
        p.need_gtk_2_way, p.safe_mode_enabled, p.is_assoc, input.hw_crypto_disabled];
    let flags = bools.iter().enumerate().fold(0, |bits, (index, set)|
        bits | (u32::from(*set) << index));
    let he = p.peer_he_mcs.iter().flat_map(|rate|
        [rate.rx_mcs_set, rate.tx_mcs_set]).collect::<Vec<_>>();
    wmi_capture(|out| {
        // SAFETY: all slices remain alive and readable for the supplied lengths.
        unsafe { oracle_wmi_peer_assoc(values.as_ptr(), p.peer_mac.as_ptr(),
            p.peer_ppet.ppet16_ppet8_ru3_ru0.as_ptr(), p.peer_legacy_rates.as_ptr(),
            p.peer_legacy_rates.len(), p.peer_ht_rates.as_ptr(), p.peer_ht_rates.len(),
            he.as_ptr(), p.peer_he_mcs.len(), flags, out) }
    })
}
pub fn c_wmi_mgmt_send(input: &ath11k_wmi::cmd::MgmtSend) -> Result<WmiCapture, i32> {
    wmi_capture(|out| {
        // SAFETY: frame is readable for its stated length and `out` is writable.
        unsafe { oracle_wmi_mgmt_send(input.vdev_id, input.desc_id, input.channel_freq,
            input.paddr, input.frame.as_ptr(), input.frame.len(), u8::from(input.tx_params_valid), out) }
    })
}
pub fn c_wmi_init(input: &ath11k_wmi::cmd::Init) -> Result<WmiCapture, i32> {
    let mut c = CWmiInit { words: input.resource_config.words(),
        chunk_len: input.memory_chunks.len() as u32, mode_valid: u8::from(input.hardware_mode.is_some()),
        mode: input.hardware_mode.unwrap_or(0), band_len: input.bands.len() as u32, ..Default::default() };
    for (out, chunk) in c.chunks.iter_mut().zip(&input.memory_chunks) {
        *out = CInitChunk { request_id: chunk.request_id, address: chunk.physical_address, size: chunk.size };
    }
    for (out, band) in c.bands.iter_mut().zip(&input.bands) {
        *out = CInitBand { pdev_id: band.pdev_id, start_freq: band.start_freq, end_freq: band.end_freq };
    }
    wmi_capture(|out| {
        // SAFETY: both pointers refer to valid matching C-layout objects.
        unsafe { oracle_wmi_init(&c, out) }
    })
}

pub fn c_htt_version() -> [u8; 4] {
    let mut bytes = [0; 4];
    // SAFETY: the output is writable for the fixed four-byte message.
    unsafe { oracle_htt_version(bytes.as_mut_ptr()) };
    bytes
}
pub fn c_htt_srng(input: ath11k_dp::htt::SrngSetup) -> [u8; 52] {
    let flags = input.flags;
    let c = CHttSrng { pdev_id: input.pdev_id, ring_id: input.ring_id as u8,
        ring_type: input.ring_type as u8, entry_words: input.ring_entry_size_words,
        base: input.ring_base_address, head: input.head_address, tail: input.tail_address,
        msi: input.msi_address, size_words: input.ring_size_words,
        batch_words: input.interrupt_batch_threshold_words, timer: input.interrupt_timer_threshold,
        low: input.interrupt_low_threshold, msi_data: input.msi_data,
        msi_swap: u8::from(flags.msi_swap), host_swap: u8::from(flags.host_firmware_swap),
        tlv_swap: u8::from(flags.tlv_swap), low_enable: u8::from(flags.low_threshold_interrupt) };
    let mut bytes = [0; 52];
    // SAFETY: input/output are valid matching fixed-size C-layout objects.
    unsafe { oracle_htt_srng_encode(&c, bytes.as_mut_ptr()) };
    bytes
}
pub fn c_htt_rx_selection(input: ath11k_dp::htt::RxRingSelection) -> [u8; 28] {
    let f = input.filter;
    let c = CHttRxSelect { pdev_id: input.pdev_id, ring_id: input.ring_id as u8,
        status_swap: u8::from(input.status_swap), packet_swap: u8::from(input.packet_swap),
        buffer_size: input.buffer_size, tlvs: f.tlvs, management_0: f.management_0,
        management_1: f.management_1, control: f.control, data: f.data };
    let mut bytes = [0; 28];
    // SAFETY: input/output are valid matching fixed-size C-layout objects.
    unsafe { oracle_htt_rx_select_encode(&c, bytes.as_mut_ptr()) };
    bytes
}
#[cfg(test)]
fn c_htt_event(bytes: &[u8]) -> Result<CHttEvent, i32> {
    let mut out = CHttEvent::default();
    // SAFETY: input is readable for its length and output is a valid C object.
    let result = unsafe { oracle_htt_event_decode(bytes.as_ptr(), bytes.len(), &mut out) };
    if result < 0 { Err(result) } else { Ok(out) }
}
#[cfg(test)]
fn c_htt_completion(bytes: &[u8]) -> Result<CHttCompletion, i32> {
    let mut out = CHttCompletion::default();
    // SAFETY: input is readable for its length and output is a valid C object.
    let result = unsafe { oracle_htt_completion_decode(bytes.as_ptr(), bytes.len(), &mut out) };
    if result < 0 { Err(result) } else { Ok(out) }
}
#[cfg(test)]
fn c_qcn_rx(bytes: &[u8]) -> Result<CQcnRx, i32> {
    let mut out = CQcnRx::default();
    // SAFETY: input is readable for its length and output is a valid C object.
    let result = unsafe { oracle_qcn9074_rx_decode(bytes.as_ptr(), bytes.len(), &mut out) };
    if result < 0 { Err(result) } else { Ok(out) }
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
    use ath11k_wmi::cmd::{CommandStrategy, EncodeCommand, Init, PdevSetParam, PeerAssoc,
        PeerAuthorize, PeerCreate, PeerDelete, PeerSetParam, ScanStart,
        Channel as WmiChannel, KeySeqCounter, MgmtSend, ScanCancelType, ScanStop, TxRxStreams, VdevCreate,
        VdevDelete, VdevDown, VdevInstallKey, VdevStart, VdevStop, VdevUp};
    use ath11k_wmi::trace::{TraceEvent as WmiTraceEvent, TraceSink as WmiTraceSink};
    use ath11k_wmi::event::{
        Decoder as WmiDecoder, InstallKeyCompletion, MgmtRx, MgmtTxCompletion,
        FirmwareMemoryDumpComplete, PeerAssocConfirmation, PeerCreateConfirmation,
        PeerDeleteResponse, ReadyDecoder, RoamCapabilityReport, Scan, ServiceAvailable,
        ServiceReadyDecoder, ServiceReadyExt2Decoder, ServiceReadyExtDecoder, VdevDeleteResponse,
        VdevStartResponse, VdevStopped, WlanFrequencyAvoid,
    };
    use ath11k_dp::htt::{HttEvent, RxRingFilter, RxRingSelection, SrngFlags, SrngRingId,
        SrngRingType, SrngSetup, TxCompletion, version_request};
    use ath11k_dp::{HttTargetMessage, PeerId};
    use ath11k_dp::rx::{WCN6750_RX_DESCRIPTOR_BYTES, Wcn6750RxDescriptor};

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

    fn rust_wmi(command: &impl EncodeCommand) -> WmiCapture {
        let command = command.encode_command().unwrap();
        WmiCapture { id: command.id.0, bytes: command.tlvs().to_vec() }
    }
    #[derive(Default)]
    struct WmiSink(Vec<WmiTraceEvent>);
    impl WmiTraceSink for WmiSink {
        fn record(&mut self, event: WmiTraceEvent) { self.0.push(event); }
    }
    fn assert_wmi(command: &impl EncodeCommand, c: WmiCapture) {
        let mut sink = WmiSink::default();
        let command = command.encode_command_with_trace(&mut sink).unwrap();
        let rust = WmiCapture { id: command.id.0, bytes: command.tlvs().to_vec() };
        assert_eq!(rust, c);
        let rust_tlvs: Vec<_> = sink.0.into_iter().filter_map(|event| match event {
            WmiTraceEvent::Tlv { tag, len, offset } => Some(WmiTlv { tag, len, offset }),
            WmiTraceEvent::Reject { .. } => panic!("valid command produced rejection trace"),
            WmiTraceEvent::Field { .. } | WmiTraceEvent::Branch { .. } => None,
        }).collect();
        assert_eq!(rust_tlvs, c_wmi_tlv_iter(&c.bytes).unwrap());
    }

    fn words(values: impl IntoIterator<Item = u32>) -> Vec<u8> {
        values.into_iter().flat_map(u32::to_le_bytes).collect()
    }
    fn event_tlv(tag: u16, value: &[u8]) -> Vec<u8> {
        assert_eq!(value.len() % 4, 0);
        let mut out = words([u32::from(tag) << 16 | value.len() as u32]);
        out.extend_from_slice(value);
        out
    }
    fn event(id: ath11k_wmi::EventId, bytes: &[u8]) -> ath11k_wmi::Event {
        ath11k_wmi::Event::from_tlvs(id, bytes.to_vec()).unwrap()
    }
    fn mac_value(mac: [u8; 6]) -> u64 {
        mac.into_iter()
            .enumerate()
            .fold(0, |value, (i, byte)| value | (u64::from(byte) << (8 * i)))
    }
    fn assert_event(kind: WmiEventKind, bytes: &[u8], fields: Vec<u64>, trace: WmiSink) {
        let rust_trace = trace
            .0
            .into_iter()
            .filter_map(|event| match event {
                WmiTraceEvent::Tlv { tag, len, offset } => {
                    Some(WmiEventTrace::Tlv { tag, len, offset })
                }
                WmiTraceEvent::Field { name, value } => Some(WmiEventTrace::Field { name, value }),
                WmiTraceEvent::Branch { .. } => None,
                WmiTraceEvent::Reject { .. } => panic!("well-formed fixture was rejected"),
            })
            .collect::<Vec<_>>();
        let c = c_wmi_event_parse(kind, bytes).unwrap();
        assert_eq!(c.fields, fields, "typed fields for {kind:?}");
        assert_eq!(c.trace, rust_trace, "normalized trace for {kind:?}");
    }

    #[test]
    fn client_bringup_lifecycle_events_match_c() {
        use ath11k_wmi::tags::*;
        let fixed_words: Vec<u32> = (1..=32).collect();
        let mut bytes = event_tlv(WMI_TAG_SERVICE_READY_EVENT.0, &words(fixed_words));
        bytes.extend(event_tlv(WMI_TAG_ARRAY_UINT32.0, &words(101..=132)));
        let mut trace = WmiSink::default();
        let decoded = ServiceReadyDecoder
            .decode_with_trace(event(WMI_SERVICE_READY_EVENTID, &bytes), Some(&mut trace))
            .unwrap();
        let fixed = decoded.fixed.unwrap();
        let mut fields = vec![1, u64::from(fixed.firmware_build)];
        fields.extend(fixed.firmware_abi.map(u64::from));
        fields.extend(
            [
                fixed.phy_capability,
                fixed.max_fragment_entries,
                fixed.num_rf_chains,
                fixed.ht_capability,
                fixed.vht_capability,
                fixed.vht_supported_mcs,
                fixed.hw_min_tx_power,
                fixed.hw_max_tx_power,
                fixed.system_capability,
                fixed.max_beacon_ie_size,
                fixed.num_memory_requests,
                fixed.max_scan_channels,
                fixed.max_supported_macs,
                fixed.firmware_subfeature_caps,
                fixed.num_dbs_hw_modes,
                fixed.txrx_chainmask,
                fixed.default_dbs_hw_mode_index,
                fixed.num_msdu_descriptors,
            ]
            .map(u64::from),
        );
        fields.push(1);
        fields.extend(decoded.service_bitmap.unwrap().map(u64::from));
        assert_event(WmiEventKind::ServiceReady, &bytes, fields, trace);

        let mut bytes = event_tlv(WMI_TAG_SERVICE_READY_EXT_EVENT.0, &words(201..=219));
        bytes.extend(event_tlv(
            WMI_TAG_SOC_MAC_PHY_HW_MODE_CAPS.0,
            &words([1, 0]),
        ));
        bytes.extend(event_tlv(WMI_TAG_SOC_HAL_REG_CAPABILITIES.0, &words([1])));
        let hw_mode = event_tlv(WMI_TAG_HW_MODE_CAPABILITIES.0, &words([11, 3, 7]));
        bytes.extend(event_tlv(WMI_TAG_ARRAY_STRUCT.0, &hw_mode));
        let mut trace = WmiSink::default();
        let decoded = ServiceReadyExtDecoder
            .decode_with_trace(
                event(WMI_SERVICE_READY_EXT_EVENTID, &bytes),
                Some(&mut trace),
            )
            .unwrap();
        let fixed = decoded.fixed.unwrap();
        let mut fields = vec![
            1,
            fixed.default_concurrent_scan_config.into(),
            fixed.default_firmware_config.into(),
        ];
        fields.extend(fixed.ppe_threshold.map(u64::from));
        fields.extend(
            [
                fixed.he_capability,
                fixed.mpdu_density,
                fixed.max_bssid_rx_filters,
                fixed.firmware_build_ext,
                fixed.max_nlo_ssids,
                fixed.max_bssid_indicator,
                fixed.he_capability_ext,
            ]
            .map(u64::from),
        );
        fields.extend([
            1,
            decoded.num_hw_modes.unwrap().into(),
            1,
            decoded.num_phys.unwrap().into(),
        ]);
        for mode in &decoded.hw_modes {
            fields.extend([
                u64::from(mode.hw_mode_id),
                u64::from(mode.phy_id_map),
                u64::from(mode.config_type),
            ]);
        }
        fields.extend([
            decoded.array_groups.len() as u64,
            decoded.hw_modes.len() as u64,
        ]);
        assert_event(WmiEventKind::ServiceReadyExt, &bytes, fields, trace);

        let ring = event_tlv(WMI_TAG_DMA_RING_CAPABILITIES.0, &words([4, 1, 32, 2048, 8]));
        let bytes = event_tlv(WMI_TAG_ARRAY_STRUCT.0, &ring);
        let mut trace = WmiSink::default();
        let decoded = ServiceReadyExt2Decoder
            .decode_with_trace(
                event(WMI_SERVICE_READY_EXT2_EVENTID, &bytes),
                Some(&mut trace),
            )
            .unwrap();
        let mut fields = vec![decoded.dma_ring_capabilities.len() as u64];
        for cap in &decoded.dma_ring_capabilities {
            fields.extend(
                cap.value
                    .chunks_exact(4)
                    .map(|v| u64::from(u32::from_le_bytes(v.try_into().unwrap()))),
            );
        }
        assert_event(WmiEventKind::ServiceReadyExt2, &bytes, fields, trace);

        let mut bytes = event_tlv(WMI_TAG_SERVICE_AVAILABLE_EVENT.0, &words([64, 1, 2, 3, 4]));
        bytes.extend(event_tlv(WMI_TAG_ARRAY_UINT32.0, &words([5, 6, 7, 8])));
        let mut trace = WmiSink::default();
        let decoded = WmiDecoder::<ServiceAvailable>::new(WMI_SERVICE_AVAILABLE_EVENTID)
            .decode_with_trace(
                event(WMI_SERVICE_AVAILABLE_EVENTID, &bytes),
                Some(&mut trace),
            )
            .unwrap();
        let mut fields = vec![decoded.segment_offset.into()];
        fields.extend(decoded.bitmap.map(u64::from));
        fields.push(1);
        fields.extend(decoded.ext2_bitmap.unwrap().map(u64::from));
        assert_event(WmiEventKind::ServiceAvailable, &bytes, fields, trace);

        let primary = [2, 4, 6, 8, 10, 12];
        let extras = [[1, 3, 5, 7, 9, 11], [12, 10, 8, 6, 4, 2]];
        let mut ready = vec![0; 60];
        ready[24..30].copy_from_slice(&primary);
        ready[32..36].copy_from_slice(&9u32.to_le_bytes());
        ready[40..44].copy_from_slice(&2u32.to_le_bytes());
        ready[56..60].copy_from_slice(&0xaabb_ccddu32.to_le_bytes());
        let mut bytes = event_tlv(WMI_TAG_READY_EVENT.0, &ready);
        bytes.extend(event_tlv(
            WMI_TAG_ARRAY_FIXED_STRUCT.0,
            &extras
                .into_iter()
                .flat_map(|mac| mac.into_iter().chain([0, 0]))
                .collect::<Vec<_>>(),
        ));
        let mut trace = WmiSink::default();
        let decoded = ReadyDecoder
            .decode_with_trace(event(WMI_READY_EVENTID, &bytes), Some(&mut trace))
            .unwrap();
        let fields = vec![
            1,
            mac_value(decoded.mac_addr.unwrap()),
            decoded.status.unwrap().into(),
            1,
            decoded.pktlog_defs_checksum.unwrap().into(),
            decoded.extra_mac_addresses.len() as u64,
            mac_value(decoded.extra_mac_addresses[0]),
            mac_value(decoded.extra_mac_addresses[1]),
        ];
        assert_event(WmiEventKind::Ready, &bytes, fields, trace);
    }

    #[test]
    fn client_bringup_operation_events_match_c() {
        use ath11k_wmi::tags::*;
        macro_rules! fixed_event {
            ($kind:ident, $id:ident, $tag:ident, $ty:ty, $values:expr, $fields:expr) => {{
                let bytes = event_tlv($tag.0, &words($values));
                let mut trace = WmiSink::default();
                let value = WmiDecoder::<$ty>::new($id)
                    .decode_with_trace(event($id, &bytes), Some(&mut trace))
                    .unwrap();
                assert_event(WmiEventKind::$kind, &bytes, ($fields)(value), trace);
            }};
        }
        fixed_event!(
            Scan,
            WMI_SCAN_EVENTID,
            WMI_TAG_SCAN_EVENT,
            Scan,
            [1, 2, 5180, 4, 5, 6, 7],
            |v: Scan| vec![
                v.event_type,
                v.reason,
                v.channel_freq,
                v.scan_request_id,
                v.scan_id,
                v.vdev_id,
                v.tsf_timestamp
            ]
            .into_iter()
            .map(u64::from)
            .collect()
        );
        fixed_event!(
            VdevStartResponse,
            WMI_VDEV_START_RESP_EVENTID,
            WMI_TAG_VDEV_START_RESPONSE_EVENT,
            VdevStartResponse,
            [1, 2, 3, 4, 5, 6, 7, 8, 9, 0xffff_ffd0],
            |v: VdevStartResponse| vec![
                v.vdev_id,
                v.requestor_id,
                v.response_type,
                v.status,
                v.chain_mask,
                v.smps_mode,
                v.mac_id,
                v.configured_tx_streams,
                v.configured_rx_streams,
                v.max_allowed_tx_power
            ]
            .into_iter()
            .map(u64::from)
            .collect()
        );
        fixed_event!(
            VdevStopped,
            WMI_VDEV_STOPPED_EVENTID,
            WMI_TAG_VDEV_STOPPED_EVENT,
            VdevStopped,
            [17],
            |v: VdevStopped| vec![v.vdev_id.into()]
        );
        fixed_event!(
            VdevDeleteResponse,
            WMI_VDEV_DELETE_RESP_EVENTID,
            WMI_TAG_VDEV_DELETE_RESP_EVENT,
            VdevDeleteResponse,
            [18],
            |v: VdevDeleteResponse| vec![v.vdev_id.into()]
        );

        let peer = [0, 17, 34, 51, 68, 85];
        for (kind, id, tag) in [
            (
                WmiEventKind::PeerAssocConfirmation,
                WMI_PEER_ASSOC_CONF_EVENTID,
                WMI_TAG_PEER_ASSOC_CONF_EVENT,
            ),
            (
                WmiEventKind::PeerDeleteResponse,
                WMI_PEER_DELETE_RESP_EVENTID,
                WMI_TAG_PEER_DELETE_RESP_EVENT,
            ),
        ] {
            let mut value = words([19]);
            value.extend(peer);
            value.extend([0, 0]);
            let bytes = event_tlv(tag.0, &value);
            let mut trace = WmiSink::default();
            let fields = if kind == WmiEventKind::PeerAssocConfirmation {
                let v = WmiDecoder::<PeerAssocConfirmation>::new(id)
                    .decode_with_trace(event(id, &bytes), Some(&mut trace))
                    .unwrap();
                vec![v.vdev_id.into(), mac_value(v.peer_mac)]
            } else {
                let v = WmiDecoder::<PeerDeleteResponse>::new(id)
                    .decode_with_trace(event(id, &bytes), Some(&mut trace))
                    .unwrap();
                vec![v.vdev_id.into(), mac_value(v.peer_mac)]
            };
            assert_event(kind, &bytes, fields, trace);
        }

        let mut key = words([20]);
        key.extend(peer);
        key.extend([0, 0]);
        key.extend(words([3, 0x40, 0]));
        let bytes = event_tlv(WMI_TAG_VDEV_INSTALL_KEY_COMPLETE_EVENT.0, &key);
        let mut trace = WmiSink::default();
        let v = WmiDecoder::<InstallKeyCompletion>::new(WMI_VDEV_INSTALL_KEY_COMPLETE_EVENTID)
            .decode_with_trace(
                event(WMI_VDEV_INSTALL_KEY_COMPLETE_EVENTID, &bytes),
                Some(&mut trace),
            )
            .unwrap();
        assert_event(
            WmiEventKind::InstallKeyCompletion,
            &bytes,
            vec![
                v.vdev_id.into(),
                mac_value(v.peer_mac),
                v.key_index.into(),
                v.key_flags.into(),
                v.status.into(),
            ],
            trace,
        );

        let frame = [0x40, 0, 1, 2, 3, 4, 5, 6];
        let rx = words([
            2412,
            31,
            6,
            2,
            frame.len() as u32,
            0,
            1,
            2,
            3,
            4,
            0x10,
            (-42i32) as u32,
            77,
            0,
            0,
            1,
            2412,
        ]);
        let mut bytes = event_tlv(WMI_TAG_MGMT_RX_HDR.0, &rx);
        bytes.extend(event_tlv(WMI_TAG_ARRAY_BYTE.0, &frame));
        let mut trace = WmiSink::default();
        let v = WmiDecoder::<MgmtRx>::new(WMI_MGMT_RX_EVENTID)
            .decode_with_trace(event(WMI_MGMT_RX_EVENTID, &bytes), Some(&mut trace))
            .unwrap();
        let mut fields = vec![
            v.channel.into(),
            v.snr.into(),
            v.rate.into(),
            v.phy_mode.into(),
            v.status.into(),
            v.flags.into(),
            u64::from(v.rssi as u32),
            v.tsf_delta.into(),
            v.pdev_id.into(),
            v.channel_freq.into(),
            v.frame.len() as u64,
        ];
        fields.extend(v.frame.iter().copied().map(u64::from));
        assert_event(WmiEventKind::MgmtRx, &bytes, fields, trace);

        fixed_event!(
            MgmtTxCompletion,
            WMI_MGMT_TX_COMPLETION_EVENTID,
            WMI_TAG_MGMT_TX_COMPL_EVENT,
            MgmtTxCompletion,
            [21, 0, 1, 22, 0xffff_ffd8],
            |v: MgmtTxCompletion| vec![v.descriptor_id, v.status, v.pdev_id, v.ppdu_id, v.ack_rssi]
                .into_iter()
                .map(u64::from)
                .collect()
        );
    }

    #[test]
    fn newly_typed_connect_events_match_c() {
        use ath11k_wmi::tags::*;

        for (kind, id, tag, values) in [
            (
                WmiEventKind::FirmwareMemoryDumpComplete,
                WMI_UPDATE_FW_MEM_DUMP_EVENTID,
                WMI_TAG_UPDATE_FW_MEM_DUMP,
                vec![0x1234, 1],
            ),
            (
                WmiEventKind::RoamCapabilityReport,
                WMI_ROAM_CAPABILITY_REPORT_EVENTID,
                WMI_TAG_ROAM_CAPABILITY_REPORT_EVENT,
                vec![0xa5a5_5a5a],
            ),
        ] {
            let bytes = event_tlv(tag.0, &words(values.clone()));
            let mut trace = WmiSink::default();
            let fields = if kind == WmiEventKind::FirmwareMemoryDumpComplete {
                let value = WmiDecoder::<FirmwareMemoryDumpComplete>::new(id)
                    .decode_with_trace(event(id, &bytes), Some(&mut trace))
                    .unwrap();
                vec![value.request_id.into(), value.fw_mem_dump_complete.into()]
            } else {
                let value = WmiDecoder::<RoamCapabilityReport>::new(id)
                    .decode_with_trace(event(id, &bytes), Some(&mut trace))
                    .unwrap();
                vec![value.scoring_capability_bitmap.into()]
            };
            assert_event(kind, &bytes, fields, trace);
        }

        let peer = [0, 17, 34, 51, 68, 85];
        let mut value = words([7]);
        value.extend(peer);
        value.extend([0, 0]);
        value.extend(words([3]));
        let bytes = event_tlv(WMI_TAG_PEER_CREATE_CONF_EVENT.0, &value);
        let mut trace = WmiSink::default();
        let decoded = WmiDecoder::<PeerCreateConfirmation>::new(WMI_PEER_CREATE_CONF_EVENTID)
            .decode_with_trace(event(WMI_PEER_CREATE_CONF_EVENTID, &bytes), Some(&mut trace))
            .unwrap();
        assert_event(
            WmiEventKind::PeerCreateConfirmation,
            &bytes,
            vec![decoded.vdev_id.into(), mac_value(decoded.peer_mac), decoded.status.into()],
            trace,
        );

        let ranges = [(2412, 2437), (5180, 5240)];
        let mut nested = Vec::new();
        for (start, end) in ranges {
            nested.extend(event_tlv(WMI_TAG_AVOID_FREQ_RANGE_DESC.0, &words([start, end])));
        }
        let mut bytes = event_tlv(WMI_TAG_AVOID_FREQ_RANGES_EVENT.0, &words([ranges.len() as u32]));
        bytes.extend(event_tlv(WMI_TAG_ARRAY_STRUCT.0, &nested));
        let mut trace = WmiSink::default();
        let decoded = WmiDecoder::<WlanFrequencyAvoid>::new(WMI_WLAN_FREQ_AVOID_EVENTID)
            .decode_with_trace(event(WMI_WLAN_FREQ_AVOID_EVENTID, &bytes), Some(&mut trace))
            .unwrap();
        let mut fields = vec![decoded.ranges.len() as u64];
        for range in decoded.ranges {
            fields.extend([u64::from(range.start_freq), u64::from(range.end_freq)]);
        }
        assert_event(WmiEventKind::WlanFrequencyAvoid, &bytes, fields, trace);
    }

    proptest! {
        #[test]
        fn htt_host_messages_match_c(pdev_id: u8, ring in 0_u8..8, kind in 0_u8..3,
            base: u64, size_words: u16, entry_words: u8, head: u64, tail: u64,
            msi: u64, msi_data: u32, batch: u16, timer: u16, low: u16,
            msi_swap: bool, host_swap: bool, tlv_swap: bool, low_enable: bool,
            buffer_size: u16, tlvs: u32, management_0: u32, management_1: u32,
            control: u32, data: u32, status_swap: bool, packet_swap: bool) {
            let ring_id = match ring { 0 => SrngRingId::RxdmaHostBuffer,
                1 => SrngRingId::RxdmaMonitorStatus, 2 => SrngRingId::RxdmaMonitorBuffer,
                3 => SrngRingId::RxdmaMonitorDescriptor, 4 => SrngRingId::RxdmaMonitorDestination,
                5 => SrngRingId::Host1ToFirmwareRxBuffer, 6 => SrngRingId::Host2ToFirmwareRxBuffer,
                _ => SrngRingId::RxdmaNonMonitorDestination };
            let ring_type = match kind { 0 => SrngRingType::HardwareToSoftware,
                1 => SrngRingType::SoftwareToHardware, _ => SrngRingType::SoftwareToSoftware };
            let setup = SrngSetup { pdev_id, ring_id, ring_type, ring_base_address: base,
                ring_size_words: size_words, ring_entry_size_words: entry_words,
                head_address: head, tail_address: tail, msi_address: msi, msi_data,
                interrupt_batch_threshold_words: batch, interrupt_timer_threshold: timer,
                interrupt_low_threshold: low, flags: SrngFlags { msi_swap,
                    host_firmware_swap: host_swap, tlv_swap, low_threshold_interrupt: low_enable } };
            prop_assert_eq!(setup.encode().0, c_htt_srng(setup));
            let selection = RxRingSelection { pdev_id, ring_id, status_swap, packet_swap,
                buffer_size, filter: RxRingFilter { tlvs, management_0, management_1, control, data } };
            prop_assert_eq!(selection.encode().0, c_htt_rx_selection(selection));
            prop_assert_eq!(version_request().0, c_htt_version());
        }

        #[test]
        fn htt_target_events_match_c(major: u8, minor: u8, vdev_id: u8, peer_id: u16,
            address: [u8; 6], ast_hash: u16, hardware_peer_id: u16, v2: bool) {
            let version = [0, minor, major, 0];
            let c = c_htt_event(&version).unwrap();
            prop_assert_eq!(HttTargetMessage(version.to_vec()).decode().unwrap(), HttEvent::VersionConfirm { major: c.major, minor: c.minor });
            let mut map = vec![0; 16];
            let type_: u8 = if v2 { 0x1e } else { 3 };
            map[0..4].copy_from_slice(&(u32::from(type_) | (u32::from(vdev_id) << 8) | (u32::from(peer_id) << 16)).to_le_bytes());
            map[4..8].copy_from_slice(&u32::from_le_bytes([address[0],address[1],address[2],address[3]]).to_le_bytes());
            map[8..12].copy_from_slice(&(u32::from_le_bytes([address[4],address[5],0,0]) | (u32::from(hardware_peer_id) << 16)).to_le_bytes());
            map[12..16].copy_from_slice(&u32::from(ast_hash).to_le_bytes());
            let c = c_htt_event(&map).unwrap();
            let HttEvent::PeerMap(rust) = HttTargetMessage(map).decode().unwrap() else { unreachable!() };
            prop_assert_eq!((rust.vdev_id, rust.peer_id.0, rust.address, rust.ast_hash,
                rust.hardware_peer_id, rust.v2), (c.vdev_id, c.peer_id, c.address, c.ast_hash,
                c.hardware_peer_id, c.v2 != 0));
            let mut unmap = vec![0; 12];
            let type_: u8 = if v2 { 0x1f } else { 4 };
            unmap[0..4].copy_from_slice(&(u32::from(type_) | (u32::from(peer_id) << 16)).to_le_bytes());
            let c = c_htt_event(&unmap).unwrap();
            prop_assert_eq!(HttTargetMessage(unmap).decode().unwrap(), HttEvent::PeerUnmap { peer_id: PeerId(c.peer_id), v2: c.v2 != 0 });
        }

        #[test]
        fn htt_tx_completion_fields_match_c(status in 0_u8..16, reason in 0_u8..16,
            ack_rssi: i8, peer in option::of(any::<u16>())) {
            let mut bytes = vec![0; 24];
            bytes[8..12].copy_from_slice(&((u32::from(status) << 9) | (u32::from(reason) << 13)).to_le_bytes());
            bytes[12..16].copy_from_slice(&(u32::from(ack_rssi as u8) << 24).to_le_bytes());
            bytes[16..20].copy_from_slice(&(u32::from(peer.unwrap_or(0)) | if peer.is_some() { 1 << 21 } else { 0 }).to_le_bytes());
            let c = c_htt_completion(&bytes).unwrap();
            let rust = TxCompletion::decode_wbm_release(&bytes).unwrap();
            prop_assert_eq!((rust.status, rust.reinject_reason, rust.ack_rssi, rust.peer.map(|p|p.0)),
                (c.status, c.reinject_reason, c.ack_rssi, if c.peer_valid != 0 { Some(c.peer_id) } else { None }));
        }

        #[test]
        fn qcn9074_rx_ops_selected_by_wcn6750_match_c(end4: u16, attention1: u32,
            attention2: u32, msdu1: u32, msdu2: u32, msdu3: u32, frequency: u32,
            mpdu9: u32, phy_ppdu_id: u16, peer: u16, mpdu11: u32) {
            let mut bytes = vec![0; WCN6750_RX_DESCRIPTOR_BYTES];
            bytes[46..48].copy_from_slice(&end4.to_le_bytes());
            for (offset, value) in [(80, attention1), (84, attention2), (96, msdu1),
                (100, msdu2), (112, msdu3), (120, frequency), (168, mpdu9), (184, mpdu11)] {
                bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            }
            bytes[178..180].copy_from_slice(&phy_ppdu_id.to_le_bytes());
            bytes[182..184].copy_from_slice(&peer.to_le_bytes());
            let rust = Wcn6750RxDescriptor::parse(&bytes).unwrap().status();
            let c = c_qcn_rx(&bytes).unwrap();
            prop_assert_eq!((rust.first_msdu, rust.last_msdu, rust.l3_padding, rust.msdu_done,
                rust.msdu_length_error, rust.fcs_error, rust.decrypt_error, rust.tkip_mic_error),
                (c.first_msdu != 0, c.last_msdu != 0, c.l3_padding, c.msdu_done != 0,
                c.msdu_length_error != 0, c.fcs_error != 0, c.decrypt_error != 0, c.tkip_mic_error != 0));
            prop_assert_eq!((rust.multicast_broadcast, rust.decrypted, rust.msdu_length,
                rust.decap_type, rust.ldpc, rust.short_guard_interval, rust.mcs, rust.bandwidth),
                (c.multicast_broadcast != 0, c.decrypted != 0, c.msdu_length,
                c.decap_type, c.ldpc != 0, c.sgi, c.mcs, c.bandwidth));
            prop_assert_eq!((rust.packet_type, rust.spatial_stream_bitmap, rust.nss,
                rust.frequency, rust.tid, rust.peer.0),
                (c.packet_type, c.spatial_stream_bitmap, c.nss, c.frequency, c.tid, c.peer));
            prop_assert_eq!((rust.sequence_control_valid, rust.frame_control_valid,
                rust.sequence_number, rust.encryption_info_valid, rust.encryption_type,
                rust.phy_ppdu_id), (c.sequence_valid != 0, c.frame_valid != 0,
                c.sequence_number, c.encryption_valid != 0, c.encryption_type, c.phy_ppdu_id));
        }

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

        #[test]
        fn fixed_wmi_command_builders_match_c(vdev_id: u32, pdev_id: u32,
            param_id: u32, value: u32, assoc_id: u32, address: [u8; 6],
            tx_address in option::of(any::<[u8; 6]>()), peer_type: u32,
            profile_idx: u32, profile_cnt: u32, subtype: u32, mbssid_flags: u32,
            mbssid_tx_vdev_id: u32, tx2: u32, rx2: u32, tx5: u32, rx5: u32,
            cancel in 0_u32..3) {
            assert_wmi(&PdevSetParam { pdev_id, param_id, param_value: value },
                c_wmi_pdev_set_param(pdev_id, param_id, value).unwrap());
            assert_wmi(&VdevDelete { vdev_id }, c_wmi_vdev_id(0, vdev_id).unwrap());
            assert_wmi(&VdevStop { vdev_id }, c_wmi_vdev_id(1, vdev_id).unwrap());
            assert_wmi(&VdevDown { vdev_id }, c_wmi_vdev_id(2, vdev_id).unwrap());
            assert_wmi(&PeerCreate { vdev_id, peer_addr: address, peer_type },
                c_wmi_peer(0, vdev_id, &address, peer_type, 0, 0).unwrap());
            assert_wmi(&PeerDelete { vdev_id, peer_addr: address },
                c_wmi_peer(1, vdev_id, &address, 0, 0, 0).unwrap());
            assert_wmi(&PeerSetParam { vdev_id, peer_addr: address, param_id, param_value: value },
                c_wmi_peer(2, vdev_id, &address, 0, param_id, value).unwrap());
            assert_wmi(&VdevUp { vdev_id, assoc_id, bssid: address,
                tx_bssid: tx_address, nontx_profile_idx: profile_idx,
                nontx_profile_cnt: profile_cnt }, c_wmi_vdev_up(vdev_id, assoc_id,
                &address, tx_address.as_ref(), profile_idx, profile_cnt).unwrap());
            assert_wmi(&VdevCreate { vdev_id, vdev_type: peer_type, vdev_subtype: subtype,
                mac_addr: address, pdev_id, mbssid_flags, mbssid_tx_vdev_id,
                band_2ghz: TxRxStreams { tx: tx2, rx: rx2 },
                band_5ghz: TxRxStreams { tx: tx5, rx: rx5 } },
                c_wmi_vdev_create(vdev_id, peer_type, subtype, &address, pdev_id,
                    mbssid_flags, mbssid_tx_vdev_id, tx2, rx2, tx5, rx5).unwrap());
            let cancel_type = match cancel { 0 => ScanCancelType::PdevAll,
                1 => ScanCancelType::VdevAll, _ => ScanCancelType::Single };
            assert_wmi(&ScanStop { requester: param_id, scan_id: value,
                cancel_type, vdev_id, pdev_id },
                c_wmi_scan_stop(param_id, value, cancel, vdev_id, pdev_id).unwrap());
        }

        #[test]
        fn vdev_start_builder_matches_c(restart: bool, vdev_id: u32, beacon_interval: u32,
            dtim_period: u32, hidden_ssid: bool, pmf_enabled: bool, hw_crypto_disabled: bool,
            ssid in option::of(vec(any::<u8>(), 0..=32)), bcn_tx_rate: u32,
            noa: u32, tx: u32, rx: u32, he_ops: u32, cac: u32, regdomain: u32,
            mbssid_flags: u32, mbssid_tx_vdev_id: u32, channel: [u32; 6]) {
            let command = VdevStart { restart, vdev_id, beacon_interval, dtim_period, hidden_ssid,
                pmf_enabled, hw_crypto_disabled, ssid, bcn_tx_rate,
                num_noa_descriptors: noa, preferred_tx_streams: tx, preferred_rx_streams: rx,
                he_ops, cac_duration_ms: cac, regdomain, mbssid_flags, mbssid_tx_vdev_id,
                channel: WmiChannel { mhz: channel[0], band_center_freq1: channel[1],
                    band_center_freq2: channel[2], info: channel[3], reg_info_1: channel[4],
                    reg_info_2: channel[5] } };
            assert_wmi(&command, c_wmi_vdev_start(&command).unwrap());
        }

        #[test]
        fn install_key_builder_matches_c(vdev_id: u32, address: [u8; 6], key_idx: u32,
            key_flags: u32, key_cipher: u32, low: u32, high: u32,
            key_data in vec(any::<u8>(), 0..=256), txmic: u32, rxmic: u32) {
            let command = VdevInstallKey { vdev_id, peer_addr: address, key_idx, key_flags,
                key_cipher, key_rsc_counter: KeySeqCounter { low, high }, key_data,
                key_txmic_len: txmic, key_rxmic_len: rxmic };
            assert_wmi(&command, c_wmi_install_key(&command).unwrap());
        }

        #[test]
        fn peer_assoc_builder_matches_c(command in PeerAssoc::strategy()) {
            assert_wmi(&command, c_wmi_peer_assoc(&command).unwrap());
        }

        #[test]
        fn scan_start_builder_matches_c(command in ScanStart::strategy()) {
            assert_wmi(&command, c_wmi_scan_start(&command).unwrap());
        }

        #[test]
        fn management_send_builder_matches_c(vdev_id: u32, desc_id: u32, freq: u32,
            paddr: u64, frame in vec(any::<u8>(), 0..=512), tx_params_valid: bool) {
            let command = MgmtSend { vdev_id, desc_id, channel_freq: freq, paddr, frame,
                tx_params_valid };
            // The pinned builder deliberately advertises the unpadded frame
            // length while reserving padding, so the generic TLV iterator is
            // not applicable to this command body.
            prop_assert_eq!(rust_wmi(&command), c_wmi_mgmt_send(&command).unwrap());
        }

        #[test]
        fn typed_vdev_create_strategy_matches_c(command in VdevCreate::strategy()) {
            assert_wmi(&command, c_wmi_vdev_create(command.vdev_id, command.vdev_type,
                command.vdev_subtype, &command.mac_addr, command.pdev_id, command.mbssid_flags,
                command.mbssid_tx_vdev_id, command.band_2ghz.tx, command.band_2ghz.rx,
                command.band_5ghz.tx, command.band_5ghz.rx).unwrap());
        }

        #[test]
        fn typed_vdev_start_strategy_matches_c(command in VdevStart::strategy().prop_map(|mut command| {
            command.restart = false;
            command
        })) {
            assert_wmi(&command, c_wmi_vdev_start(&command).unwrap());
        }

        #[test]
        fn typed_vdev_restart_strategy_matches_c(command in VdevStart::strategy().prop_map(|mut command| {
            command.restart = true;
            command
        })) {
            assert_wmi(&command, c_wmi_vdev_start(&command).unwrap());
        }

        #[test]
        fn typed_vdev_up_strategy_matches_c(command in VdevUp::strategy()) {
            assert_wmi(&command, c_wmi_vdev_up(command.vdev_id, command.assoc_id,
                &command.bssid, command.tx_bssid.as_ref(), command.nontx_profile_idx,
                command.nontx_profile_cnt).unwrap());
        }

        #[test]
        fn typed_vdev_down_strategy_matches_c(command in VdevDown::strategy()) {
            assert_wmi(&command, c_wmi_vdev_id(2, command.vdev_id).unwrap());
        }

        #[test]
        fn typed_peer_authorize_strategy_matches_c(command in PeerAuthorize::strategy()) {
            assert_wmi(&command, c_wmi_peer(2, command.vdev_id, &command.peer_addr, 0, 3,
                u32::from(command.authorized)).unwrap());
        }

        #[test]
        fn typed_install_key_strategy_matches_c(command in VdevInstallKey::strategy()) {
            assert_wmi(&command, c_wmi_install_key(&command).unwrap());
        }
    }

    fn response(id: MessageId, body: &[u8]) -> Response {
        Response::checked(TransactionId::new(1), id, body.to_vec()).unwrap()
    }

    #[test]
    fn init_resource_config_generated_messages_match_c() {
        let mut runner = proptest::test_runner::TestRunner::default();
        runner.run(&Init::strategy(), |command| {
            assert_eq!(rust_wmi(&command), c_wmi_init(&command).unwrap());
            Ok(())
        }).unwrap();
    }

    #[test]
    fn init_memory_chunk_trace_matches_c() {
        use ath11k_wmi::cmd::{HostMemoryChunk, ResourceConfig};
        let command = Init { resource_config: ResourceConfig::default(),
            memory_chunks: vec![HostMemoryChunk { request_id: 0, physical_address: 0, size: 0 }],
            hardware_mode: None, bands: vec![] };
        assert_eq!(rust_wmi(&command), c_wmi_init(&command).unwrap());
        let mut sink = WmiSink::default();
        command.encode_command_with_trace(&mut sink).unwrap();
        assert!(!sink.0.iter().any(|event| matches!(event, WmiTraceEvent::Reject { .. })));
    }

    #[test]
    fn wcn6750_htt_bringup_fixtures_match_c() {
        assert_eq!(version_request().0, c_htt_version());
        let setup = SrngSetup { pdev_id: 0, ring_id: SrngRingId::RxdmaHostBuffer,
            ring_type: SrngRingType::SoftwareToHardware, ring_base_address: 0x8800_0000,
            ring_size_words: 4096, ring_entry_size_words: 8, head_address: 0x8810_0000,
            tail_address: 0x8810_0008, msi_address: 0x8820_0000, msi_data: 4,
            interrupt_batch_threshold_words: 32, interrupt_timer_threshold: 8,
            interrupt_low_threshold: 4, flags: SrngFlags { low_threshold_interrupt: true,
                ..Default::default() } };
        assert_eq!(setup.encode().0, c_htt_srng(setup));
        let selection = RxRingSelection { pdev_id: 0, ring_id: SrngRingId::RxdmaHostBuffer,
            status_swap: false, packet_swap: false, buffer_size: 2048,
            filter: RxRingFilter { tlvs: 0x1f, management_0: u32::MAX,
                management_1: u32::MAX, control: u32::MAX, data: u32::MAX } };
        assert_eq!(selection.encode().0, c_htt_rx_selection(selection));
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
