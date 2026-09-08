//! Device-wide command encoders adjacent to WMI initialization in `wmi.c`.

use super::{EncodeCommand, TlvWriter, one};
use crate::tags::*;
use crate::{Command, WmiError};

/// Seeds supplied to Linux's `ath11k_wmi_pdev_lro_cfg` by the kernel RNG.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PdevLroConfig {
    pub pdev_id: u32,
    pub ipv4_hash_seed: [u32; 5],
    pub ipv6_hash_seed: [u32; 11],
}

impl EncodeCommand for PdevLroConfig {
    fn encode_command(&self) -> Result<Command, WmiError> {
        one(WMI_LRO_CONFIG_CMDID, WMI_TAG_LRO_INFO_CMD, |w| {
            // lro_enable and res remain zero in the zero-filled C skb.
            w.u32(0);
            w.u32(0);
            for value in self.ipv4_hash_seed {
                w.u32(value);
            }
            for value in self.ipv6_hash_seed {
                w.u32(value);
            }
            w.u32(self.pdev_id);
        })
    }
}

/// Valid values of `enum wmi_host_hw_mode_config_type` accepted by the C API.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum HardwareMode {
    Single = 0,
    Dbs = 1,
    SbsPassive = 2,
    Sbs = 3,
    DbsSbs = 4,
    DbsOrSbs = 5,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PdevSetHardwareMode {
    pub mode: HardwareMode,
}

impl EncodeCommand for PdevSetHardwareMode {
    fn encode_command(&self) -> Result<Command, WmiError> {
        one(
            WMI_PDEV_SET_HW_MODE_CMDID,
            WMI_TAG_PDEV_SET_HW_MODE_CMD,
            |w| {
                w.u32(0); // WMI_PDEV_ID_SOC
                w.u32(self.mode as u32);
                w.u32(0); // num_band_to_mac remains zero in the C skb
            },
        )
    }
}

/// The packed parameters copied by `ath11k_wmi_vdev_spectral_conf`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VdevSpectralConfig {
    pub vdev_id: u32,
    pub scan_count: u32,
    pub scan_period: u32,
    pub scan_priority: u32,
    pub scan_fft_size: u32,
    pub scan_gc_enable: u32,
    pub scan_restart_enable: u32,
    pub scan_noise_floor_ref: i32,
    pub scan_init_delay: u32,
    pub scan_nb_tone_threshold: u32,
    pub scan_str_bin_threshold: u32,
    pub scan_wb_report_mode: u32,
    pub scan_rssi_report_mode: u32,
    pub scan_rssi_threshold: u32,
    pub scan_power_format: u32,
    pub scan_report_mode: u32,
    pub scan_bin_scale: u32,
    pub scan_dbm_adjust: u32,
    pub scan_channel_mask: u32,
}

