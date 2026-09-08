//! Data-driven native WMI transcript parsing and byte-exact verification.
use crate::{Command, CommandId, Event, EventId, WmiError};
use alloc::{string::String, vec::Vec};
use core::ops::Range;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TranscriptKind {
    Command,
    Event,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranscriptRecord {
    pub seq: u64,
    pub timestamp_ns: u64,
    pub kind: TranscriptKind,
    pub id: u32,
    pub declared_len: usize,
    /// Full tracepoint payload, including the four-byte WMI command header.
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GoldenError {
    MissingField(&'static str),
    InvalidField(&'static str),
    InvalidHex,
    LengthMismatch { declared: usize, actual: usize },
    TruncatedEnvelope,
    IdentifierMismatch { declared: u32, envelope: u32 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ByteMismatch {
    pub first_differing_offset: usize,
    pub expected: Option<u8>,
    pub actual: Option<u8>,
    pub expected_len: usize,
    pub actual_len: usize,
}

/// A command envelope recovered from a native trace record.
///
/// This is deliberately envelope-level verification: each TLV is separated
/// into its tag, declared value, and canonical zero padding, but the value is
/// retained as opaque bytes. It checks command-family dispatch, envelope and
/// TLV headers, lengths, padding, and documented masks. It does **not** verify
/// the field semantics or the existing concrete command-family encoders.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GoldenCommandEnvelope {
    pub family: &'static str,
    tlvs: Vec<GoldenTlv>,
    /// Host-only fields excluded from comparison for this command family.
    pub masked_fields: &'static [&'static str],
    /// Byte ranges in the complete command envelope corresponding to fields
    /// present in this particular request.
    pub masked_ranges: Vec<Range<usize>>,
    id: CommandId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct GoldenTlv {
    tag: u16,
    wire_len: u16,
    value: Vec<u8>,
}

impl crate::cmd::EncodeCommand for GoldenCommandEnvelope {
    fn encode_command(&self) -> Result<Command, WmiError> {
        let mut bytes = Vec::new();
        for tlv in &self.tlvs {
            bytes.extend_from_slice(
                &((u32::from(tlv.tag) << 16) | u32::from(tlv.wire_len)).to_le_bytes(),
            );
            bytes.extend_from_slice(&tlv.value);
            if tlv.value.len() == usize::from(tlv.wire_len) {
                bytes.resize(
                    bytes.len() + (tlv.value.len().next_multiple_of(4) - tlv.value.len()),
                    0,
                );
            }
        }
        Command::from_tlvs(self.id, bytes)
    }
}

const NO_MASKS: &[&str] = &[];
const INIT_MASKS: &[&str] = &["host_memory_chunks[].paddr"];
const MGMT_TX_MASKS: &[&str] = &["paddr", "frame"];
const REORDER_MASKS: &[&str] = &["queue_address"];

fn command_family(id: u32) -> Option<(&'static str, &'static [&'static str])> {
    Some(match id {
        0x000001 => ("init", INIT_MASKS),
        0x003001 => ("scan-start", NO_MASKS),
        0x003002 => ("scan-stop", NO_MASKS),
        0x003003 => ("scan-channel-list", NO_MASKS),
        0x003006 => ("scan-probe-request-oui", NO_MASKS),
        0x004003 => ("pdev-set-param", NO_MASKS),
        0x005001 => ("vdev-create", NO_MASKS),
        0x005002 => ("vdev-delete", NO_MASKS),
        0x005003 => ("vdev-start", NO_MASKS),
        0x005005 => ("vdev-up", NO_MASKS),
        0x005006 => ("vdev-stop", NO_MASKS),
        0x005008 => ("vdev-set-param", NO_MASKS),
        0x005009 => ("vdev-install-key", NO_MASKS),
        0x00500d => ("vdev-wmm-update", NO_MASKS),
        0x006001 => ("peer-create", NO_MASKS),
        0x006002 => ("peer-delete", NO_MASKS),
        0x006004 => ("peer-set-param", NO_MASKS),
        0x006005 => ("peer-assoc", NO_MASKS),
        0x006013 => ("peer-reorder-queue-setup", REORDER_MASKS),
        0x007008 => ("mgmt-tx-send", MGMT_TX_MASKS),
        0x00700c => ("bss-color-change-enable", NO_MASKS),
        0x009001 => ("sta-powersave-mode", NO_MASKS),
        0x009002 => ("sta-powersave-param", NO_MASKS),
        0x00a005 => ("pdev-dfs-phyerr-offload-enable", NO_MASKS),
        0x016001 => ("request-stats", NO_MASKS),
        0x01d010 => ("pdev-lro-config", NO_MASKS),
        0x02a003 => ("obss-color-collision-config", NO_MASKS),
        0x03a001 => ("set-current-country", NO_MASKS),
        0x03a002 => ("11d-scan-start", NO_MASKS),
        0x03a003 => ("11d-scan-stop", NO_MASKS),
        0x040001 => ("obss-spatial-reuse", NO_MASKS),
        _ => return None,
    })
}

/// Reverse maps a command captured by the pinned native ath11k tracepoint.
/// Unknown command IDs are deliberately not guessed.
pub fn reverse_map_command_envelope(
    id: CommandId,
    bytes: &[u8],
) -> Result<Option<GoldenCommandEnvelope>, WmiError> {
    let Some((family, masked_fields)) = command_family(id.0) else {
        return Ok(None);
    };
    let mut tlvs = Vec::new();
    let mut offset = 0usize;
    while offset < bytes.len() {
        let header = bytes.get(offset..offset + 4).ok_or(WmiError::Malformed)?;
        let header = u32::from_le_bytes(header.try_into().map_err(|_| WmiError::Malformed)?);
        let wire_len = (header & 0xffff) as u16;
        let len = usize::from(wire_len);
        // The pinned ath11k scan-channel-list encoder advertises the array as
        // `nested_bytes - TLV_HDR_SIZE`; the production Rust encoder preserves
        // this observable ABI quirk.  Consume the four bytes here rather than
        // misreading the final channel word as a new top-level TLV.
        let consumed_len = if id.0 == 0x003003 && offset != 0 {
            len.checked_add(4).ok_or(WmiError::Malformed)?
        } else {
            len
        };
        let padded = consumed_len.next_multiple_of(4);
        let value = bytes
            .get(offset + 4..offset + 4 + consumed_len)
            .ok_or(WmiError::Malformed)?;
        let padding = bytes
            .get(offset + 4 + consumed_len..offset + 4 + padded)
            .ok_or(WmiError::Malformed)?;
        if padding.iter().any(|byte| *byte != 0) {
            return Err(WmiError::Malformed);
        }
        tlvs.push(GoldenTlv {
            tag: (header >> 16) as u16,
            wire_len,
            value: value.to_vec(),
        });
        offset += 4 + padded;
    }

    let mut masked_ranges = Vec::new();
    if id.0 == 0x007008 {
        // Envelope (4), fixed TLV header (4), then vdev/desc/frequency (12).
        masked_ranges.push(20..28);
        // The byte-array TLV follows the 36-byte fixed TLV.  Its declared
        // value is the downloaded prefix of the host management frame.
        if let Some(frame) = tlvs.get(1) {
            masked_ranges.push(44..44 + frame.value.len());
        }
    } else if id.0 == 0x000001 {
        // A host-memory chunk is encoded as a 16-byte nested TLV whose first
        // eight value bytes are the DMA address.  The redwood golden has no
        // chunks, but keep the rule here so future captures cannot silently
        // compare process-specific addresses.
        let mut top = 4usize;
        for tlv in &tlvs {
            if tlv.tag == 0x12 {
                let mut nested = 0usize;
                while nested + 16 <= tlv.value.len() {
                    let h = u32::from_le_bytes(
                        tlv.value[nested..nested + 4]
                            .try_into()
                            .map_err(|_| WmiError::Malformed)?,
                    );
                    if (h >> 16) as u16 != 0x4c || (h & 0xffff) != 16 {
                        return Err(WmiError::Malformed);
                    }
                    masked_ranges.push(top + 4 + nested + 8..top + 4 + nested + 12);
                    nested += 16;
                }
            }
            top += 4 + tlv.value.len().next_multiple_of(4);
        }
    }
    Ok(Some(GoldenCommandEnvelope {
        family,
        tlvs,
        masked_fields,
        masked_ranges,
        id,
    }))
}

/// Compares command envelopes after applying the reverse mapper's documented
/// host-state masks.
pub fn compare_reencoded(
    expected: &[u8],
    actual: &[u8],
    masks: &[Range<usize>],
) -> Option<ByteMismatch> {
    let mut expected = expected.to_vec();
    let mut actual = actual.to_vec();
    for range in masks {
        let end = range.end.min(expected.len()).min(actual.len());
        if range.start < end {
            expected[range.start..end].fill(0);
            actual[range.start..end].fill(0);
        }
    }
    first_difference(&expected, &actual)
}

/// A native command decoded into the existing high-level request type for its
/// family. Unlike [`GoldenCommandEnvelope`], encoding this value exercises the
/// production family encoder and all of its field-to-wire transformations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GoldenSemanticRequest {
    pub family: &'static str,
    request: SemanticRequest,
    pub masked_fields: &'static [&'static str],
    pub masked_ranges: Vec<Range<usize>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SemanticRequest {
    Init(super::Init),
    PeerAssoc(super::PeerAssoc),
    PeerCreate(super::PeerCreate),
    PeerDelete(super::PeerDelete),
    PeerReorderQueueSetup(super::PeerReorderQueueSetup),
    PeerSetParam(super::PeerSetParam),
    PdevSetParam(super::PdevSetParam),
    ScanChannelList(super::ScanChannelList),
    ScanStart(super::ScanStart),
    ScanStop(super::ScanStop),
    StaPowerSaveMode(super::StaPowerSaveMode),
    StaPowerSaveParameter(super::StaPowerSaveParameter),
    VdevInstallKey(super::VdevInstallKey),
    VdevSetParam(super::VdevSetParam),
    WmmUpdate(super::WmmUpdate),
    VdevCreate(super::VdevCreate),
    VdevDelete(super::VdevDelete),
    VdevStart(super::VdevStart),
    VdevStop(super::VdevStop),
    VdevUp(super::VdevUp),
}

impl crate::cmd::EncodeCommand for GoldenSemanticRequest {
    fn encode_command(&self) -> Result<Command, WmiError> {
        match &self.request {
            SemanticRequest::Init(request) => request.encode_command(),
            SemanticRequest::PeerAssoc(request) => request.encode_command(),
            SemanticRequest::PeerCreate(request) => request.encode_command(),
            SemanticRequest::PeerDelete(request) => request.encode_command(),
            SemanticRequest::PeerReorderQueueSetup(request) => request.encode_command(),
            SemanticRequest::PeerSetParam(request) => request.encode_command(),
            SemanticRequest::PdevSetParam(request) => request.encode_command(),
            SemanticRequest::ScanChannelList(request) => request.encode_command(),
            SemanticRequest::ScanStart(request) => request.encode_command(),
            SemanticRequest::ScanStop(request) => request.encode_command(),
            SemanticRequest::StaPowerSaveMode(request) => request.encode_command(),
            SemanticRequest::StaPowerSaveParameter(request) => request.encode_command(),
            SemanticRequest::VdevInstallKey(request) => request.encode_command(),
            SemanticRequest::VdevSetParam(request) => request.encode_command(),
            SemanticRequest::WmmUpdate(request) => request.encode_command(),
            SemanticRequest::VdevCreate(request) => request.encode_command(),
            SemanticRequest::VdevDelete(request) => request.encode_command(),
            SemanticRequest::VdevStart(request) => request.encode_command(),
            SemanticRequest::VdevStop(request) => request.encode_command(),
            SemanticRequest::VdevUp(request) => request.encode_command(),
        }
    }
}

fn words<const N: usize>(bytes: &[u8]) -> Result<[u32; N], WmiError> {
    if bytes.len() != N * 4 {
        return Err(WmiError::Malformed);
    }
    let mut out = [0; N];
    for (word, bytes) in out.iter_mut().zip(bytes.chunks_exact(4)) {
        *word = u32::from_le_bytes(bytes.try_into().map_err(|_| WmiError::Malformed)?);
    }
    Ok(out)
}

fn mac(bytes: &[u8]) -> Result<[u8; 6], WmiError> {
    bytes.try_into().map_err(|_| WmiError::Malformed)
}

fn semantic_tlvs(id: CommandId, bytes: &[u8]) -> Result<Vec<GoldenTlv>, WmiError> {
    reverse_map_command_envelope(id, bytes)?
        .map(|request| request.tlvs)
        .ok_or(WmiError::Malformed)
}

/// Decodes the native families currently covered by concrete reverse mapping.
/// `Ok(None)` means that the known family has not yet acquired a semantic
/// mapper; malformed bytes in a covered family are always an error.
pub fn reverse_map_semantic_command(
    id: CommandId,
    bytes: &[u8],
) -> Result<Option<GoldenSemanticRequest>, WmiError> {
    let request = match id.0 {
        0x000001 => {
            let tlvs = semantic_tlvs(id, bytes)?;
            if tlvs.len() != 3
                || tlvs[0].tag != crate::tags::WMI_TAG_INIT_CMD.0
                || tlvs[1].tag != crate::tags::WMI_TAG_RESOURCE_CONFIG.0
                || tlvs[2].tag != crate::tags::WMI_TAG_ARRAY_STRUCT.0
            {
                return Err(WmiError::Malformed);
            }
            let init = words::<7>(&tlvs[0].value)?;
            if init[..6] != [0; 6] || init[6] != 0 || !tlvs[2].value.is_empty() {
                return Err(WmiError::Malformed);
            }
            SemanticRequest::Init(super::Init {
                resource_config: super::ResourceConfig::from_words(words::<72>(&tlvs[1].value)?),
                memory_chunks: Vec::new(),
                hardware_mode: None,
                bands: Vec::new(),
            })
        }
        0x005001 => {
            let tlvs = semantic_tlvs(id, bytes)?;
            if tlvs.len() != 2
                || tlvs[0].tag != crate::tags::WMI_TAG_VDEV_CREATE_CMD.0
                || tlvs[1].tag != crate::tags::WMI_TAG_ARRAY_STRUCT.0
            {
                return Err(WmiError::Malformed);
            }
            let fixed = words::<9>(&tlvs[0].value)?;
            let streams = &tlvs[1].value;
            if fixed[5] != 2 || streams.len() != 32 {
                return Err(WmiError::Malformed);
            }
            let band_2ghz = words::<3>(&streams[4..16])?;
            let band_5ghz = words::<3>(&streams[20..32])?;
            if band_2ghz[0] != 0 || band_5ghz[0] != 1 {
                return Err(WmiError::Malformed);
            }
            SemanticRequest::VdevCreate(super::VdevCreate {
                vdev_id: fixed[0],
                vdev_type: fixed[1],
                vdev_subtype: fixed[2],
                mac_addr: mac(&tlvs[0].value[12..18])?,
                pdev_id: fixed[6],
                mbssid_flags: fixed[7],
                mbssid_tx_vdev_id: fixed[8],
                band_2ghz: super::TxRxStreams {
                    tx: band_2ghz[1],
                    rx: band_2ghz[2],
                },
                band_5ghz: super::TxRxStreams {
                    tx: band_5ghz[1],
                    rx: band_5ghz[2],
                },
            })
        }
        0x005002 | 0x005006 => {
            let tlvs = semantic_tlvs(id, bytes)?;
            if tlvs.len() != 1 {
                return Err(WmiError::Malformed);
            }
            let fixed = words::<1>(&tlvs[0].value)?;
            if id.0 == 0x005002 {
                SemanticRequest::VdevDelete(super::VdevDelete { vdev_id: fixed[0] })
            } else {
                SemanticRequest::VdevStop(super::VdevStop { vdev_id: fixed[0] })
            }
        }
        0x005003 => {
            let tlvs = semantic_tlvs(id, bytes)?;
            if tlvs.len() != 3
                || tlvs[0].tag != crate::tags::WMI_TAG_VDEV_START_REQUEST_CMD.0
                || tlvs[1].tag != crate::tags::WMI_TAG_CHANNEL.0
                || tlvs[2].tag != crate::tags::WMI_TAG_ARRAY_STRUCT.0
                || !tlvs[2].value.is_empty()
            {
                return Err(WmiError::Malformed);
            }
            let fixed = words::<26>(&tlvs[0].value)?;
            let channel = words::<6>(&tlvs[1].value)?;
            let ssid_len = usize::try_from(fixed[5]).map_err(|_| WmiError::Malformed)?;
            if fixed[1] != 0 || ssid_len > 32 || fixed[15] != 0 || fixed[17] != 0 || fixed[23] != 0
            {
                return Err(WmiError::Malformed);
            }
            let flags = fixed[4];
            SemanticRequest::VdevStart(super::VdevStart {
                restart: false,
                vdev_id: fixed[0],
                beacon_interval: fixed[2],
                dtim_period: fixed[3],
                hidden_ssid: flags & 1 != 0,
                pmf_enabled: flags & 2 != 0,
                hw_crypto_disabled: flags & (1 << 4) != 0,
                ssid: (ssid_len != 0).then(|| tlvs[0].value[24..24 + ssid_len].to_vec()),
                bcn_tx_rate: fixed[14],
                num_noa_descriptors: fixed[16],
                preferred_tx_streams: fixed[18],
                preferred_rx_streams: fixed[19],
                he_ops: fixed[20],
                cac_duration_ms: fixed[21],
                regdomain: fixed[22],
                mbssid_flags: fixed[24],
                mbssid_tx_vdev_id: fixed[25],
                channel: super::Channel {
                    mhz: channel[0],
                    band_center_freq1: channel[1],
                    band_center_freq2: channel[2],
                    info: channel[3],
                    reg_info_1: channel[4],
                    reg_info_2: channel[5],
                },
            })
        }
        0x005005 => {
            let tlvs = semantic_tlvs(id, bytes)?;
            if tlvs.len() != 1 || tlvs[0].tag != crate::tags::WMI_TAG_VDEV_UP_CMD.0 {
                return Err(WmiError::Malformed);
            }
            let fixed = words::<8>(&tlvs[0].value)?;
            let tx = mac(&tlvs[0].value[16..22])?;
            SemanticRequest::VdevUp(super::VdevUp {
                vdev_id: fixed[0],
                assoc_id: fixed[1],
                bssid: mac(&tlvs[0].value[8..14])?,
                tx_bssid: (tx != [0; 6]).then_some(tx),
                nontx_profile_idx: fixed[6],
                nontx_profile_cnt: fixed[7],
            })
        }
        0x006001 => {
            let tlvs = semantic_tlvs(id, bytes)?;
            if tlvs.len() != 1 || tlvs[0].tag != crate::tags::WMI_TAG_PEER_CREATE_CMD.0 {
                return Err(WmiError::Malformed);
            }
            let fixed = words::<4>(&tlvs[0].value)?;
            SemanticRequest::PeerCreate(super::PeerCreate {
                vdev_id: fixed[0],
                peer_addr: mac(&tlvs[0].value[4..10])?,
                peer_type: fixed[3],
            })
        }
        0x006002 => {
            let tlvs = semantic_tlvs(id, bytes)?;
            if tlvs.len() != 1 || tlvs[0].tag != crate::tags::WMI_TAG_PEER_DELETE_CMD.0 {
                return Err(WmiError::Malformed);
            }
            let fixed = words::<3>(&tlvs[0].value)?;
            SemanticRequest::PeerDelete(super::PeerDelete {
                vdev_id: fixed[0],
                peer_addr: mac(&tlvs[0].value[4..10])?,
            })
        }
        0x006005 => {
            let tlvs = semantic_tlvs(id, bytes)?;
            if tlvs.len() != 5
                || tlvs[0].tag != crate::tags::WMI_TAG_PEER_ASSOC_COMPLETE_CMD.0
                || tlvs[1].tag != crate::tags::WMI_TAG_ARRAY_BYTE.0
                || tlvs[2].tag != crate::tags::WMI_TAG_ARRAY_BYTE.0
                || tlvs[3].tag != crate::tags::WMI_TAG_VHT_RATE_SET.0
                || tlvs[4].tag != crate::tags::WMI_TAG_ARRAY_STRUCT.0
            {
                return Err(WmiError::Malformed);
            }
            let fixed = words::<40>(&tlvs[0].value)?;
            if fixed[15] != 0 || fixed[16] != 0 {
                return Err(WmiError::Malformed);
            }
            let legacy_len = usize::try_from(fixed[17]).map_err(|_| WmiError::Malformed)?;
            let ht_len = usize::try_from(fixed[18]).map_err(|_| WmiError::Malformed)?;
            if legacy_len > tlvs[1].value.len() || ht_len > tlvs[2].value.len() {
                return Err(WmiError::Malformed);
            }
            let vht = words::<5>(&tlvs[3].value)?;
            if vht[4] != 0 || !tlvs[4].value.len().is_multiple_of(12) {
                return Err(WmiError::Malformed);
            }
            let mut peer_he_mcs = Vec::new();
            for rate in tlvs[4].value.chunks_exact(12) {
                let header =
                    u32::from_le_bytes(rate[..4].try_into().map_err(|_| WmiError::Malformed)?);
                if header != (u32::from(crate::tags::WMI_TAG_HE_RATE_SET.0) << 16) | 8 {
                    return Err(WmiError::Malformed);
                }
                let values = words::<2>(&rate[4..])?;
                peer_he_mcs.push(super::PeerHeRateSet {
                    rx_mcs_set: values[0],
                    tx_mcs_set: values[1],
                });
            }
            if peer_he_mcs.len() != usize::try_from(fixed[35]).map_err(|_| WmiError::Malformed)? {
                return Err(WmiError::Malformed);
            }
            let flags = fixed[5];
            let flag = |bit| flags & bit != 0;
            SemanticRequest::PeerAssoc(super::PeerAssoc {
                params: super::PeerAssocParams {
                    vdev_id: fixed[2],
                    peer_new_assoc: fixed[3],
                    peer_associd: fixed[4],
                    peer_mac: mac(&tlvs[0].value[..6])?,
                    peer_rate_caps: fixed[11],
                    peer_caps: fixed[6],
                    peer_listen_intval: fixed[7],
                    peer_ht_caps: fixed[8],
                    peer_max_mpdu: fixed[9],
                    peer_mpdu_density: fixed[10],
                    peer_vht_caps: fixed[13],
                    peer_phymode: fixed[14],
                    peer_nss: fixed[12],
                    peer_bw_rxnss_override: fixed[19],
                    peer_legacy_rates: tlvs[1].value[..legacy_len].to_vec(),
                    peer_ht_rates: tlvs[2].value[..ht_len].to_vec(),
                    vht_capable: vht[..4] != [0; 4],
                    rx_max_rate: vht[2],
                    rx_mcs_set: vht[3],
                    tx_max_rate: vht[0],
                    tx_mcs_set: vht[1],
                    peer_he_mcs,
                    min_data_rate: u8::try_from(fixed[38]).map_err(|_| WmiError::Malformed)?,
                    peer_he_cap_macinfo: [fixed[30], fixed[36]],
                    peer_he_cap_macinfo_internal: fixed[37],
                    peer_he_caps_6ghz: fixed[39],
                    peer_he_ops: fixed[31],
                    peer_he_cap_phyinfo: [fixed[32], fixed[33], fixed[34]],
                    peer_ppet: super::PeerPpeThreshold {
                        numss_m1: fixed[20],
                        ru_bit_mask: fixed[21],
                        ppet16_ppet8_ru3_ru0: fixed[22..30]
                            .try_into()
                            .map_err(|_| WmiError::Malformed)?,
                    },
                    is_pmf_enabled: flag(0x0800_0000),
                    is_wme_set: true,
                    qos_flag: flag(0x0000_0002),
                    apsd_flag: flag(0x0000_0800),
                    ht_flag: flag(0x0000_1000),
                    bw_40: flag(0x0000_2000),
                    bw_80: flag(0x0400_0000),
                    bw_160: flag(0x4000_0000),
                    stbc_flag: flag(0x0000_8000),
                    ldpc_flag: flag(0x0001_0000),
                    static_mimops_flag: flag(0x0004_0000),
                    dynamic_mimops_flag: flag(0x0002_0000),
                    spatial_mux_flag: flag(0x0020_0000),
                    vht_flag: flag(0x0200_0000),
                    he_flag: flag(0x0000_0400),
                    twt_requester: flag(0x0040_0000),
                    twt_responder: flag(0x0080_0000),
                    auth_flag: flag(0x0000_0001),
                    need_ptk_4_way: flag(0x0000_0004),
                    need_gtk_2_way: flag(0x0000_0010),
                    safe_mode_enabled: false,
                    is_assoc: false,
                },
                hw_crypto_disabled: false,
            })
        }
        0x003002 => {
            let tlvs = semantic_tlvs(id, bytes)?;
            if tlvs.len() != 1 || tlvs[0].tag != crate::tags::WMI_TAG_STOP_SCAN_CMD.0 {
                return Err(WmiError::Malformed);
            }
            let fixed = words::<5>(&tlvs[0].value)?;
            let cancel_type = match fixed[2] {
                0 => super::ScanCancelType::Single,
                0x0100_0000 => super::ScanCancelType::VdevAll,
                0x0400_0000 => super::ScanCancelType::PdevAll,
                _ => return Err(WmiError::Malformed),
            };
            SemanticRequest::ScanStop(super::ScanStop {
                requester: fixed[0],
                scan_id: fixed[1],
                cancel_type,
                vdev_id: fixed[3],
                pdev_id: fixed[4],
            })
        }
        0x003003 => {
            let tlvs = semantic_tlvs(id, bytes)?;
            if tlvs.len() != 2
                || tlvs[0].tag != crate::tags::WMI_TAG_SCAN_CHAN_LIST_CMD.0
                || tlvs[1].tag != crate::tags::WMI_TAG_ARRAY_STRUCT.0
            {
                return Err(WmiError::Malformed);
            }
            let fixed = words::<3>(&tlvs[0].value)?;
            let count = usize::try_from(fixed[0]).map_err(|_| WmiError::Malformed)?;
            if count == 0 || tlvs[1].value.len() != count * 28 {
                return Err(WmiError::Malformed);
            }
            let mut channels = Vec::with_capacity(count);
            for channel in tlvs[1].value.chunks_exact(28) {
                let header =
                    u32::from_le_bytes(channel[..4].try_into().map_err(|_| WmiError::Malformed)?);
                if header != (u32::from(crate::tags::WMI_TAG_CHANNEL.0) << 16) | 24 {
                    return Err(WmiError::Malformed);
                }
                let value = words::<6>(&channel[4..])?;
                let info = value[3];
                channels.push(super::ScanChannel {
                    mhz: value[0],
                    center_freq1: value[1],
                    center_freq2: value[2],
                    passive: info & (1 << 7) != 0,
                    allow_ht: info & (1 << 11) != 0,
                    allow_vht: info & (1 << 12) != 0,
                    allow_he: info & (1 << 17) != 0,
                    half_rate: info & (1 << 14) != 0,
                    quarter_rate: info & (1 << 15) != 0,
                    psc: info & (1 << 18) != 0,
                    dfs: info & (1 << 10) != 0,
                    phy_mode: info & 0x3f,
                    min_power: value[4] as u8,
                    max_power: (value[4] >> 8) as u8,
                    max_reg_power: (value[4] >> 16) as u8,
                    antenna_max: value[5] as u8,
                    reg_class_id: (value[4] >> 24) as u8,
                });
            }
            SemanticRequest::ScanChannelList(super::ScanChannelList {
                pdev_id: fixed[2],
                append: fixed[1] != 0,
                channels,
            })
        }
        0x003001 => {
            let tlvs = semantic_tlvs(id, bytes)?;
            if tlvs.len() != 5
                || tlvs[0].tag != crate::tags::WMI_TAG_START_SCAN_CMD.0
                || tlvs[1].tag != crate::tags::WMI_TAG_ARRAY_UINT32.0
                || tlvs[2].tag != crate::tags::WMI_TAG_ARRAY_FIXED_STRUCT.0
                || tlvs[3].tag != crate::tags::WMI_TAG_ARRAY_FIXED_STRUCT.0
                || tlvs[4].tag != crate::tags::WMI_TAG_ARRAY_BYTE.0
            {
                return Err(WmiError::Malformed);
            }
            let fixed = words::<39>(&tlvs[0].value)?;
            if fixed[25..34] != [0; 9] || fixed[38] != 0 {
                return Err(WmiError::Malformed);
            }
            let channels = tlvs[1]
                .value
                .chunks_exact(4)
                .map(|value| u32::from_le_bytes(value.try_into().expect("four-byte chunk")))
                .collect::<Vec<_>>();
            if channels.len() != fixed[16] as usize
                || tlvs[2].value.len() != fixed[18] as usize * 36
                || tlvs[3].value.len() != fixed[17] as usize * 8
            {
                return Err(WmiError::Malformed);
            }
            let mut ssids = Vec::new();
            for value in tlvs[2].value.chunks_exact(36) {
                let len =
                    u32::from_le_bytes(value[..4].try_into().map_err(|_| WmiError::Malformed)?)
                        as usize;
                if len > 32 {
                    return Err(WmiError::Malformed);
                }
                ssids.push(value[4..4 + len].to_vec());
            }
            let bssids = tlvs[3]
                .value
                .chunks_exact(8)
                .map(|value| mac(&value[..6]))
                .collect::<Result<Vec<_>, _>>()?;
            let ie_len = fixed[19] as usize;
            if ie_len > tlvs[4].value.len() {
                return Err(WmiError::Malformed);
            }
            let flags = fixed[14];
            let bit = |mask| flags & mask != 0;
            SemanticRequest::ScanStart(super::ScanStart {
                scan_id: fixed[0],
                scan_requester_id: fixed[1],
                vdev_id: fixed[2],
                scan_priority: fixed[3],
                notify_scan_events: fixed[4],
                event_flags: super::ScanEventFlags::default(),
                control_flags: super::ScanControlFlags {
                    passive: bit(0x1),
                    broadcast_probe: bit(0x2),
                    cck_rates: bit(0x4),
                    ofdm_rates: bit(0x8),
                    channel_stat_event: bit(0x10),
                    filter_probe_request: bit(0x20),
                    promiscuous: bit(0x100),
                    force_active_dfs: bit(0x200),
                    add_tpc_ie: bit(0x400),
                    add_ds_ie: bit(0x800),
                    spoofed_mac: bit(0x1000),
                    offchannel_mgmt_tx: bit(0x2000),
                    offchannel_data_tx: bit(0x4000),
                    capture_phy_error: bit(0x8000),
                    strict_passive: bit(0x10000),
                    half_rate: bit(0x20000),
                    quarter_rate: bit(0x40000),
                    random_sequence: bit(0x80000),
                    ie_whitelist: bit(0x100000),
                    adaptive_dwell_mode: (flags >> 21) & 7,
                },
                control_flags_ext: fixed[34],
                dwell_time_active: fixed[5],
                dwell_time_active_2ghz: fixed[35],
                dwell_time_passive: fixed[6],
                dwell_time_active_6ghz: fixed[36],
                dwell_time_passive_6ghz: fixed[37],
                min_rest_time: fixed[7],
                max_rest_time: fixed[8],
                repeat_probe_time: fixed[9],
                probe_spacing_time: fixed[10],
                idle_time: fixed[11],
                max_scan_time: fixed[12],
                probe_delay: fixed[13],
                burst_duration: fixed[15],
                n_probes: fixed[20],
                mac_addr: mac(&tlvs[0].value[84..90])?,
                mac_mask: mac(&tlvs[0].value[92..98])?,
                channels,
                ssids,
                bssids,
                extra_ie: tlvs[4].value[..ie_len].to_vec(),
                short_ssid_hints: Vec::new(),
                bssid_hints: Vec::new(),
            })
        }
        0x004003 | 0x005008 => {
            let tlvs = semantic_tlvs(id, bytes)?;
            if tlvs.len() != 1 {
                return Err(WmiError::Malformed);
            }
            let fixed = words::<3>(&tlvs[0].value)?;
            if id.0 == 0x004003 {
                if tlvs[0].tag != crate::tags::WMI_TAG_PDEV_SET_PARAM_CMD.0 {
                    return Err(WmiError::Malformed);
                }
                SemanticRequest::PdevSetParam(super::PdevSetParam {
                    pdev_id: fixed[0],
                    param_id: fixed[1],
                    param_value: fixed[2],
                })
            } else {
                if tlvs[0].tag != crate::tags::WMI_TAG_VDEV_SET_PARAM_CMD.0 {
                    return Err(WmiError::Malformed);
                }
                SemanticRequest::VdevSetParam(super::VdevSetParam {
                    vdev_id: fixed[0],
                    param_id: fixed[1],
                    param_value: fixed[2],
                })
            }
        }
        0x005009 => {
            let tlvs = semantic_tlvs(id, bytes)?;
            if tlvs.len() != 2
                || tlvs[0].tag != crate::tags::WMI_TAG_VDEV_INSTALL_KEY_CMD.0
                || tlvs[1].tag != crate::tags::WMI_TAG_ARRAY_BYTE.0
            {
                return Err(WmiError::Malformed);
            }
            let fixed = words::<25>(&tlvs[0].value)?;
            let key_len = fixed[20] as usize;
            if fixed[8..20] != [0; 12]
                || fixed[23] != 0
                || fixed[24] != 0
                || key_len > tlvs[1].value.len()
            {
                return Err(WmiError::Malformed);
            }
            SemanticRequest::VdevInstallKey(super::VdevInstallKey {
                vdev_id: fixed[0],
                peer_addr: mac(&tlvs[0].value[4..10])?,
                key_idx: fixed[3],
                key_flags: fixed[4],
                key_cipher: fixed[5],
                key_rsc_counter: super::KeySeqCounter {
                    low: fixed[6],
                    high: fixed[7],
                },
                key_data: tlvs[1].value[..key_len].to_vec(),
                key_txmic_len: fixed[21],
                key_rxmic_len: fixed[22],
            })
        }
        0x00500d => {
            let tlvs = semantic_tlvs(id, bytes)?;
            if tlvs.len() != 1 || tlvs[0].tag != crate::tags::WMI_TAG_VDEV_SET_WMM_PARAMS_CMD.0 {
                return Err(WmiError::Malformed);
            }
            let value = &tlvs[0].value;
            if value.len() != 120 {
                return Err(WmiError::Malformed);
            }
            let vdev_id =
                u32::from_le_bytes(value[..4].try_into().map_err(|_| WmiError::Malformed)?);
            let mut categories = [super::WmmAccessCategory::default(); 4];
            for (category, encoded) in categories.iter_mut().zip(value[4..116].chunks_exact(28)) {
                let header =
                    u32::from_le_bytes(encoded[..4].try_into().map_err(|_| WmiError::Malformed)?);
                if header != (u32::from(crate::tags::WMI_TAG_VDEV_SET_WMM_PARAMS_CMD.0) << 16) | 24
                {
                    return Err(WmiError::Malformed);
                }
                let fields = words::<6>(&encoded[4..])?;
                *category = super::WmmAccessCategory {
                    cw_min: fields[0],
                    cw_max: fields[1],
                    aifs: fields[2],
                    txop_limit: fields[3],
                    admission_control_mandatory: fields[4],
                    no_ack: fields[5],
                };
            }
            let parameter_type =
                u32::from_le_bytes(value[116..].try_into().map_err(|_| WmiError::Malformed)?);
            SemanticRequest::WmmUpdate(super::WmmUpdate {
                vdev_id,
                parameter_type,
                access_categories: categories,
            })
        }
        0x006004 => {
            let tlvs = semantic_tlvs(id, bytes)?;
            if tlvs.len() != 1 || tlvs[0].tag != crate::tags::WMI_TAG_PEER_SET_PARAM_CMD.0 {
                return Err(WmiError::Malformed);
            }
            let fixed = words::<5>(&tlvs[0].value)?;
            SemanticRequest::PeerSetParam(super::PeerSetParam {
                vdev_id: fixed[0],
                peer_addr: mac(&tlvs[0].value[4..10])?,
                param_id: fixed[3],
                param_value: fixed[4],
            })
        }
        0x006013 => {
            let tlvs = semantic_tlvs(id, bytes)?;
            if tlvs.len() != 1 || tlvs[0].tag != crate::tags::WMI_TAG_REORDER_QUEUE_SETUP_CMD.0 {
                return Err(WmiError::Malformed);
            }
            let fixed = words::<9>(&tlvs[0].value)?;
            if fixed[3] != fixed[6] {
                return Err(WmiError::Malformed);
            }
            SemanticRequest::PeerReorderQueueSetup(super::PeerReorderQueueSetup {
                vdev_id: fixed[0],
                peer_addr: mac(&tlvs[0].value[4..10])?,
                tid: u8::try_from(fixed[3]).map_err(|_| WmiError::Malformed)?,
                queue_address: 0,
                ba_window_size_valid: u8::try_from(fixed[7]).map_err(|_| WmiError::Malformed)?,
                ba_window_size: fixed[8],
            })
        }
        0x009001 => {
            let tlvs = semantic_tlvs(id, bytes)?;
            if tlvs.len() != 1 || tlvs[0].tag != crate::tags::WMI_TAG_STA_POWERSAVE_MODE_CMD.0 {
                return Err(WmiError::Malformed);
            }
            let fixed = words::<2>(&tlvs[0].value)?;
            SemanticRequest::StaPowerSaveMode(super::StaPowerSaveMode {
                vdev_id: fixed[0],
                mode: fixed[1],
            })
        }
        0x009002 => {
            let tlvs = semantic_tlvs(id, bytes)?;
            if tlvs.len() != 1 || tlvs[0].tag != crate::tags::WMI_TAG_STA_POWERSAVE_PARAM_CMD.0 {
                return Err(WmiError::Malformed);
            }
            let fixed = words::<3>(&tlvs[0].value)?;
            SemanticRequest::StaPowerSaveParameter(super::StaPowerSaveParameter {
                vdev_id: fixed[0],
                param: fixed[1],
                value: fixed[2],
            })
        }
        _ => return Ok(None),
    };
    let (family, masked_fields) = command_family(id.0).ok_or(WmiError::Malformed)?;
    Ok(Some(GoldenSemanticRequest {
        family,
        request,
        masked_fields,
        masked_ranges: if id.0 == 0x006013 {
            core::iter::once(24..32).collect()
        } else {
            Vec::new()
        },
    }))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Verification {
    CommandExact {
        seq: u64,
        id: u32,
    },
    CommandUnmapped {
        seq: u64,
        id: u32,
    },
    CommandMismatch {
        seq: u64,
        id: u32,
        mismatch: ByteMismatch,
    },
    EventDecoded {
        seq: u64,
        id: u32,
        decoder: &'static str,
    },
    DecodeFailed {
        seq: u64,
        id: u32,
        error: WmiError,
    },
}

fn json_value<'a>(line: &'a str, key: &'static str) -> Result<&'a str, GoldenError> {
    let mut needle = String::from("\"");
    needle.push_str(key);
    needle.push_str("\":");
    let start = line.find(&needle).ok_or(GoldenError::MissingField(key))? + needle.len();
    let tail = line[start..].trim_start();
    if let Some(tail) = tail.strip_prefix('"') {
        let end = tail.find('"').ok_or(GoldenError::InvalidField(key))?;
        Ok(&tail[..end])
    } else {
        Ok(tail
            .split([',', '}'])
            .next()
            .ok_or(GoldenError::InvalidField(key))?
            .trim())
    }
}
fn number(line: &str, key: &'static str) -> Result<u64, GoldenError> {
    json_value(line, key)?
        .parse()
        .map_err(|_| GoldenError::InvalidField(key))
}
fn decode_hex(value: &str) -> Result<Vec<u8>, GoldenError> {
    if !value.len().is_multiple_of(2)
        || value
            .bytes()
            .any(|b| !b.is_ascii_digit() && !(b'a'..=b'f').contains(&b))
    {
        return Err(GoldenError::InvalidHex);
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|p| {
            let d = |b: u8| if b <= b'9' { b - b'0' } else { b - b'a' + 10 };
            Ok((d(p[0]) << 4) | d(p[1]))
        })
        .collect()
}

pub fn parse_jsonl(input: &str) -> Result<Vec<TranscriptRecord>, GoldenError> {
    input
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let kind = match json_value(line, "kind")? {
                "wmi_cmd" => TranscriptKind::Command,
                "wmi_event" => TranscriptKind::Event,
                _ => return Err(GoldenError::InvalidField("kind")),
            };
            let bytes = decode_hex(json_value(line, "bytes_hex")?)?;
            let declared_len = usize::try_from(number(line, "len")?)
                .map_err(|_| GoldenError::InvalidField("len"))?;
            if declared_len != bytes.len() {
                return Err(GoldenError::LengthMismatch {
                    declared: declared_len,
                    actual: bytes.len(),
                });
            }
            let record = TranscriptRecord {
                seq: number(line, "seq")?,
                timestamp_ns: number(line, "ts_ns")?,
                kind,
                id: u32::try_from(number(line, "id")?)
                    .map_err(|_| GoldenError::InvalidField("id"))?,
                declared_len,
                bytes,
            };
            record.envelope_parts()?;
            Ok(record)
        })
        .collect()
}

impl TranscriptRecord {
    pub fn envelope_parts(&self) -> Result<(u32, &[u8]), GoldenError> {
        let h = self.bytes.get(..4).ok_or(GoldenError::TruncatedEnvelope)?;
        let envelope =
            u32::from_le_bytes(h.try_into().map_err(|_| GoldenError::TruncatedEnvelope)?)
                & 0x00ff_ffff;
        if envelope != (self.id & 0x00ff_ffff) {
            return Err(GoldenError::IdentifierMismatch {
                declared: self.id,
                envelope,
            });
        }
        Ok((envelope, &self.bytes[4..]))
    }
}

pub fn first_difference(expected: &[u8], actual: &[u8]) -> Option<ByteMismatch> {
    let offset = expected
        .iter()
        .zip(actual)
        .position(|(a, b)| a != b)
        .or_else(|| (expected.len() != actual.len()).then_some(expected.len().min(actual.len())))?;
    Some(ByteMismatch {
        first_differing_offset: offset,
        expected: expected.get(offset).copied(),
        actual: actual.get(offset).copied(),
        expected_len: expected.len(),
        actual_len: actual.len(),
    })
}
fn command_envelope(command: &Command) -> Vec<u8> {
    let mut b = Vec::with_capacity(4 + command.tlvs().len());
    b.extend_from_slice(&(command.id.0 & 0x00ff_ffff).to_le_bytes());
    b.extend_from_slice(command.tlvs());
    b
}

/// Verifies a transcript while keeping command reverse-mapping and event
/// dispatch owned by their typed protocol modules.
pub fn verify_transcript<C, E>(
    records: &[TranscriptRecord],
    mut reencode: C,
    mut decode_event: E,
) -> Vec<Verification>
where
    C: FnMut(CommandId, &[u8]) -> Result<Option<Command>, WmiError>,
    E: FnMut(Event) -> Result<&'static str, WmiError>,
{
    records
        .iter()
        .map(|r| {
            let (_, tlvs) = match r.envelope_parts() {
                Ok(v) => v,
                Err(_) => {
                    return Verification::DecodeFailed {
                        seq: r.seq,
                        id: r.id,
                        error: WmiError::Malformed,
                    };
                }
            };
            match r.kind {
                TranscriptKind::Command => match reencode(CommandId(r.id), tlvs) {
                    Ok(None) => Verification::CommandUnmapped {
                        seq: r.seq,
                        id: r.id,
                    },
                    Err(error) => Verification::DecodeFailed {
                        seq: r.seq,
                        id: r.id,
                        error,
                    },
                    Ok(Some(command)) => {
                        let actual = command_envelope(&command);
                        match first_difference(&r.bytes, &actual) {
                            None => Verification::CommandExact {
                                seq: r.seq,
                                id: r.id,
                            },
                            Some(mismatch) => Verification::CommandMismatch {
                                seq: r.seq,
                                id: r.id,
                                mismatch,
                            },
                        }
                    }
                },
                TranscriptKind::Event => match Event::from_tlvs(EventId(r.id), tlvs.to_vec())
                    .and_then(&mut decode_event)
                {
                    Ok(decoder) => Verification::EventDecoded {
                        seq: r.seq,
                        id: r.id,
                        decoder,
                    },
                    Err(error) => Verification::DecodeFailed {
                        seq: r.seq,
                        id: r.id,
                        error,
                    },
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    #[test]
    fn parses_capture_schema_and_reports_first_offset() {
        let input = "{\"seq\":7,\"ts_ns\":99,\"kind\":\"wmi_cmd\",\"id\":1,\"len\":8,\"bytes_hex\":\"0100000000000000\"}\n";
        let records = parse_jsonl(input).unwrap();
        assert_eq!(records[0].timestamp_ns, 99);
        let out = verify_transcript(
            &records,
            |_, _| {
                Ok(Some(
                    Command::from_tlvs(CommandId(1), vec![1, 0, 0, 0]).unwrap(),
                ))
            },
            |_| Ok("unused"),
        );
        assert_eq!(
            out[0],
            Verification::CommandMismatch {
                seq: 7,
                id: 1,
                mismatch: ByteMismatch {
                    first_differing_offset: 4,
                    expected: Some(0),
                    actual: Some(1),
                    expected_len: 8,
                    actual_len: 8
                }
            }
        );
    }
    #[test]
    fn validates_event_through_caller_dispatch() {
        let records=parse_jsonl("{\"seq\":8,\"ts_ns\":100,\"kind\":\"wmi_event\",\"id\":2,\"len\":8,\"bytes_hex\":\"0200000000000000\"}").unwrap();
        assert_eq!(
            verify_transcript(&records, |_, _| Ok(None), |_| Ok("Ready"))[0],
            Verification::EventDecoded {
                seq: 8,
                id: 2,
                decoder: "Ready"
            }
        );
    }
    #[test]
    fn rejects_bad_hex_length_and_header() {
        assert_eq!(
            parse_jsonl(
                "{\"seq\":1,\"ts_ns\":0,\"kind\":\"wmi_cmd\",\"id\":1,\"len\":1,\"bytes_hex\":\"A0\"}"
            ),
            Err(GoldenError::InvalidHex)
        );
        assert!(matches!(
            parse_jsonl(
                "{\"seq\":1,\"ts_ns\":0,\"kind\":\"wmi_cmd\",\"id\":2,\"len\":4,\"bytes_hex\":\"01000000\"}"
            ),
            Err(GoldenError::IdentifierMismatch { .. })
        ));
    }
}
