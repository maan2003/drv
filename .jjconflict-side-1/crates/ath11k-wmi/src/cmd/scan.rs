use super::{EncodeCommand, TlvWriter};
use crate::tags::*;
use crate::{Command, WmiError};
use alloc::vec::Vec;

const MAX_SCAN_SSIDS: usize = 16;
const MAX_SCAN_BSSIDS: usize = 4;
const MAX_SCAN_HINTS: usize = 10;
const MAX_SSID_LEN: usize = 32;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ScanEventFlags {
    pub started: bool,
    pub completed: bool,
    pub bss_channel: bool,
    pub foreign_channel: bool,
    pub dequeued: bool,
    pub preempted: bool,
    pub start_failed: bool,
    pub restarted: bool,
    pub foreign_channel_exit: bool,
    pub suspended: bool,
    pub resumed: bool,
}

impl ScanEventFlags {
    fn bits(self) -> u32 {
        [
            self.started,
            self.completed,
            self.bss_channel,
            self.foreign_channel,
            self.dequeued,
            self.preempted,
            self.start_failed,
            self.restarted,
            self.foreign_channel_exit,
            self.suspended,
            self.resumed,
        ]
        .into_iter()
        .enumerate()
        .fold(0, |bits, (bit, set)| bits | (u32::from(set) << bit))
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ScanControlFlags {
    pub passive: bool,
    pub strict_passive: bool,
    pub promiscuous: bool,
    pub capture_phy_error: bool,
    pub half_rate: bool,
    pub quarter_rate: bool,
    pub cck_rates: bool,
    pub ofdm_rates: bool,
    pub channel_stat_event: bool,
    pub filter_probe_request: bool,
    pub broadcast_probe: bool,
    pub offchannel_mgmt_tx: bool,
    pub offchannel_data_tx: bool,
    pub force_active_dfs: bool,
    pub add_tpc_ie: bool,
    pub add_ds_ie: bool,
    pub spoofed_mac: bool,
    pub random_sequence: bool,
    pub ie_whitelist: bool,
    pub adaptive_dwell_mode: u32,
}

impl ScanControlFlags {
    fn bits(self) -> u32 {
        let flags = [
            (self.passive, 0x000001),
            (self.broadcast_probe, 0x000002),
            (self.cck_rates, 0x000004),
            (self.ofdm_rates, 0x000008),
            (self.channel_stat_event, 0x000010),
            (self.filter_probe_request, 0x000020),
            (self.promiscuous, 0x000100),
            (self.force_active_dfs, 0x000200),
            (self.add_tpc_ie, 0x000400),
            (self.add_ds_ie, 0x000800),
            (self.spoofed_mac, 0x001000),
            (self.offchannel_mgmt_tx, 0x002000),
            (self.offchannel_data_tx, 0x004000),
            (self.capture_phy_error, 0x008000),
            (self.strict_passive, 0x010000),
            (self.half_rate, 0x020000),
            (self.quarter_rate, 0x040000),
            (self.random_sequence, 0x080000),
            (self.ie_whitelist, 0x100000),
        ]
        .into_iter()
        .fold(0, |bits, (set, flag)| bits | if set { flag } else { 0 });
        flags | ((self.adaptive_dwell_mode << 21) & 0x00e0_0000)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScanShortSsidHint {
    pub freq_flags: u32,
    pub short_ssid: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScanBssidHint {
    pub freq_flags: u32,
    pub bssid: [u8; 6],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScanStart {
    pub scan_id: u32,
    pub scan_requester_id: u32,
    pub vdev_id: u32,
    pub scan_priority: u32,
    pub notify_scan_events: u32,
    pub event_flags: ScanEventFlags,
    pub control_flags: ScanControlFlags,
    pub control_flags_ext: u32,
    pub dwell_time_active: u32,
    pub dwell_time_active_2ghz: u32,
    pub dwell_time_passive: u32,
    pub dwell_time_active_6ghz: u32,
    pub dwell_time_passive_6ghz: u32,
    pub min_rest_time: u32,
    pub max_rest_time: u32,
    pub repeat_probe_time: u32,
    pub probe_spacing_time: u32,
    pub idle_time: u32,
    pub max_scan_time: u32,
    pub probe_delay: u32,
    pub burst_duration: u32,
    pub n_probes: u32,
    pub mac_addr: [u8; 6],
    pub mac_mask: [u8; 6],
    pub channels: Vec<u32>,
    pub ssids: Vec<Vec<u8>>,
    pub bssids: Vec<[u8; 6]>,
    pub extra_ie: Vec<u8>,
    pub short_ssid_hints: Vec<ScanShortSsidHint>,
    pub bssid_hints: Vec<ScanBssidHint>,
}

impl EncodeCommand for ScanStart {
    fn encode_command(&self) -> Result<Command, WmiError> {
        if self.ssids.len() > MAX_SCAN_SSIDS
            || self.ssids.iter().any(|ssid| ssid.len() > MAX_SSID_LEN)
            || self.bssids.len() > MAX_SCAN_BSSIDS
            || self.short_ssid_hints.len() > MAX_SCAN_HINTS
            || self.bssid_hints.len() > MAX_SCAN_HINTS
        {
            return Err(WmiError::Malformed);
        }
        let num_channels = u32::try_from(self.channels.len()).map_err(|_| WmiError::Malformed)?;
        let ie_len = u32::try_from(self.extra_ie.len()).map_err(|_| WmiError::Malformed)?;

        let mut w = TlvWriter::default();
        w.tlv(WMI_TAG_START_SCAN_CMD, |w| {
            for value in [
                self.scan_id,
                self.scan_requester_id,
                self.vdev_id,
                self.scan_priority,
                self.notify_scan_events | self.event_flags.bits(),
                self.dwell_time_active,
                self.dwell_time_passive,
                self.min_rest_time,
                self.max_rest_time,
                self.repeat_probe_time,
                self.probe_spacing_time,
                self.idle_time,
                self.max_scan_time,
                self.probe_delay,
                self.control_flags.bits(),
                self.burst_duration,
                num_channels,
                self.bssids.len() as u32,
                self.ssids.len() as u32,
                ie_len,
                self.n_probes,
            ] {
                w.u32(value);
            }
            w.mac(&self.mac_addr);
            w.mac(&self.mac_mask);
            w.zeros(8 * 4); // ie_bitmap
            w.u32(0); // num_vendor_oui
            w.u32(self.control_flags_ext);
            w.u32(self.dwell_time_active_2ghz);
            w.u32(self.dwell_time_active_6ghz);
            w.u32(self.dwell_time_passive_6ghz);
            w.u32(0); // scan_start_offset
        })?;
        w.tlv(WMI_TAG_ARRAY_UINT32, |w| {
            for channel in &self.channels {
                w.u32(*channel);
            }
        })?;
        w.tlv(WMI_TAG_ARRAY_FIXED_STRUCT, |w| {
            for ssid in &self.ssids {
                w.u32(ssid.len() as u32);
                w.bytes(ssid);
                w.zeros(MAX_SSID_LEN - ssid.len());
            }
        })?;
        w.tlv(WMI_TAG_ARRAY_FIXED_STRUCT, |w| {
            for bssid in &self.bssids {
                w.mac(bssid);
            }
        })?;

        // The pinned C stores this padded length in u16. Consequently lengths
        // above 65532 produce the same empty array TLV as the C implementation.
        let padded_ie_len = if self.extra_ie.len() <= (u16::MAX as usize & !3) {
            self.extra_ie.len().div_ceil(4) * 4
        } else {
            0
        };
        w.tlv(WMI_TAG_ARRAY_BYTE, |w| {
            if padded_ie_len != 0 {
                w.bytes(&self.extra_ie);
                w.zeros(padded_ie_len - self.extra_ie.len());
            }
        })?;
        if !self.short_ssid_hints.is_empty() {
            w.tlv(WMI_TAG_ARRAY_FIXED_STRUCT, |w| {
                for hint in &self.short_ssid_hints {
                    w.u32(hint.freq_flags);
                    w.u32(hint.short_ssid);
                }
            })?;
        }
        if !self.bssid_hints.is_empty() {
            w.tlv(WMI_TAG_ARRAY_FIXED_STRUCT, |w| {
                for hint in &self.bssid_hints {
                    w.u32(hint.freq_flags);
                    // The pinned source reverses ether_addr_copy's arguments;
                    // its zero-filled command buffer therefore retains zero.
                    w.mac(&[0; 6]);
                }
            })?;
        }
        w.finish(WMI_START_SCAN_CMDID)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ScanChannel {
    pub mhz: u32,
    pub center_freq1: u32,
    pub center_freq2: u32,
    pub passive: bool,
    pub allow_ht: bool,
    pub allow_vht: bool,
    pub allow_he: bool,
    pub half_rate: bool,
    pub quarter_rate: bool,
    pub psc: bool,
    pub dfs: bool,
    pub phy_mode: u32,
    pub min_power: u8,
    pub max_power: u8,
    pub max_reg_power: u8,
    pub antenna_max: u8,
    pub reg_class_id: u8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScanChannelList {
    pub pdev_id: u32,
    /// Set for every batch after the first when a channel list is split.
    pub append: bool,
    pub channels: Vec<ScanChannel>,
}

impl EncodeCommand for ScanChannelList {
    fn encode_command(&self) -> Result<Command, WmiError> {
        if self.channels.is_empty() {
            return Err(WmiError::Malformed);
        }
        let array_len = self
            .channels
            .len()
            .checked_mul(28)
            .and_then(|len| len.checked_sub(4))
            .and_then(|len| u16::try_from(len).ok())
            .ok_or(WmiError::Malformed)?;

        let mut w = TlvWriter::default();
        w.tlv(WMI_TAG_SCAN_CHAN_LIST_CMD, |w| {
            w.u32(self.channels.len() as u32);
            w.u32(u32::from(self.append));
            w.u32(self.pdev_id);
        })?;
        // This intentionally preserves the pinned source's len - TLV_HDR_SIZE
        // ARRAY_STRUCT header, while retaining every nested channel TLV byte.
        w.header(WMI_TAG_ARRAY_STRUCT, array_len);
        for channel in &self.channels {
            w.tlv(WMI_TAG_CHANNEL, |w| {
                let mut info = channel.phy_mode & 0x3f;
                if channel.passive {
                    info |= 1 << 7;
                }
                if channel.allow_he {
                    info |= 1 << 17;
                } else if channel.allow_vht {
                    info |= 1 << 12;
                } else if channel.allow_ht {
                    info |= 1 << 11;
                }
                if channel.half_rate {
                    info |= 1 << 14;
                }
                if channel.quarter_rate {
                    info |= 1 << 15;
                }
                if channel.psc {
                    info |= 1 << 18;
                }
                if channel.dfs {
                    info |= 1 << 10;
                }
                w.u32(channel.mhz);
                w.u32(channel.center_freq1);
                w.u32(channel.center_freq2);
                w.u32(info);
                w.u32(
                    u32::from(channel.min_power)
                        | (u32::from(channel.max_power) << 8)
                        | (u32::from(channel.max_reg_power) << 16)
                        | (u32::from(channel.reg_class_id) << 24),
                );
                w.u32(u32::from(channel.antenna_max) | (u32::from(channel.max_reg_power) << 8));
            })?;
        }
        w.finish(WMI_SCAN_CHAN_LIST_CMDID)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn bytes(words: &[u32]) -> Vec<u8> {
        words.iter().flat_map(|word| word.to_le_bytes()).collect()
    }

    #[test]
    fn scan_start_layout_matches_pinned_source() {
        let request = ScanStart {
            scan_id: 1,
            scan_requester_id: 2,
            vdev_id: 3,
            scan_priority: 4,
            notify_scan_events: 1 << 12,
            event_flags: ScanEventFlags {
                completed: true,
                resumed: true,
                ..Default::default()
            },
            control_flags: ScanControlFlags {
                passive: true,
                strict_passive: true,
                adaptive_dwell_mode: 3,
                ..Default::default()
            },
            control_flags_ext: 0x800,
            dwell_time_active: 5,
            dwell_time_active_2ghz: 6,
            dwell_time_passive: 7,
            dwell_time_active_6ghz: 8,
            dwell_time_passive_6ghz: 9,
            min_rest_time: 10,
            max_rest_time: 11,
            repeat_probe_time: 12,
            probe_spacing_time: 13,
            idle_time: 14,
            max_scan_time: 15,
            probe_delay: 16,
            burst_duration: 17,
            n_probes: 18,
            mac_addr: [1, 2, 3, 4, 5, 6],
            mac_mask: [0xff; 6],
            channels: vec![2412],
            ssids: vec![vec![b'a', b'b']],
            bssids: vec![[6, 5, 4, 3, 2, 1]],
            extra_ie: vec![0xdd, 1, 0xaa],
            short_ssid_hints: vec![ScanShortSsidHint {
                freq_flags: 20,
                short_ssid: 21,
            }],
            bssid_hints: vec![ScanBssidHint {
                freq_flags: 22,
                bssid: [9; 6],
            }],
        };
        let command = request.encode_command().unwrap();
        let mut expected = bytes(&[
            156 | (0x4d << 16),
            1,
            2,
            3,
            4,
            0x1402,
            5,
            7,
            10,
            11,
            12,
            13,
            14,
            15,
            16,
            0x0061_0001,
            17,
            1,
            1,
            1,
            3,
            18,
        ]);
        expected.extend_from_slice(&[1, 2, 3, 4, 5, 6, 0, 0]);
        expected.extend_from_slice(&[0xff; 6]);
        expected.extend_from_slice(&[0, 0]);
        expected.extend_from_slice(&bytes(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0x800, 6, 8, 9, 0]));
        expected.extend_from_slice(&bytes(&[4 | (0x10 << 16), 2412]));
        expected.extend_from_slice(&bytes(&[36 | (0x13 << 16), 2]));
        expected.extend_from_slice(b"ab");
        expected.extend_from_slice(&[0; 30]);
        expected.extend_from_slice(&bytes(&[8 | (0x13 << 16)]));
        expected.extend_from_slice(&[6, 5, 4, 3, 2, 1, 0, 0]);
        expected.extend_from_slice(&bytes(&[4 | (0x11 << 16)]));
        expected.extend_from_slice(&[0xdd, 1, 0xaa, 0]);
        expected.extend_from_slice(&bytes(&[8 | (0x13 << 16), 20, 21]));
        expected.extend_from_slice(&bytes(&[12 | (0x13 << 16), 22, 0, 0]));
        assert_eq!(command.id, WMI_START_SCAN_CMDID);
        assert_eq!(command.tlvs(), expected);
    }

    #[test]
    fn scan_channel_list_layout_matches_pinned_source() {
        let request = ScanChannelList {
            pdev_id: 7,
            append: true,
            channels: vec![ScanChannel {
                mhz: 5180,
                center_freq1: 5210,
                center_freq2: 0,
                passive: true,
                allow_ht: true,
                allow_vht: true,
                allow_he: false,
                half_rate: true,
                quarter_rate: false,
                psc: false,
                dfs: true,
                phy_mode: 9,
                min_power: 1,
                max_power: 2,
                max_reg_power: 3,
                antenna_max: 4,
                reg_class_id: 5,
            }],
        };
        let command = request.encode_command().unwrap();
        assert_eq!(command.id, WMI_SCAN_CHAN_LIST_CMDID);
        assert_eq!(
            command.tlvs(),
            bytes(&[
                12 | (0x4f << 16),
                1,
                1,
                7,
                24 | (0x12 << 16),
                24 | (0x50 << 16),
                5180,
                5210,
                0,
                9 | (1 << 7) | (1 << 10) | (1 << 12) | (1 << 14),
                0x0503_0201,
                0x0000_0304,
            ])
        );
    }

    #[test]
    fn scan_fixed_array_limits_match_c_types() {
        let mut request = ScanStart {
            scan_id: 0,
            scan_requester_id: 0,
            vdev_id: 0,
            scan_priority: 0,
            notify_scan_events: 0,
            event_flags: Default::default(),
            control_flags: Default::default(),
            control_flags_ext: 0,
            dwell_time_active: 0,
            dwell_time_active_2ghz: 0,
            dwell_time_passive: 0,
            dwell_time_active_6ghz: 0,
            dwell_time_passive_6ghz: 0,
            min_rest_time: 0,
            max_rest_time: 0,
            repeat_probe_time: 0,
            probe_spacing_time: 0,
            idle_time: 0,
            max_scan_time: 0,
            probe_delay: 0,
            burst_duration: 0,
            n_probes: 0,
            mac_addr: [0; 6],
            mac_mask: [0; 6],
            channels: vec![],
            ssids: vec![vec![0; 33]],
            bssids: vec![],
            extra_ie: vec![],
            short_ssid_hints: vec![],
            bssid_hints: vec![],
        };
        assert_eq!(request.encode_command(), Err(WmiError::Malformed));
        request.ssids = vec![vec![]; 17];
        assert_eq!(request.encode_command(), Err(WmiError::Malformed));
    }
}