impl EncodeCommand for VdevSpectralConfig {
    fn encode_command(&self) -> Result<Command, WmiError> {
        one(
            WMI_VDEV_SPECTRAL_SCAN_CONFIGURE_CMDID,
            WMI_TAG_VDEV_SPECTRAL_CONFIGURE_CMD,
            |w| {
                for value in [
                    self.vdev_id,
                    self.scan_count,
                    self.scan_period,
                    self.scan_priority,
                    self.scan_fft_size,
                    self.scan_gc_enable,
                    self.scan_restart_enable,
                    self.scan_noise_floor_ref as u32,
                    self.scan_init_delay,
                    self.scan_nb_tone_threshold,
                    self.scan_str_bin_threshold,
                    self.scan_wb_report_mode,
                    self.scan_rssi_report_mode,
                    self.scan_rssi_threshold,
                    self.scan_power_format,
                    self.scan_report_mode,
                    self.scan_bin_scale,
                    self.scan_dbm_adjust,
                    self.scan_channel_mask,
                ] {
                    w.u32(value);
                }
            },
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum SpectralTrigger {
    Trigger = 1,
    Clear = 2,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum SpectralEnable {
    Enable = 1,
    Disable = 2,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VdevSpectralEnable {
    pub vdev_id: u32,
    pub trigger: SpectralTrigger,
    pub enable: SpectralEnable,
}

impl EncodeCommand for VdevSpectralEnable {
    fn encode_command(&self) -> Result<Command, WmiError> {
        one(
            WMI_VDEV_SPECTRAL_SCAN_ENABLE_CMDID,
            WMI_TAG_VDEV_SPECTRAL_ENABLE_CMD,
            |w| {
                w.u32(self.vdev_id);
                w.u32(self.trigger as u32);
                w.u32(self.enable as u32);
            },
        )
    }
}

/// Values below C's `WMI_DIRECT_BUF_MAX` validation boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum DirectBufferModule {
    Spectral = 0,
    Cfr = 1,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PdevDmaRingConfig {
    pub pdev_id: u32,
    pub module: DirectBufferModule,
    pub base_address: u64,
    pub head_index_address: u64,
    pub tail_index_address: u64,
    pub element_count: u32,
    pub buffer_size: u32,
    pub responses_per_event: u32,
    pub event_timeout_ms: u32,
}

impl EncodeCommand for PdevDmaRingConfig {
    fn encode_command(&self) -> Result<Command, WmiError> {
        one(
            WMI_PDEV_DMA_RING_CFG_REQ_CMDID,
            WMI_TAG_DMA_RING_CFG_REQ,
            |w| {
                w.u32(self.pdev_id);
                w.u32(self.module as u32);
                for address in [
                    self.base_address,
                    self.head_index_address,
                    self.tail_index_address,
                ] {
                    w.u32(address as u32);
                    w.u32((address >> 32) as u32);
                }
                w.u32(self.element_count);
                w.u32(self.buffer_size);
                w.u32(self.responses_per_event);
                w.u32(self.event_timeout_ms);
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;

    fn bytes(words: &[u32]) -> Vec<u8> {
        words.iter().flat_map(|word| word.to_le_bytes()).collect()
    }

    #[test]
    fn lro_layout_matches_pinned_source() {
        let command = PdevLroConfig {
            pdev_id: 7,
            ipv4_hash_seed: [1, 2, 3, 4, 5],
            ipv6_hash_seed: [6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
        }
        .encode_command()
        .unwrap();
        assert_eq!(command.id, WMI_LRO_CONFIG_CMDID);
        assert_eq!(
            command.tlvs(),
            bytes(&[
                76 | (0x1aa << 16),
                0,
                0,
                1,
                2,
                3,
                4,
                5,
                6,
                7,
                8,
                9,
                10,
                11,
                12,
                13,
                14,
                15,
                16,
                7,
            ])
        );
    }

    #[test]
    fn set_hardware_mode_layout_matches_pinned_source() {
        let command = PdevSetHardwareMode {
            mode: HardwareMode::DbsOrSbs,
        }
        .encode_command()
        .unwrap();
        assert_eq!(command.id, WMI_PDEV_SET_HW_MODE_CMDID);
        assert_eq!(command.tlvs(), bytes(&[12 | (0x203 << 16), 0, 5, 0]));
    }

    #[test]
    fn spectral_config_layout_matches_pinned_source() {
        let command = VdevSpectralConfig {
            vdev_id: 1,
            scan_count: 2,
            scan_period: 3,
            scan_priority: 4,
            scan_fft_size: 5,
            scan_gc_enable: 6,
            scan_restart_enable: 7,
            scan_noise_floor_ref: -96,
            scan_init_delay: 9,
            scan_nb_tone_threshold: 10,
            scan_str_bin_threshold: 11,
            scan_wb_report_mode: 12,
            scan_rssi_report_mode: 13,
            scan_rssi_threshold: 14,
            scan_power_format: 15,
            scan_report_mode: 16,
            scan_bin_scale: 17,
            scan_dbm_adjust: 18,
            scan_channel_mask: 19,
        }
        .encode_command()
        .unwrap();
        assert_eq!(command.id, WMI_VDEV_SPECTRAL_SCAN_CONFIGURE_CMDID);
        assert_eq!(
            command.tlvs(),
            bytes(&[
                76 | (0x8d << 16),
                1,
                2,
                3,
                4,
                5,
                6,
                7,
                (-96i32) as u32,
                9,
                10,
                11,
                12,
                13,
                14,
                15,
                16,
                17,
                18,
                19,
            ])
        );
    }

    #[test]
    fn spectral_enable_layout_matches_pinned_source() {
        let command = VdevSpectralEnable {
            vdev_id: 3,
            trigger: SpectralTrigger::Clear,
            enable: SpectralEnable::Enable,
        }
        .encode_command()
        .unwrap();
        assert_eq!(command.id, WMI_VDEV_SPECTRAL_SCAN_ENABLE_CMDID);
        assert_eq!(command.tlvs(), bytes(&[12 | (0x8e << 16), 3, 2, 1]));
    }

    #[test]
    fn dma_ring_config_layout_matches_pinned_source() {
        let command = PdevDmaRingConfig {
            pdev_id: 1,
            module: DirectBufferModule::Cfr,
            base_address: 0x1122_3344_5566_7788,
            head_index_address: 0x99aa_bbcc_ddee_ff00,
            tail_index_address: 0x0123_4567_89ab_cdef,
            element_count: 8,
            buffer_size: 9,
            responses_per_event: 10,
            event_timeout_ms: 11,
        }
        .encode_command()
        .unwrap();
        assert_eq!(command.id, WMI_PDEV_DMA_RING_CFG_REQ_CMDID);
        assert_eq!(
            command.tlvs(),
            bytes(&[
                48 | (0x2c0 << 16),
                1,
                1,
                0x5566_7788,
                0x1122_3344,
                0xddee_ff00,
                0x99aa_bbcc,
                0x89ab_cdef,
                0x0123_4567,
                8,
                9,
                10,
                11,
            ])
        );
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VdevChannelPower {
    pub center_freq: u32,
    pub tx_power: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VdevSetTpcPower {
    pub vdev_id: u32,
    pub psd_power: bool,
    pub eirp_power: u32,
    pub power_type_6ghz: u32,
    pub channels: alloc::vec::Vec<VdevChannelPower>,
}

impl EncodeCommand for VdevSetTpcPower {
    fn encode_command(&self) -> Result<Command, WmiError> {
        let mut w = TlvWriter::default();
        w.tlv(WMI_TAG_VDEV_SET_TPC_POWER_CMD, |w| {
            w.u32(self.vdev_id);
            w.u32(u32::from(self.psd_power));
            w.u32(self.eirp_power);
            w.u32(self.power_type_6ghz);
        })?;
        w.tlv(WMI_TAG_ARRAY_STRUCT, |w| {
            for channel in &self.channels {
                let _ = w.tlv(WMI_TAG_VDEV_CH_POWER_INFO, |w| {
                    w.u32(channel.center_freq);
                    w.u32(channel.tx_power);
                });
            }
        })?;
        w.finish(WMI_VDEV_SET_TPC_POWER_CMDID)
    }
}
