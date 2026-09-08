use alloc::vec::Vec;

use super::{EncodeCommand, TlvWriter};
use crate::tags::*;
use crate::{Command, WmiError};

/// The 72 payload words of `struct wmi_resource_config`, in declaration order.
/// Keeping the complete firmware-owned table together lets hardware-specific
/// code populate its source table without an unchecked byte representation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceConfig {
    pub words: [u32; 72],
}

impl Default for ResourceConfig {
    fn default() -> Self {
        Self { words: [0; 72] }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostMemoryChunk {
    pub request_id: u32,
    pub physical_address: u32,
    pub size: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BandToMac {
    pub pdev_id: u32,
    pub start_freq: u32,
    pub end_freq: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Init {
    pub resource_config: ResourceConfig,
    pub memory_chunks: Vec<HostMemoryChunk>,
    /// `None` is Linux `WMI_HOST_HW_MODE_MAX` (single-pdev WCN6750 path).
    pub hardware_mode: Option<u32>,
    pub bands: Vec<BandToMac>,
}

impl EncodeCommand for Init {
    fn encode_command(&self) -> Result<Command, WmiError> {
        if self.memory_chunks.len() > 32 || self.bands.len() > 3 {
            return Err(WmiError::Malformed);
        }
        let mut w = TlvWriter::default();
        w.tlv(WMI_TAG_INIT_CMD, |w| {
            w.zeros(24); // host_abi_vers is left zero by ath11k_init_cmd_send
            w.u32(self.memory_chunks.len() as u32);
        })?;
        w.tlv(WMI_TAG_RESOURCE_CONFIG, |w| {
            for value in self.resource_config.words {
                w.u32(value);
            }
        })?;

        // wmi.c's chunk structure advertises sizeof(struct), including its
        // header, as its TLV length. Preserve that protocol quirk verbatim.
        w.header(WMI_TAG_ARRAY_STRUCT, (self.memory_chunks.len() * 16) as u16);
        for chunk in &self.memory_chunks {
            w.header(WMI_TAG_WLAN_HOST_MEMORY_CHUNK, 16);
            w.u32(chunk.request_id);
            w.u32(chunk.physical_address);
            w.u32(chunk.size);
        }
        // When any chunk exists Linux allocates WMI_MAX_MEM_REQS entries,
        // although the array TLV names only the live entries. The zero tail is
        // part of skb->len and therefore part of the command bytes.
        if !self.memory_chunks.is_empty() {
            w.zeros((32usize.saturating_sub(self.memory_chunks.len())) * 16);
        }

        if let Some(mode) = self.hardware_mode {
            w.tlv(WMI_TAG_PDEV_SET_HW_MODE_CMD, |w| {
                w.u32(0); // pdev_id is zeroed by the C builder
                w.u32(mode);
                w.u32(self.bands.len() as u32);
            })?;
            w.tlv(WMI_TAG_ARRAY_STRUCT, |w| {
                for band in &self.bands {
                    let _ = w.tlv(WMI_TAG_PDEV_BAND_TO_MAC, |w| {
                        w.u32(band.pdev_id);
                        w.u32(band.start_freq);
                        w.u32(band.end_freq);
                    });
                }
            })?;
        }
        w.finish(WMI_INIT_CMDID)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_pdev_init_matches_fixed_c_layout() {
        let command = Init {
            resource_config: ResourceConfig::default(),
            memory_chunks: Vec::new(),
            hardware_mode: None,
            bands: Vec::new(),
        }
        .encode_command()
        .unwrap();
        assert_eq!(command.id, WMI_INIT_CMDID);
        assert_eq!(command.tlvs().len(), 32 + 292 + 4);
        assert_eq!(
            u32::from_le_bytes(command.tlvs()[32..36].try_into().unwrap()),
            (u32::from(WMI_TAG_RESOURCE_CONFIG.0) << 16) | 288
        );
    }
}
