//! Source-shaped WMI receive waits used by the core lifecycle.

use alloc::vec::Vec;

use crate::tags::{
    WMI_READY_EVENTID, WMI_SERVICE_AVAILABLE_EVENTID, WMI_SERVICE_READY_EVENTID,
    WMI_SERVICE_READY_EXT_EVENTID, WMI_SERVICE_READY_EXT2_EVENTID, WMI_TAG_ARRAY_STRUCT,
    WMI_TAG_ARRAY_UINT32, WMI_TAG_DMA_RING_CAPABILITIES, WMI_TAG_HAL_REG_CAPABILITIES_EXT,
    WMI_TAG_HW_MODE_CAPABILITIES, WMI_TAG_MAC_PHY_CAPABILITIES, WMI_TAG_SERVICE_AVAILABLE_EVENT,
    WMI_TAG_SERVICE_READY_EVENT, WMI_TAG_SERVICE_READY_EXT_EVENT, WMI_TAG_SOC_HAL_REG_CAPABILITIES,
    WMI_TAG_SOC_MAC_PHY_HW_MODE_CAPS,
};
use crate::{Event, Transport, WmiError};

use super::{EventDecoder, Ready, ReadyDecoder, TlvIter, word};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceReadyFixed {
    pub firmware_build: u32,
    pub firmware_abi: [u32; 6],
    pub phy_capability: u32,
    pub max_fragment_entries: u32,
    pub num_rf_chains: u32,
    pub ht_capability: u32,
    pub vht_capability: u32,
    pub vht_supported_mcs: u32,
    pub hw_min_tx_power: u32,
    pub hw_max_tx_power: u32,
    pub system_capability: u32,
    pub max_beacon_ie_size: u32,
    pub num_memory_requests: u32,
    pub max_scan_channels: u32,
    pub max_supported_macs: u32,
    pub firmware_subfeature_caps: u32,
    pub num_dbs_hw_modes: u32,
    pub txrx_chainmask: u32,
    pub default_dbs_hw_mode_index: u32,
    pub num_msdu_descriptors: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceReady {
    /// Linux accepts a syntactically valid service-ready event without this TLV.
    pub fixed: Option<ServiceReadyFixed>,
    /// The first ARRAY_UINT32. Linux requires 32 words when it is present.
    pub service_bitmap: Option<[u32; 32]>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ServiceReadyDecoder;

impl EventDecoder for ServiceReadyDecoder {
    type Output = ServiceReady;

    fn decode(&self, event: Event) -> Result<ServiceReady, WmiError> {
        if event.id != WMI_SERVICE_READY_EVENTID {
            return Err(WmiError::Malformed);
        }
        let mut fixed = None;
        let mut bitmap = None;
        for tlv in TlvIter::new(event.tlvs()) {
            let tlv = tlv?;
            if tlv.tag == WMI_TAG_SERVICE_READY_EVENT.0 {
                if tlv.value.len() < 128 {
                    return Err(WmiError::Malformed);
                }
                let mut abi = [0; 6];
                for (i, item) in abi.iter_mut().enumerate() {
                    *item = word(tlv.value, 4 + i * 4)?;
                }
                fixed = Some(ServiceReadyFixed {
                    firmware_build: word(tlv.value, 0)?,
                    firmware_abi: abi,
                    phy_capability: word(tlv.value, 28)?,
                    max_fragment_entries: word(tlv.value, 32)?,
                    num_rf_chains: word(tlv.value, 36)?,
                    ht_capability: word(tlv.value, 40)?,
                    vht_capability: word(tlv.value, 44)?,
                    vht_supported_mcs: word(tlv.value, 48)?,
                    hw_min_tx_power: word(tlv.value, 52)?,
                    hw_max_tx_power: word(tlv.value, 56)?,
                    system_capability: word(tlv.value, 60)?,
                    max_beacon_ie_size: word(tlv.value, 68)?,
                    num_memory_requests: word(tlv.value, 72)?,
                    max_scan_channels: word(tlv.value, 76)?,
                    max_supported_macs: word(tlv.value, 104)?,
                    firmware_subfeature_caps: word(tlv.value, 108)?,
                    num_dbs_hw_modes: word(tlv.value, 112)?,
                    txrx_chainmask: word(tlv.value, 116)?,
                    default_dbs_hw_mode_index: word(tlv.value, 120)?,
                    num_msdu_descriptors: word(tlv.value, 124)?,
                });
            } else if tlv.tag == WMI_TAG_ARRAY_UINT32.0 && bitmap.is_none() {
                if tlv.value.len() < 128 {
                    return Err(WmiError::Malformed);
                }
                let mut words = [0; 32];
                for (i, item) in words.iter_mut().enumerate() {
                    *item = word(tlv.value, i * 4)?;
                }
                bitmap = Some(words);
            }
        }
        Ok(ServiceReady {
            fixed,
            service_bitmap: bitmap,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedTlv {
    pub tag: u16,
    pub value: Vec<u8>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ServiceReadyExt {
    pub fixed: Option<ServiceReadyExtFixed>,
    pub num_hw_modes: Option<u32>,
    pub num_phys: Option<u32>,
    pub hw_modes: Vec<HwModeCapability>,
    /// ARRAY_STRUCT groups retain their positional meaning and checked nesting.
    pub array_groups: Vec<Vec<OwnedTlv>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceReadyExtFixed {
    pub default_concurrent_scan_config: u32,
    pub default_firmware_config: u32,
    pub ppe_threshold: [u32; 10],
    pub he_capability: u32,
    pub mpdu_density: u32,
    pub max_bssid_rx_filters: u32,
    pub firmware_build_ext: u32,
    pub max_nlo_ssids: u32,
    pub max_bssid_indicator: u32,
    pub he_capability_ext: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HwModeCapability {
    pub hw_mode_id: u32,
    pub phy_id_map: u32,
    pub config_type: u32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ServiceReadyExtDecoder;

impl EventDecoder for ServiceReadyExtDecoder {
    type Output = ServiceReadyExt;

    fn decode(&self, event: Event) -> Result<ServiceReadyExt, WmiError> {
        if event.id != WMI_SERVICE_READY_EXT_EVENTID {
            return Err(WmiError::Malformed);
        }
        let mut out = ServiceReadyExt::default();
        for tlv in TlvIter::new(event.tlvs()) {
            let tlv = tlv?;
            match tlv.tag {
                x if x == WMI_TAG_SERVICE_READY_EXT_EVENT.0 => {
                    if tlv.value.len() < 76 {
                        return Err(WmiError::Malformed);
                    }
                    let mut ppe_threshold = [0; 10];
                    for (i, item) in ppe_threshold.iter_mut().enumerate() {
                        *item = word(tlv.value, 8 + i * 4)?;
                    }
                    out.fixed = Some(ServiceReadyExtFixed {
                        default_concurrent_scan_config: word(tlv.value, 0)?,
                        default_firmware_config: word(tlv.value, 4)?,
                        ppe_threshold,
                        he_capability: word(tlv.value, 48)?,
                        mpdu_density: word(tlv.value, 52)?,
                        max_bssid_rx_filters: word(tlv.value, 56)?,
                        firmware_build_ext: word(tlv.value, 60)?,
                        max_nlo_ssids: word(tlv.value, 64)?,
                        max_bssid_indicator: word(tlv.value, 68)?,
                        he_capability_ext: word(tlv.value, 72)?,
                    });
                }
                x if x == WMI_TAG_SOC_MAC_PHY_HW_MODE_CAPS.0 => {
                    if tlv.value.len() < 4 {
                        return Err(WmiError::Malformed);
                    }
                    out.num_hw_modes = Some(word(tlv.value, 0)?);
                }
                x if x == WMI_TAG_SOC_HAL_REG_CAPABILITIES.0 => {
                    if tlv.value.len() < 4 {
                        return Err(WmiError::Malformed);
                    }
                    out.num_phys = Some(word(tlv.value, 0)?);
                }
                x if x == WMI_TAG_ARRAY_STRUCT.0 => {
                    let mut group = Vec::new();
                    for nested in TlvIter::new(tlv.value) {
                        let nested = nested?;
                        group.push(OwnedTlv {
                            tag: nested.tag,
                            value: nested.value.to_vec(),
                        });
                    }
                    out.array_groups.push(group);
                }
                _ => {}
            }
        }
        validate_ext_groups(&out)?;
        if let Some(group) = out.array_groups.first() {
            out.hw_modes = group
                .iter()
                .map(|nested| {
                    Ok(HwModeCapability {
                        hw_mode_id: word(&nested.value, 0)?,
                        phy_id_map: word(&nested.value, 4)?,
                        config_type: word(&nested.value, 8)?,
                    })
                })
                .collect::<Result<Vec<_>, WmiError>>()?;
        }
        Ok(out)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ServiceReadyExt2 {
    pub dma_ring_capabilities: Vec<OwnedTlv>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ServiceReadyExt2Decoder;

impl EventDecoder for ServiceReadyExt2Decoder {
    type Output = ServiceReadyExt2;

    fn decode(&self, event: Event) -> Result<ServiceReadyExt2, WmiError> {
        if event.id != WMI_SERVICE_READY_EXT2_EVENTID {
            return Err(WmiError::Malformed);
        }
        let mut out = ServiceReadyExt2::default();
        for tlv in TlvIter::new(event.tlvs()) {
            let tlv = tlv?;
            if tlv.tag == WMI_TAG_ARRAY_STRUCT.0 && out.dma_ring_capabilities.is_empty() {
                for nested in TlvIter::new(tlv.value) {
                    let nested = nested?;
                    if nested.tag != WMI_TAG_DMA_RING_CAPABILITIES.0
                        || nested.value.len() < 20
                        || word(nested.value, 4)? >= 2
                    {
                        return Err(WmiError::Malformed);
                    }
                    out.dma_ring_capabilities.push(OwnedTlv {
                        tag: nested.tag,
                        value: nested.value.to_vec(),
                    });
                }
            }
        }
        Ok(out)
    }
}

fn validate_ext_groups(event: &ServiceReadyExt) -> Result<(), WmiError> {
    let mut total_phys = 0usize;
    if let Some(group) = event.array_groups.first() {
        let limit = event.num_hw_modes.unwrap_or(0) as usize;
        if group.len() > limit {
            return Err(WmiError::Malformed);
        }
        for nested in group {
            if nested.tag != WMI_TAG_HW_MODE_CAPABILITIES.0 || nested.value.len() < 12 {
                return Err(WmiError::Malformed);
            }
            total_phys += word(&nested.value, 4)?.count_ones() as usize;
        }
    }
    if let Some(group) = event.array_groups.get(1) {
        if group.len() > total_phys {
            return Err(WmiError::Malformed);
        }
        for nested in group {
            if nested.tag != WMI_TAG_MAC_PHY_CAPABILITIES.0 || nested.value.len() < 236 {
                return Err(WmiError::Malformed);
            }
        }
    }
    if let Some(group) = event.array_groups.get(2) {
        if group.len() > event.num_phys.unwrap_or(0) as usize {
            return Err(WmiError::Malformed);
        }
        for nested in group {
            if nested.tag != WMI_TAG_HAL_REG_CAPABILITIES_EXT.0 || nested.value.len() < 40 {
                return Err(WmiError::Malformed);
            }
        }
    }
    if let Some(group) = event.array_groups.get(6) {
        for nested in group {
            if nested.tag != WMI_TAG_DMA_RING_CAPABILITIES.0
                || nested.value.len() < 20
                || word(&nested.value, 4)? >= 2
            {
                return Err(WmiError::Malformed);
            }
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ServiceReadyState {
    pub service_ready: Option<ServiceReady>,
    pub service_ready_ext: Option<ServiceReadyExt>,
    pub service_ready_ext2: Option<ServiceReadyExt2>,
    pub service_available_bitmap: Option<[u32; 4]>,
}

/// Receive-side buffering makes lifecycle waits retain events that arrived first.
pub struct EventStream<T> {
    transport: T,
    pending: Vec<Event>,
}

impl<T: Transport> EventStream<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            pending: Vec::new(),
        }
    }

    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    pub fn into_inner(self) -> T {
        self.transport
    }

    pub fn pop_pending(&mut self) -> Option<Event> {
        if self.pending.is_empty() {
            None
        } else {
            Some(self.pending.remove(0))
        }
    }

    pub fn wait_for_service_ready(
        &mut self,
        deadline_ns: u64,
    ) -> Result<ServiceReadyState, WmiError> {
        let mut state = ServiceReadyState::default();
        loop {
            let event = self
                .transport
                .receive(deadline_ns)?
                .ok_or(WmiError::Timeout)?;
            if event.id == WMI_SERVICE_READY_EVENTID {
                state.service_ready = Some(ServiceReadyDecoder.decode(event)?);
            } else if event.id == WMI_SERVICE_AVAILABLE_EVENTID {
                if let Some(value) =
                    super::find_tlv(event.tlvs(), WMI_TAG_SERVICE_AVAILABLE_EVENT.0)?
                {
                    if value.len() < 20 {
                        return Err(WmiError::Malformed);
                    }
                    let mut bitmap = [0; 4];
                    for (i, item) in bitmap.iter_mut().enumerate() {
                        *item = word(value, 4 + i * 4)?;
                    }
                    state.service_available_bitmap = Some(bitmap);
                }
            } else if event.id == WMI_SERVICE_READY_EXT_EVENTID {
                state.service_ready_ext = Some(ServiceReadyExtDecoder.decode(event)?);
                // EXT2 service is service 220: bit 92 of the extended bitmap.
                let ext2 = state
                    .service_available_bitmap
                    .map(|bitmap| bitmap[2] & (1 << 28) != 0)
                    .unwrap_or(false);
                if !ext2 {
                    return Ok(state);
                }
            } else if event.id == WMI_SERVICE_READY_EXT2_EVENTID {
                state.service_ready_ext2 = Some(ServiceReadyExt2Decoder.decode(event)?);
                return Ok(state);
            } else {
                self.pending.push(event);
            }
        }
    }

    pub fn wait_for_unified_ready(&mut self, deadline_ns: u64) -> Result<Ready, WmiError> {
        if let Some(index) = self
            .pending
            .iter()
            .position(|event| event.id == WMI_READY_EVENTID)
        {
            return ReadyDecoder.decode(self.pending.remove(index));
        }
        loop {
            let event = self
                .transport
                .receive(deadline_ns)?
                .ok_or(WmiError::Timeout)?;
            if event.id == WMI_READY_EVENTID {
                return ReadyDecoder.decode(event);
            }
            self.pending.push(event);
        }
    }
}
