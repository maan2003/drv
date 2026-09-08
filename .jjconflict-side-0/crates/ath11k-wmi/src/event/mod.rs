//! Firmware-to-host WMI TLV decoding.
//!
//! The iterator mirrors `ath11k_wmi_tlv_iter`: the four-byte TLV header and
//! the advertised value length must fit, while values need not be padded by
//! the decoder.  Unknown tags are retained/ignored by the event-specific
//! parser, as they are by the Linux callbacks.

use alloc::vec::Vec;
use core::marker::PhantomData;

use crate::tags;
use crate::tags::{
    WMI_TAG_ARRAY_BYTE, WMI_TAG_ARRAY_FIXED_STRUCT, WMI_TAG_CHAN_INFO_EVENT, WMI_TAG_MGMT_RX_HDR,
    WMI_TAG_MGMT_TX_COMPL_EVENT, WMI_TAG_OFFLOAD_BCN_TX_STATUS_EVENT,
    WMI_TAG_PEER_ASSOC_CONF_EVENT, WMI_TAG_PEER_DELETE_RESP_EVENT, WMI_TAG_PEER_STA_KICKOUT_EVENT,
    WMI_TAG_READY_EVENT, WMI_TAG_ROAM_EVENT, WMI_TAG_SCAN_EVENT, WMI_TAG_VDEV_DELETE_RESP_EVENT,
    WMI_TAG_VDEV_INSTALL_KEY_COMPLETE_EVENT, WMI_TAG_VDEV_START_RESPONSE_EVENT,
    WMI_TAG_VDEV_STOPPED_EVENT,
};
use crate::{Event, EventId, WmiError};

mod lifecycle;
pub use lifecycle::*;

pub trait EventDecoder {
    type Output;
    fn decode(&self, event: Event) -> Result<Self::Output, WmiError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Tlv<'a> {
    pub tag: u16,
    pub value: &'a [u8],
}

#[derive(Clone, Debug)]
pub struct TlvIter<'a> {
    remaining: &'a [u8],
    failed: bool,
}

impl<'a> TlvIter<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self {
            remaining: bytes,
            failed: false,
        }
    }
}

impl<'a> Iterator for TlvIter<'a> {
    type Item = Result<Tlv<'a>, WmiError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining.is_empty() || self.failed {
            return None;
        }
        if self.remaining.len() < 4 {
            self.failed = true;
            return Some(Err(WmiError::Malformed));
        }
        let header = u32::from_le_bytes(self.remaining[..4].try_into().unwrap());
        let len = (header & 0xffff) as usize;
        let tag = (header >> 16) as u16;
        if len > self.remaining.len() - 4 {
            self.failed = true;
            return Some(Err(WmiError::Malformed));
        }
        let (value, tail) = self.remaining[4..].split_at(len);
        if policy_min_len(tag).is_some_and(|minimum| len < minimum) {
            self.failed = true;
            return Some(Err(WmiError::Malformed));
        }
        self.remaining = tail;
        Some(Ok(Tlv { tag, value }))
    }
}

fn policy_min_len(tag: u16) -> Option<usize> {
    Some(match tag {
        x if x == tags::WMI_TAG_SERVICE_READY_EVENT.0 => 128,
        x if x == tags::WMI_TAG_SERVICE_READY_EXT_EVENT.0 => 76,
        x if x == tags::WMI_TAG_SOC_MAC_PHY_HW_MODE_CAPS.0 => 4,
        x if x == tags::WMI_TAG_SOC_HAL_REG_CAPABILITIES.0 => 4,
        x if x == tags::WMI_TAG_VDEV_START_RESPONSE_EVENT.0 => 40,
        x if x == tags::WMI_TAG_PEER_DELETE_RESP_EVENT.0 => 12,
        x if x == tags::WMI_TAG_OFFLOAD_BCN_TX_STATUS_EVENT.0 => 8,
        x if x == tags::WMI_TAG_VDEV_STOPPED_EVENT.0 => 4,
        x if x == tags::WMI_TAG_REG_CHAN_LIST_CC_EVENT.0 => 56,
        x if x == tags::WMI_TAG_REG_CHAN_LIST_CC_EXT_EVENT.0 => 312,
        x if x == tags::WMI_TAG_MGMT_RX_HDR.0 => 68,
        x if x == tags::WMI_TAG_MGMT_TX_COMPL_EVENT.0 => 20,
        x if x == tags::WMI_TAG_SCAN_EVENT.0 => 28,
        x if x == tags::WMI_TAG_PEER_STA_KICKOUT_EVENT.0 => 8,
        x if x == tags::WMI_TAG_ROAM_EVENT.0 => 12,
        x if x == tags::WMI_TAG_CHAN_INFO_EVENT.0 => 56,
        x if x == tags::WMI_TAG_PDEV_BSS_CHAN_INFO_EVENT.0 => 52,
        x if x == tags::WMI_TAG_VDEV_INSTALL_KEY_COMPLETE_EVENT.0 => 24,
        x if x == tags::WMI_TAG_READY_EVENT.0 => 52,
        x if x == tags::WMI_TAG_SERVICE_AVAILABLE_EVENT.0 => 20,
        x if x == tags::WMI_TAG_PEER_ASSOC_CONF_EVENT.0 => 12,
        x if x == tags::WMI_TAG_STATS_EVENT.0 => 44,
        x if x == tags::WMI_TAG_PDEV_CTL_FAILSAFE_CHECK_EVENT.0 => 8,
        x if x == tags::WMI_TAG_HOST_SWFDA_EVENT.0 => 12,
        x if x == tags::WMI_TAG_OFFLOAD_PRB_RSP_TX_STATUS_EVENT.0 => 8,
        x if x == tags::WMI_TAG_VDEV_DELETE_RESP_EVENT.0 => 4,
        x if x == tags::WMI_TAG_OBSS_COLOR_COLLISION_EVT.0 => 16,
        x if x == tags::WMI_TAG_11D_NEW_COUNTRY_EVENT.0 => 4,
        x if x == tags::WMI_TAG_PER_CHAIN_RSSI_STATS.0 => 4,
        x if x == tags::WMI_TAG_TWT_ADD_DIALOG_COMPLETE_EVENT.0 => 20,
        x if x == tags::WMI_TAG_P2P_NOA_INFO.0 => 68,
        x if x == tags::WMI_TAG_P2P_NOA_EVENT.0 => 4,
        _ => return None,
    })
}

/// Source-shaped parse table behavior: a later TLV replaces an earlier TLV
/// with the same tag.
pub fn find_tlv(bytes: &[u8], tag: u16) -> Result<Option<&[u8]>, WmiError> {
    let mut found = None;
    for tlv in TlvIter::new(bytes) {
        let tlv = tlv?;
        if tlv.tag == tag {
            found = Some(tlv.value);
        }
    }
    Ok(found)
}

pub trait WireEvent: Sized {
    const TAG: u16;
    const MIN_LEN: usize;
    fn parse(value: &[u8], all_tlvs: &[u8]) -> Result<Self, WmiError>;
}

#[derive(Clone, Copy, Debug)]
pub struct Decoder<T> {
    id: EventId,
    marker: PhantomData<T>,
}

impl<T> Decoder<T> {
    pub const fn new(id: EventId) -> Self {
        Self {
            id,
            marker: PhantomData,
        }
    }
}

impl<T: WireEvent> EventDecoder for Decoder<T> {
    type Output = T;

    fn decode(&self, event: Event) -> Result<T, WmiError> {
        if event.id != self.id {
            return Err(WmiError::Malformed);
        }
        let value = find_tlv(event.tlvs(), T::TAG)?.ok_or(WmiError::Malformed)?;
        if value.len() < T::MIN_LEN {
            return Err(WmiError::Malformed);
        }
        T::parse(value, event.tlvs())
    }
}

pub(crate) fn word(bytes: &[u8], offset: usize) -> Result<u32, WmiError> {
    bytes
        .get(offset..offset + 4)
        .and_then(|b| b.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or(WmiError::Malformed)
}

fn mac(bytes: &[u8], offset: usize) -> Result<[u8; 6], WmiError> {
    bytes
        .get(offset..offset + 6)
        .and_then(|b| b.try_into().ok())
        .ok_or(WmiError::Malformed)
}

macro_rules! words_event {
    ($name:ident, $tag:expr, $len:expr, { $($field:ident : $off:expr),+ $(,)? }) => {
        #[derive(Clone, Debug, Eq, PartialEq)]
        pub struct $name { $(pub $field: u32),+ }
        impl WireEvent for $name {
            const TAG: u16 = $tag;
            const MIN_LEN: usize = $len;
            fn parse(value: &[u8], _: &[u8]) -> Result<Self, WmiError> {
                Ok(Self { $($field: word(value, $off)?),+ })
            }
        }
    };
}

words_event!(VdevStartResponse, WMI_TAG_VDEV_START_RESPONSE_EVENT.0, 40, {
    vdev_id: 0, requestor_id: 4, response_type: 8, status: 12,
    chain_mask: 16, smps_mode: 20, mac_id: 24, configured_tx_streams: 28,
    configured_rx_streams: 32, max_allowed_tx_power: 36
});
words_event!(VdevStopped, WMI_TAG_VDEV_STOPPED_EVENT.0, 4, { vdev_id: 0 });
words_event!(VdevDeleteResponse, WMI_TAG_VDEV_DELETE_RESP_EVENT.0, 4, { vdev_id: 0 });
words_event!(BeaconTxStatus, WMI_TAG_OFFLOAD_BCN_TX_STATUS_EVENT.0, 8, { vdev_id: 0, tx_status: 4 });
words_event!(MgmtTxCompletion, WMI_TAG_MGMT_TX_COMPL_EVENT.0, 20, {
    descriptor_id: 0, status: 4, pdev_id: 8, ppdu_id: 12, ack_rssi: 16
});
words_event!(Scan, WMI_TAG_SCAN_EVENT.0, 28, {
    event_type: 0, reason: 4, channel_freq: 8, scan_request_id: 12,
    scan_id: 16, vdev_id: 20, tsf_timestamp: 24
});
words_event!(Roam, WMI_TAG_ROAM_EVENT.0, 12, { vdev_id: 0, reason: 4, rssi: 8 });
words_event!(ChannelInfo, WMI_TAG_CHAN_INFO_EVENT.0, 56, {
    error_code: 0, freq: 4, command_flags: 8, noise_floor: 12,
    rx_clear_count: 16, cycle_count: 20, tx_power_range: 24,
    tx_power_throughput: 28, rx_frame_count: 32, my_bss_rx_cycle_count: 36,
    rx_11b_duration: 40, tx_frame_count: 44, mac_clock_mhz: 48, vdev_id: 52
});
words_event!(PdevBssChannelInfo, tags::WMI_TAG_PDEV_BSS_CHAN_INFO_EVENT.0, 52, {
    freq: 0, noise_floor: 4, rx_clear_low: 8, rx_clear_high: 12,
    cycle_low: 16, cycle_high: 20, tx_cycle_low: 24, tx_cycle_high: 28,
    rx_cycle_low: 32, rx_cycle_high: 36, rx_bss_cycle_low: 40,
    rx_bss_cycle_high: 44, pdev_id: 48
});
words_event!(PdevCtlFailsafeCheck, tags::WMI_TAG_PDEV_CTL_FAILSAFE_CHECK_EVENT.0, 8, {
    pdev_id: 0, status: 4
});
words_event!(PdevTemperature, tags::WMI_TAG_PDEV_TEMPERATURE_EVENT.0, 8, {
    temperature: 0, pdev_id: 4
});
words_event!(FilsDiscovery, tags::WMI_TAG_HOST_SWFDA_EVENT.0, 12, {
    vdev_id: 0, fils_transmit_time: 4, next_tbtt: 8
});
words_event!(ProbeResponseTxStatus, tags::WMI_TAG_OFFLOAD_PRB_RSP_TX_STATUS_EVENT.0, 8, {
    vdev_id: 0, tx_status: 4
});
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObssColorCollision {
    pub vdev_id: u32,
    pub event_type: u32,
    pub color_bitmap: u64,
}
impl WireEvent for ObssColorCollision {
    const TAG: u16 = tags::WMI_TAG_OBSS_COLOR_COLLISION_EVT.0;
    const MIN_LEN: usize = 16;
    fn parse(v: &[u8], _: &[u8]) -> Result<Self, WmiError> {
        Ok(Self {
            vdev_id: word(v, 0)?,
            event_type: word(v, 4)?,
            color_bitmap: u64::from(word(v, 8)?) | (u64::from(word(v, 12)?) << 32),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TwtAddDialog {
    pub vdev_id: u32,
    pub peer_mac: [u8; 6],
    pub dialog_id: u32,
    pub status: u32,
}
impl WireEvent for TwtAddDialog {
    const TAG: u16 = tags::WMI_TAG_TWT_ADD_DIALOG_COMPLETE_EVENT.0;
    const MIN_LEN: usize = 20;
    fn parse(v: &[u8], _: &[u8]) -> Result<Self, WmiError> {
        Ok(Self {
            vdev_id: word(v, 0)?,
            peer_mac: mac(v, 4)?,
            dialog_id: word(v, 12)?,
            status: word(v, 16)?,
        })
    }
}
words_event!(PdevDfsRadar, tags::WMI_TAG_PDEV_DFS_RADAR_DETECTION_EVENT.0, 40, {
    pdev_id: 0, detection_mode: 4, channel_freq: 8, channel_width: 12,
    detector_id: 16, segment_id: 20, timestamp: 24, is_chirp: 28,
    frequency_offset: 32, sidx: 36
});
words_event!(NewCountry, tags::WMI_TAG_11D_NEW_COUNTRY_EVENT.0, 4, { alpha2: 0 });
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PeerStaPowerSaveStateChange {
    pub peer_mac: [u8; 6],
    pub peer_ps_state: u32,
    pub supported_bitmap: u32,
    pub peer_ps_valid: u32,
    pub timestamp: u32,
}
impl WireEvent for PeerStaPowerSaveStateChange {
    const TAG: u16 = tags::WMI_TAG_PEER_STA_PS_STATECHANGE_EVENT.0;
    const MIN_LEN: usize = 24;
    fn parse(v: &[u8], _: &[u8]) -> Result<Self, WmiError> {
        Ok(Self {
            peer_mac: mac(v, 0)?,
            peer_ps_state: word(v, 8)?,
            supported_bitmap: word(v, 12)?,
            peer_ps_valid: word(v, 16)?,
            timestamp: word(v, 20)?,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PdevCsaSwitchCount {
    pub pdev_id: u32,
    pub current_switch_count: u32,
    pub declared_vdev_count: u32,
    pub vdev_ids: Vec<u32>,
}
impl WireEvent for PdevCsaSwitchCount {
    const TAG: u16 = tags::WMI_TAG_PDEV_CSA_SWITCH_COUNT_STATUS_EVENT.0;
    const MIN_LEN: usize = 12;
    fn parse(v: &[u8], all: &[u8]) -> Result<Self, WmiError> {
        let declared = word(v, 8)?;
        let ids = find_tlv(all, tags::WMI_TAG_ARRAY_UINT32.0)?.ok_or(WmiError::Malformed)?;
        let mut vdev_ids = Vec::new();
        for item in ids.chunks_exact(4).take(declared as usize) {
            vdev_ids.push(word(item, 0)?);
        }
        Ok(Self {
            pdev_id: word(v, 0)?,
            current_switch_count: word(v, 4)?,
            declared_vdev_count: declared,
            vdev_ids,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct P2pNoa {
    pub vdev_id: u32,
    pub noa_info: Vec<u8>,
    pub descriptor_count: u8,
}
impl WireEvent for P2pNoa {
    const TAG: u16 = tags::WMI_TAG_P2P_NOA_EVENT.0;
    const MIN_LEN: usize = 4;
    fn parse(v: &[u8], all: &[u8]) -> Result<Self, WmiError> {
        let info = find_tlv(all, tags::WMI_TAG_P2P_NOA_INFO.0)?.ok_or(WmiError::Malformed)?;
        if info.len() < 68 {
            return Err(WmiError::Malformed);
        }
        let descriptor_count = ((word(info, 0)? >> 24) & 0xff) as u8;
        if descriptor_count > 4 {
            return Err(WmiError::Malformed);
        }
        Ok(Self {
            vdev_id: word(v, 0)?,
            noa_info: info.to_vec(),
            descriptor_count,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GtkOffloadStatus {
    pub bytes: Vec<u8>,
}
impl WireEvent for GtkOffloadStatus {
    const TAG: u16 = tags::WMI_TAG_GTK_OFFLOAD_STATUS_EVENT.0;
    const MIN_LEN: usize = 102;
    fn parse(v: &[u8], _: &[u8]) -> Result<Self, WmiError> {
        Ok(Self { bytes: v.to_vec() })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceAvailable {
    pub segment_offset: u32,
    pub bitmap: [u32; 4],
    pub ext2_bitmap: Option<[u32; 4]>,
}
impl WireEvent for ServiceAvailable {
    const TAG: u16 = tags::WMI_TAG_SERVICE_AVAILABLE_EVENT.0;
    const MIN_LEN: usize = 20;
    fn parse(v: &[u8], all: &[u8]) -> Result<Self, WmiError> {
        let mut bitmap = [0; 4];
        for (i, item) in bitmap.iter_mut().enumerate() {
            *item = word(v, 4 + i * 4)?;
        }
        let ext2_bitmap = match find_tlv(all, tags::WMI_TAG_ARRAY_UINT32.0)? {
            Some(v) => {
                if v.len() < 16 {
                    return Err(WmiError::Malformed);
                }
                let mut value = [0; 4];
                for (i, item) in value.iter_mut().enumerate() {
                    *item = word(v, i * 4)?;
                }
                Some(value)
            }
            None => None,
        };
        Ok(Self {
            segment_offset: word(v, 0)?,
            bitmap,
            ext2_bitmap,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DmaRingBufferRelease {
    pub fixed: Vec<u8>,
    pub entries: Vec<OwnedNestedTlv>,
    pub metadata: Vec<OwnedNestedTlv>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedNestedTlv {
    pub tag: u16,
    pub value: Vec<u8>,
}

impl WireEvent for DmaRingBufferRelease {
    const TAG: u16 = tags::WMI_TAG_DMA_BUF_RELEASE.0;
    const MIN_LEN: usize = 16;
    fn parse(v: &[u8], all: &[u8]) -> Result<Self, WmiError> {
        let entry_limit = word(v, 8)? as usize;
        let metadata_limit = word(v, 12)? as usize;
        let mut groups = Vec::new();
        for tlv in TlvIter::new(all) {
            let tlv = tlv?;
            if tlv.tag == tags::WMI_TAG_ARRAY_STRUCT.0 {
                groups.push(tlv.value);
            }
        }
        let mut entries = Vec::new();
        if let Some(group) = groups.first() {
            for nested in TlvIter::new(group) {
                let nested = nested?;
                if nested.tag != tags::WMI_TAG_DMA_BUF_RELEASE_ENTRY.0
                    || entries.len() >= entry_limit
                {
                    return Err(WmiError::Malformed);
                }
                if nested.value.len() < 8 {
                    return Err(WmiError::Malformed);
                }
                entries.push(OwnedNestedTlv {
                    tag: nested.tag,
                    value: nested.value.to_vec(),
                });
            }
        }
        let mut metadata = Vec::new();
        if let Some(group) = groups.get(1) {
            for nested in TlvIter::new(group) {
                let nested = nested?;
                if nested.tag != tags::WMI_TAG_DMA_BUF_RELEASE_SPECTRAL_META_DATA.0
                    || metadata.len() >= metadata_limit
                {
                    return Err(WmiError::Malformed);
                }
                if nested.value.len() < 48 {
                    return Err(WmiError::Malformed);
                }
                metadata.push(OwnedNestedTlv {
                    tag: nested.tag,
                    value: nested.value.to_vec(),
                });
            }
        }
        Ok(Self {
            fixed: v.to_vec(),
            entries,
            metadata,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WowWakeupHost {
    pub wake_reason: u32,
    pub info: Vec<u8>,
    pub data: Option<Vec<u8>>,
}
impl WireEvent for WowWakeupHost {
    const TAG: u16 = tags::WMI_TAG_WOW_EVENT_INFO.0;
    const MIN_LEN: usize = 16;
    fn parse(v: &[u8], all: &[u8]) -> Result<Self, WmiError> {
        Ok(Self {
            wake_reason: word(v, 8)?,
            info: v.to_vec(),
            data: find_tlv(all, tags::WMI_TAG_ARRAY_BYTE.0)?.map(<[u8]>::to_vec),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PeerCfrCapture {
    pub fixed: Option<Vec<u8>>,
    pub phase: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Debug)]
pub struct PeerCfrCaptureDecoder {
    pub id: EventId,
}
impl EventDecoder for PeerCfrCaptureDecoder {
    type Output = PeerCfrCapture;
    fn decode(&self, event: Event) -> Result<Self::Output, WmiError> {
        if event.id != self.id {
            return Err(WmiError::Malformed);
        }
        let mut fixed = None;
        let mut phase = None;
        for tlv in TlvIter::new(event.tlvs()) {
            let tlv = tlv?;
            if tlv.tag == tags::WMI_TAG_PEER_CFR_CAPTURE_EVENT.0 {
                if tlv.value.len() < 100 {
                    return Err(WmiError::Malformed);
                }
                fixed = Some(tlv.value.to_vec());
            } else if tlv.tag == tags::WMI_TAG_CFR_CAPTURE_PHASE_PARAM.0 {
                if tlv.value.len() < 40 {
                    return Err(WmiError::Malformed);
                }
                phase = Some(tlv.value.to_vec());
            } else {
                return Err(WmiError::Malformed);
            }
        }
        Ok(PeerCfrCapture { fixed, phase })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpaqueEvent {
    pub bytes: Vec<u8>,
}

/// Decoder for the DIAG and UTF cases, which Linux deliberately forwards
/// without running the TLV iterator.
#[derive(Clone, Copy, Debug)]
pub struct OpaqueEventDecoder {
    pub id: EventId,
}
impl EventDecoder for OpaqueEventDecoder {
    type Output = OpaqueEvent;
    fn decode(&self, event: Event) -> Result<Self::Output, WmiError> {
        if event.id != self.id {
            return Err(WmiError::Malformed);
        }
        Ok(OpaqueEvent {
            bytes: event.tlvs().to_vec(),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegulatoryChannelList {
    pub fixed: Vec<u8>,
    /// Header-inclusive firmware rule records (16 bytes, or 20 for ext).
    pub rules: Vec<Vec<u8>>,
    pub extended: bool,
}

fn regulatory_rules(
    fixed: &[u8],
    all: &[u8],
    extended: bool,
) -> Result<RegulatoryChannelList, WmiError> {
    let mut count = (word(fixed, 48)? + word(fixed, 52)?) as usize;
    if extended {
        for offset in [252usize, 256, 260] {
            let value = word(fixed, offset)?;
            if value > 5 {
                return Err(WmiError::Malformed);
            }
            count += value as usize;
        }
        for offset in (264..312).step_by(4) {
            let value = word(fixed, offset)?;
            if value > 5 {
                return Err(WmiError::Malformed);
            }
            count += value as usize;
        }
        if word(fixed, 48)? > 10 || word(fixed, 52)? > 10 {
            return Err(WmiError::Malformed);
        }
    }
    if count == 0 {
        return Err(WmiError::Malformed);
    }
    let array = find_tlv(all, tags::WMI_TAG_ARRAY_STRUCT.0)?.ok_or(WmiError::Malformed)?;
    let size = if extended { 20 } else { 16 };
    let rules = array
        .chunks_exact(size)
        .take(count)
        .map(<[u8]>::to_vec)
        .collect();
    Ok(RegulatoryChannelList {
        fixed: fixed.to_vec(),
        rules,
        extended,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegulatoryChannelListLegacy(pub RegulatoryChannelList);
impl WireEvent for RegulatoryChannelListLegacy {
    const TAG: u16 = tags::WMI_TAG_REG_CHAN_LIST_CC_EVENT.0;
    const MIN_LEN: usize = 56;
    fn parse(v: &[u8], all: &[u8]) -> Result<Self, WmiError> {
        regulatory_rules(v, all, false).map(Self)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegulatoryChannelListExtended(pub RegulatoryChannelList);
impl WireEvent for RegulatoryChannelListExtended {
    const TAG: u16 = tags::WMI_TAG_REG_CHAN_LIST_CC_EXT_EVENT.0;
    const MIN_LEN: usize = 312;
    fn parse(v: &[u8], all: &[u8]) -> Result<Self, WmiError> {
        regulatory_rules(v, all, true).map(Self)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpdateStats {
    pub fixed: Vec<u8>,
    pub pdev_stats: Vec<Vec<u8>>,
    pub vdev_stats: Vec<Vec<u8>>,
    pub beacon_stats: Vec<Vec<u8>>,
}
impl WireEvent for UpdateStats {
    const TAG: u16 = tags::WMI_TAG_STATS_EVENT.0;
    const MIN_LEN: usize = 44;
    fn parse(v: &[u8], all: &[u8]) -> Result<Self, WmiError> {
        let data = find_tlv(all, tags::WMI_TAG_ARRAY_BYTE.0)?.unwrap_or(&[]);
        let mut offset = 0usize;
        fn take_many(
            data: &[u8],
            offset: &mut usize,
            count: u32,
            size: usize,
        ) -> Result<Vec<Vec<u8>>, WmiError> {
            let mut out = Vec::new();
            for _ in 0..count {
                let end = offset.checked_add(size).ok_or(WmiError::Malformed)?;
                let item = data.get(*offset..end).ok_or(WmiError::Malformed)?;
                out.push(item.to_vec());
                *offset = end;
            }
            Ok(out)
        }
        let pdev_stats = take_many(data, &mut offset, word(v, 4)?, 228)?;
        let vdev_stats = take_many(data, &mut offset, word(v, 8)?, 164)?;
        let beacon_stats = take_many(data, &mut offset, word(v, 32)?, 12)?;
        Ok(Self {
            fixed: v.to_vec(),
            pdev_stats,
            vdev_stats,
            beacon_stats,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallKeyCompletion {
    pub vdev_id: u32,
    pub peer_mac: [u8; 6],
    pub key_index: u32,
    pub key_flags: u32,
    pub status: u32,
}
impl WireEvent for InstallKeyCompletion {
    const TAG: u16 = WMI_TAG_VDEV_INSTALL_KEY_COMPLETE_EVENT.0;
    const MIN_LEN: usize = 24;
    fn parse(v: &[u8], _: &[u8]) -> Result<Self, WmiError> {
        Ok(Self {
            vdev_id: word(v, 0)?,
            peer_mac: mac(v, 4)?,
            key_index: word(v, 12)?,
            key_flags: word(v, 16)?,
            status: word(v, 20)?,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PeerDeleteResponse {
    pub vdev_id: u32,
    pub peer_mac: [u8; 6],
}
impl WireEvent for PeerDeleteResponse {
    const TAG: u16 = WMI_TAG_PEER_DELETE_RESP_EVENT.0;
    const MIN_LEN: usize = 12;
    fn parse(v: &[u8], _: &[u8]) -> Result<Self, WmiError> {
        Ok(Self {
            vdev_id: word(v, 0)?,
            peer_mac: mac(v, 4)?,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PeerAssocConfirmation {
    pub vdev_id: u32,
    pub peer_mac: [u8; 6],
}
impl WireEvent for PeerAssocConfirmation {
    const TAG: u16 = WMI_TAG_PEER_ASSOC_CONF_EVENT.0;
    const MIN_LEN: usize = 12;
    fn parse(v: &[u8], _: &[u8]) -> Result<Self, WmiError> {
        Ok(Self {
            vdev_id: word(v, 0)?,
            peer_mac: mac(v, 4)?,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PeerStaKickout {
    pub peer_mac: [u8; 6],
}
impl WireEvent for PeerStaKickout {
    const TAG: u16 = WMI_TAG_PEER_STA_KICKOUT_EVENT.0;
    const MIN_LEN: usize = 8;
    fn parse(v: &[u8], _: &[u8]) -> Result<Self, WmiError> {
        Ok(Self {
            peer_mac: mac(v, 0)?,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MgmtRx {
    pub channel: u32,
    pub snr: u32,
    pub rate: u32,
    pub phy_mode: u32,
    pub status: u32,
    pub flags: u32,
    pub rssi: i32,
    pub tsf_delta: u32,
    pub pdev_id: u32,
    pub channel_freq: u32,
    pub frame: Vec<u8>,
}
impl WireEvent for MgmtRx {
    const TAG: u16 = WMI_TAG_MGMT_RX_HDR.0;
    const MIN_LEN: usize = 68;
    fn parse(v: &[u8], all: &[u8]) -> Result<Self, WmiError> {
        let frame_len = word(v, 16)? as usize;
        let mut offset = 0usize;
        let mut frame_start = None;
        for tlv in TlvIter::new(all) {
            let tlv = tlv?;
            if tlv.tag == WMI_TAG_ARRAY_BYTE.0 && frame_start.is_none() {
                frame_start = Some(offset + 4);
            }
            offset += 4 + tlv.value.len();
        }
        let frame_start = frame_start.ok_or(WmiError::Malformed)?;
        let frame_end = frame_start
            .checked_add(frame_len)
            .ok_or(WmiError::Malformed)?;
        // Linux checks against the skb tail, not the ARRAY_BYTE TLV length.
        let frame = all
            .get(frame_start..frame_end)
            .ok_or(WmiError::Malformed)?
            .to_vec();
        Ok(Self {
            channel: word(v, 0)?,
            snr: word(v, 4)?,
            rate: word(v, 8)?,
            phy_mode: word(v, 12)?,
            status: word(v, 20)?,
            flags: word(v, 40)?,
            rssi: word(v, 44)? as i32,
            tsf_delta: word(v, 48)?,
            pdev_id: word(v, 60)?,
            channel_freq: word(v, 64)?,
            frame,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Ready {
    pub mac_addr: Option<[u8; 6]>,
    pub status: Option<u32>,
    pub extra_mac_addresses: Vec<[u8; 6]>,
    pub pktlog_defs_checksum: Option<u32>,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct ReadyDecoder;
impl EventDecoder for ReadyDecoder {
    type Output = Ready;
    fn decode(&self, event: Event) -> Result<Self::Output, WmiError> {
        if event.id != tags::WMI_READY_EVENTID {
            return Err(WmiError::Malformed);
        }
        let mut mac_addr = None;
        let mut status = None;
        let mut extra = Vec::new();
        let mut pktlog_defs_checksum = None;
        let mut count = 0usize;
        for tlv in TlvIter::new(event.tlvs()) {
            let tlv = tlv?;
            if tlv.tag == WMI_TAG_READY_EVENT.0 {
                count = word(tlv.value, 40)? as usize;
                mac_addr = Some(mac(tlv.value, 24)?);
                status = Some(word(tlv.value, 32)?);
                pktlog_defs_checksum = if tlv.value.len() >= 60 {
                    Some(word(tlv.value, 56)?)
                } else {
                    None
                };
            } else if tlv.tag == WMI_TAG_ARRAY_FIXED_STRUCT.0 {
                extra.clear();
                for item in tlv.value.chunks_exact(8).take(count) {
                    extra.push(mac(item, 0)?);
                }
            }
        }
        Ok(Ready {
            mac_addr,
            status,
            extra_mac_addresses: extra,
            pktlog_defs_checksum,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tlv(tag: u16, value: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&((u32::from(tag) << 16) | value.len() as u32).to_le_bytes());
        out.extend_from_slice(value);
        out
    }

    #[test]
    fn iterator_rejects_truncated_header_and_value_without_panicking() {
        for bytes in [&[1, 2, 3][..], &[8, 0, 1, 0, 0, 0, 0, 0][..]] {
            assert_eq!(TlvIter::new(bytes).next(), Some(Err(WmiError::Malformed)));
        }
    }

    #[test]
    fn parse_table_keeps_last_instance() {
        let mut bytes = tlv(7, &[1, 0, 0, 0]);
        bytes.extend(tlv(7, &[2, 0, 0, 0]));
        assert_eq!(find_tlv(&bytes, 7).unwrap(), Some(&[2, 0, 0, 0][..]));
    }

    #[test]
    fn scan_fixture_decodes_exact_words() {
        let words = [1u32, 2, 2412, 4, 5, 6, 7];
        let value: Vec<_> = words.into_iter().flat_map(u32::to_le_bytes).collect();
        let event = Event::from_tlvs(EventId(99), tlv(Scan::TAG, &value)).unwrap();
        let got = Decoder::<Scan>::new(EventId(99)).decode(event).unwrap();
        assert_eq!(got.channel_freq, 2412);
        assert_eq!(got.tsf_timestamp, 7);
    }

    #[test]
    fn every_truncation_of_fixed_event_is_rejected() {
        let value = [0u8; Scan::MIN_LEN];
        for n in 0..Scan::MIN_LEN {
            let mut bytes = tlv(Scan::TAG, &value[..n]);
            while !bytes.len().is_multiple_of(4) {
                bytes.push(0);
            }
            let event = Event::from_tlvs(EventId(1), bytes).unwrap();
            assert_eq!(
                Decoder::<Scan>::new(EventId(1)).decode(event),
                Err(WmiError::Malformed)
            );
        }
    }

    #[test]
    fn mgmt_rx_rejects_frame_shorter_than_advertised() {
        let mut hdr = [0u8; 68];
        hdr[16..20].copy_from_slice(&8u32.to_le_bytes());
        let mut bytes = tlv(MgmtRx::TAG, &hdr);
        bytes.extend(tlv(WMI_TAG_ARRAY_BYTE.0, &[0; 4]));
        let event = Event::from_tlvs(EventId(1), bytes).unwrap();
        assert!(Decoder::<MgmtRx>::new(EventId(1)).decode(event).is_err());
    }

    #[test]
    fn arbitrary_truncated_tlv_streams_never_panic() {
        let mut bytes = [0u8; 257];
        let mut state = 0x1234_5678u32;
        for byte in &mut bytes {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *byte = (state >> 24) as u8;
        }
        for end in 0..=bytes.len() {
            for result in TlvIter::new(&bytes[..end]) {
                if result.is_err() {
                    break;
                }
            }
        }
    }

    #[test]
    fn service_ready_fixture_exposes_capabilities_and_bitmap() {
        let mut fixed = [0u8; 128];
        fixed[28..32].copy_from_slice(&0xaabb_ccddu32.to_le_bytes());
        fixed[104..108].copy_from_slice(&2u32.to_le_bytes());
        let mut bytes = tlv(tags::WMI_TAG_SERVICE_READY_EVENT.0, &fixed);
        bytes.extend(tlv(tags::WMI_TAG_ARRAY_UINT32.0, &[0u8; 128]));
        let event = Event::from_tlvs(tags::WMI_SERVICE_READY_EVENTID, bytes).unwrap();
        let decoded = ServiceReadyDecoder.decode(event).unwrap();
        let fixed = decoded.fixed.unwrap();
        assert_eq!(fixed.phy_capability, 0xaabb_ccdd);
        assert_eq!(fixed.max_supported_macs, 2);
        assert!(decoded.service_bitmap.is_some());
    }
}
