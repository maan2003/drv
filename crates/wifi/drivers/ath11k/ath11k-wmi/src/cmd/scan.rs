use super::{EncodeCommand, TlvWriter, trace_branch, trace_field};
use crate::tags::*;
use crate::trace::TraceSink;
use crate::{Command, WmiError};
use alloc::vec::Vec;

#[cfg(feature = "proptest")]
use super::CommandStrategy;
#[cfg(feature = "proptest")]
use proptest::prelude::*;

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
    fn trace_fields(&self, sink: &mut dyn TraceSink) {
        for (name, value) in [
            ("ScanStart.scan_id", self.scan_id),
            ("ScanStart.scan_requester_id", self.scan_requester_id),
            ("ScanStart.vdev_id", self.vdev_id),
            ("ScanStart.scan_priority", self.scan_priority),
            ("ScanStart.notify_scan_events", self.notify_scan_events),
            ("ScanStart.control_flags_ext", self.control_flags_ext),
            ("ScanStart.dwell_time_active", self.dwell_time_active),
            (
                "ScanStart.dwell_time_active_2ghz",
                self.dwell_time_active_2ghz,
            ),
            ("ScanStart.dwell_time_passive", self.dwell_time_passive),
            (
                "ScanStart.dwell_time_active_6ghz",
                self.dwell_time_active_6ghz,
            ),
            (
                "ScanStart.dwell_time_passive_6ghz",
                self.dwell_time_passive_6ghz,
            ),
            ("ScanStart.min_rest_time", self.min_rest_time),
            ("ScanStart.max_rest_time", self.max_rest_time),
            ("ScanStart.repeat_probe_time", self.repeat_probe_time),
            ("ScanStart.probe_spacing_time", self.probe_spacing_time),
            ("ScanStart.idle_time", self.idle_time),
            ("ScanStart.max_scan_time", self.max_scan_time),
            ("ScanStart.probe_delay", self.probe_delay),
            ("ScanStart.burst_duration", self.burst_duration),
            ("ScanStart.n_probes", self.n_probes),
        ] {
            trace_field(sink, name, value);
        }
        for (name, taken) in [
            ("ScanStart.event_flags.started", self.event_flags.started),
            (
                "ScanStart.event_flags.completed",
                self.event_flags.completed,
            ),
            (
                "ScanStart.event_flags.bss_channel",
                self.event_flags.bss_channel,
            ),
            (
                "ScanStart.event_flags.foreign_channel",
                self.event_flags.foreign_channel,
            ),
            ("ScanStart.event_flags.dequeued", self.event_flags.dequeued),
            (
                "ScanStart.event_flags.preempted",
                self.event_flags.preempted,
            ),
            (
                "ScanStart.event_flags.start_failed",
                self.event_flags.start_failed,
            ),
            (
                "ScanStart.event_flags.restarted",
                self.event_flags.restarted,
            ),
            (
                "ScanStart.event_flags.foreign_channel_exit",
                self.event_flags.foreign_channel_exit,
            ),
            (
                "ScanStart.event_flags.suspended",
                self.event_flags.suspended,
            ),
            ("ScanStart.event_flags.resumed", self.event_flags.resumed),
            (
                "ScanStart.control_flags.passive",
                self.control_flags.passive,
            ),
            (
                "ScanStart.control_flags.strict_passive",
                self.control_flags.strict_passive,
            ),
            (
                "ScanStart.control_flags.promiscuous",
                self.control_flags.promiscuous,
            ),
            (
                "ScanStart.control_flags.capture_phy_error",
                self.control_flags.capture_phy_error,
            ),
            (
                "ScanStart.control_flags.half_rate",
                self.control_flags.half_rate,
            ),
            (
                "ScanStart.control_flags.quarter_rate",
                self.control_flags.quarter_rate,
            ),
            (
                "ScanStart.control_flags.cck_rates",
                self.control_flags.cck_rates,
            ),
            (
                "ScanStart.control_flags.ofdm_rates",
                self.control_flags.ofdm_rates,
            ),
            (
                "ScanStart.control_flags.channel_stat_event",
                self.control_flags.channel_stat_event,
            ),
            (
                "ScanStart.control_flags.filter_probe_request",
                self.control_flags.filter_probe_request,
            ),
            (
                "ScanStart.control_flags.broadcast_probe",
                self.control_flags.broadcast_probe,
            ),
            (
                "ScanStart.control_flags.offchannel_mgmt_tx",
                self.control_flags.offchannel_mgmt_tx,
            ),
            (
                "ScanStart.control_flags.offchannel_data_tx",
                self.control_flags.offchannel_data_tx,
            ),
            (
                "ScanStart.control_flags.force_active_dfs",
                self.control_flags.force_active_dfs,
            ),
            (
                "ScanStart.control_flags.add_tpc_ie",
                self.control_flags.add_tpc_ie,
            ),
            (
                "ScanStart.control_flags.add_ds_ie",
                self.control_flags.add_ds_ie,
            ),
            (
                "ScanStart.control_flags.spoofed_mac",
                self.control_flags.spoofed_mac,
            ),
            (
                "ScanStart.control_flags.random_sequence",
                self.control_flags.random_sequence,
            ),
            (
                "ScanStart.control_flags.ie_whitelist",
                self.control_flags.ie_whitelist,
            ),
        ] {
            trace_branch(sink, name, taken);
        }
        trace_field(
            sink,
            "ScanStart.control_flags.adaptive_dwell_mode",
            self.control_flags.adaptive_dwell_mode,
        );

        trace_field(sink, "ScanStart.mac_addr.len", self.mac_addr.len() as u64);
        for value in self.mac_addr {
            trace_field(sink, "ScanStart.mac_addr[]", value);
        }
        trace_field(sink, "ScanStart.mac_mask.len", self.mac_mask.len() as u64);
        for value in self.mac_mask {
            trace_field(sink, "ScanStart.mac_mask[]", value);
        }
        trace_field(sink, "ScanStart.channels.len", self.channels.len() as u64);
        for &channel in &self.channels {
            trace_field(sink, "ScanStart.channels[]", channel);
        }
        trace_field(sink, "ScanStart.ssids.len", self.ssids.len() as u64);
        for ssid in &self.ssids {
            trace_field(sink, "ScanStart.ssids[].len", ssid.len() as u64);
            for &value in ssid {
                trace_field(sink, "ScanStart.ssids[][]", value);
            }
        }
        trace_field(sink, "ScanStart.bssids.len", self.bssids.len() as u64);
        for bssid in &self.bssids {
            trace_field(sink, "ScanStart.bssids[].len", bssid.len() as u64);
            for &value in bssid {
                trace_field(sink, "ScanStart.bssids[][]", value);
            }
        }
        trace_field(sink, "ScanStart.extra_ie.len", self.extra_ie.len() as u64);
        for &value in &self.extra_ie {
            trace_field(sink, "ScanStart.extra_ie[]", value);
        }
        trace_field(
            sink,
            "ScanStart.short_ssid_hints.len",
            self.short_ssid_hints.len() as u64,
        );
        for hint in &self.short_ssid_hints {
            trace_field(
                sink,
                "ScanStart.short_ssid_hints[].freq_flags",
                hint.freq_flags,
            );
            trace_field(
                sink,
                "ScanStart.short_ssid_hints[].short_ssid",
                hint.short_ssid,
            );
        }
        trace_field(
            sink,
            "ScanStart.bssid_hints.len",
            self.bssid_hints.len() as u64,
        );
        for hint in &self.bssid_hints {
            trace_field(sink, "ScanStart.bssid_hints[].freq_flags", hint.freq_flags);
            trace_field(
                sink,
                "ScanStart.bssid_hints[].bssid.len",
                hint.bssid.len() as u64,
            );
            for value in hint.bssid {
                trace_field(sink, "ScanStart.bssid_hints[].bssid[]", value);
            }
        }
    }

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
    fn trace_fields(&self, sink: &mut dyn TraceSink) {
        trace_field(sink, "ScanChannelList.pdev_id", self.pdev_id);
        trace_branch(sink, "ScanChannelList.append", self.append);
        trace_field(
            sink,
            "ScanChannelList.channels.len",
            self.channels.len() as u64,
        );
        for channel in &self.channels {
            trace_field(sink, "ScanChannelList.channels[].mhz", channel.mhz);
            trace_field(
                sink,
                "ScanChannelList.channels[].center_freq1",
                channel.center_freq1,
            );
            trace_field(
                sink,
                "ScanChannelList.channels[].center_freq2",
                channel.center_freq2,
            );
            for (name, taken) in [
                ("ScanChannelList.channels[].passive", channel.passive),
                ("ScanChannelList.channels[].allow_ht", channel.allow_ht),
                ("ScanChannelList.channels[].allow_vht", channel.allow_vht),
                ("ScanChannelList.channels[].allow_he", channel.allow_he),
                ("ScanChannelList.channels[].half_rate", channel.half_rate),
                (
                    "ScanChannelList.channels[].quarter_rate",
                    channel.quarter_rate,
                ),
                ("ScanChannelList.channels[].psc", channel.psc),
                ("ScanChannelList.channels[].dfs", channel.dfs),
            ] {
                trace_branch(sink, name, taken);
            }
            trace_field(
                sink,
                "ScanChannelList.channels[].phy_mode",
                channel.phy_mode,
            );
            trace_field(
                sink,
                "ScanChannelList.channels[].min_power",
                channel.min_power,
            );
            trace_field(
                sink,
                "ScanChannelList.channels[].max_power",
                channel.max_power,
            );
            trace_field(
                sink,
                "ScanChannelList.channels[].max_reg_power",
                channel.max_reg_power,
            );
            trace_field(
                sink,
                "ScanChannelList.channels[].antenna_max",
                channel.antenna_max,
            );
            trace_field(
                sink,
                "ScanChannelList.channels[].reg_class_id",
                channel.reg_class_id,
            );
        }
    }

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

#[cfg(feature = "proptest")]
fn scan_event_flags_strategy() -> BoxedStrategy<ScanEventFlags> {
    any::<[bool; 11]>()
        .prop_map(|flags| ScanEventFlags {
            started: flags[0],
            completed: flags[1],
            bss_channel: flags[2],
            foreign_channel: flags[3],
            dequeued: flags[4],
            preempted: flags[5],
            start_failed: flags[6],
            restarted: flags[7],
            foreign_channel_exit: flags[8],
            suspended: flags[9],
            resumed: flags[10],
        })
        .boxed()
}

#[cfg(feature = "proptest")]
fn scan_control_flags_strategy() -> BoxedStrategy<ScanControlFlags> {
    any::<([bool; 19], u32)>()
        .prop_map(|(flags, adaptive_dwell_mode)| ScanControlFlags {
            passive: flags[0],
            strict_passive: flags[1],
            promiscuous: flags[2],
            capture_phy_error: flags[3],
            half_rate: flags[4],
            quarter_rate: flags[5],
            cck_rates: flags[6],
            ofdm_rates: flags[7],
            channel_stat_event: flags[8],
            filter_probe_request: flags[9],
            broadcast_probe: flags[10],
            offchannel_mgmt_tx: flags[11],
            offchannel_data_tx: flags[12],
            force_active_dfs: flags[13],
            add_tpc_ie: flags[14],
            add_ds_ie: flags[15],
            spoofed_mac: flags[16],
            random_sequence: flags[17],
            ie_whitelist: flags[18],
            adaptive_dwell_mode,
        })
        .boxed()
}

#[cfg(feature = "proptest")]
fn short_ssid_hint_strategy() -> BoxedStrategy<ScanShortSsidHint> {
    any::<(u32, u32)>()
        .prop_map(|(freq_flags, short_ssid)| ScanShortSsidHint {
            freq_flags,
            short_ssid,
        })
        .boxed()
}

#[cfg(feature = "proptest")]
fn bssid_hint_strategy() -> BoxedStrategy<ScanBssidHint> {
    any::<(u32, [u8; 6])>()
        .prop_map(|(freq_flags, bssid)| ScanBssidHint { freq_flags, bssid })
        .boxed()
}

#[cfg(feature = "proptest")]
impl CommandStrategy for ScanStart {
    fn strategy() -> BoxedStrategy<Self> {
        (
            (
                any::<[u32; 5]>(),
                scan_event_flags_strategy(),
                scan_control_flags_strategy(),
                any::<[u32; 15]>(),
                any::<[u8; 6]>(),
                any::<[u8; 6]>(),
            ),
            (
                prop::collection::vec(any::<u32>(), 0..=64),
                prop::collection::vec(
                    prop::collection::vec(any::<u8>(), 0..=MAX_SSID_LEN),
                    0..=MAX_SCAN_SSIDS,
                ),
                prop::collection::vec(any::<[u8; 6]>(), 0..=MAX_SCAN_BSSIDS),
                prop::collection::vec(any::<u8>(), 0..=256),
                prop::collection::vec(short_ssid_hint_strategy(), 0..=MAX_SCAN_HINTS),
                prop::collection::vec(bssid_hint_strategy(), 0..=MAX_SCAN_HINTS),
            ),
        )
            .prop_map(
                |(
                    (head, event_flags, control_flags, tail, mac_addr, mac_mask),
                    (channels, ssids, bssids, extra_ie, short_ssid_hints, bssid_hints),
                )| ScanStart {
                    scan_id: head[0],
                    scan_requester_id: head[1],
                    vdev_id: head[2],
                    scan_priority: head[3],
                    notify_scan_events: head[4],
                    event_flags,
                    control_flags,
                    control_flags_ext: tail[0],
                    dwell_time_active: tail[1],
                    dwell_time_active_2ghz: tail[2],
                    dwell_time_passive: tail[3],
                    dwell_time_active_6ghz: tail[4],
                    dwell_time_passive_6ghz: tail[5],
                    min_rest_time: tail[6],
                    max_rest_time: tail[7],
                    repeat_probe_time: tail[8],
                    probe_spacing_time: tail[9],
                    idle_time: tail[10],
                    max_scan_time: tail[11],
                    probe_delay: tail[12],
                    burst_duration: tail[13],
                    n_probes: tail[14],
                    mac_addr,
                    mac_mask,
                    channels,
                    ssids,
                    bssids,
                    extra_ie,
                    short_ssid_hints,
                    bssid_hints,
                },
            )
            .boxed()
    }
}

#[cfg(feature = "proptest")]
fn scan_channel_strategy() -> BoxedStrategy<ScanChannel> {
    any::<(u32, u32, u32, [bool; 8], u32, [u8; 5])>()
        .prop_map(
            |(mhz, center_freq1, center_freq2, flags, phy_mode, power)| ScanChannel {
                mhz,
                center_freq1,
                center_freq2,
                passive: flags[0],
                allow_ht: flags[1],
                allow_vht: flags[2],
                allow_he: flags[3],
                half_rate: flags[4],
                quarter_rate: flags[5],
                psc: flags[6],
                dfs: flags[7],
                phy_mode,
                min_power: power[0],
                max_power: power[1],
                max_reg_power: power[2],
                antenna_max: power[3],
                reg_class_id: power[4],
            },
        )
        .boxed()
}

#[cfg(feature = "proptest")]
impl CommandStrategy for ScanChannelList {
    fn strategy() -> BoxedStrategy<Self> {
        (
            any::<u32>(),
            any::<bool>(),
            prop::collection::vec(scan_channel_strategy(), 1..=64),
        )
            .prop_map(|(pdev_id, append, channels)| ScanChannelList {
                pdev_id,
                append,
                channels,
            })
            .boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::TraceEvent;
    use alloc::vec;

    #[derive(Default)]
    struct RecordingTrace(Vec<TraceEvent>);

    impl TraceSink for RecordingTrace {
        fn record(&mut self, event: TraceEvent) {
            self.0.push(event);
        }
    }

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

    #[test]
    fn scan_traces_use_stable_paths_and_preserve_sequence_order() {
        let request = ScanStart {
            scan_id: 1,
            scan_requester_id: 2,
            vdev_id: 3,
            scan_priority: 4,
            notify_scan_events: 5,
            event_flags: ScanEventFlags {
                completed: true,
                ..Default::default()
            },
            control_flags: ScanControlFlags {
                add_ds_ie: true,
                ..Default::default()
            },
            control_flags_ext: 6,
            dwell_time_active: 7,
            dwell_time_active_2ghz: 8,
            dwell_time_passive: 9,
            dwell_time_active_6ghz: 10,
            dwell_time_passive_6ghz: 11,
            min_rest_time: 12,
            max_rest_time: 13,
            repeat_probe_time: 14,
            probe_spacing_time: 15,
            idle_time: 16,
            max_scan_time: 17,
            probe_delay: 18,
            burst_duration: 19,
            n_probes: 20,
            mac_addr: [1, 2, 3, 4, 5, 6],
            mac_mask: [0xff; 6],
            channels: vec![2412, 5180],
            ssids: vec![vec![b'a'], vec![b'b', b'c']],
            bssids: vec![[6, 5, 4, 3, 2, 1]],
            extra_ie: vec![0xdd, 1],
            short_ssid_hints: vec![ScanShortSsidHint {
                freq_flags: 21,
                short_ssid: 22,
            }],
            bssid_hints: vec![ScanBssidHint {
                freq_flags: 23,
                bssid: [9; 6],
            }],
        };
        let mut trace = RecordingTrace::default();
        request.encode_command_with_trace(&mut trace).unwrap();

        assert!(trace.0.contains(&TraceEvent::Branch {
            name: "ScanStart.event_flags.completed",
            taken: true,
        }));
        assert!(trace.0.contains(&TraceEvent::Branch {
            name: "ScanStart.control_flags.add_ds_ie",
            taken: true,
        }));
        let channels: Vec<_> = trace
            .0
            .iter()
            .filter_map(|event| match event {
                TraceEvent::Field {
                    name: "ScanStart.channels[]",
                    value,
                } => Some(*value),
                _ => None,
            })
            .collect();
        assert_eq!(channels, [2412, 5180]);
        assert!(trace.0.contains(&TraceEvent::Field {
            name: "ScanStart.bssid_hints[].bssid[]",
            value: 9,
        }));

        let channel_list = ScanChannelList {
            pdev_id: 24,
            append: true,
            channels: vec![
                ScanChannel {
                    allow_ht: true,
                    ..Default::default()
                },
                ScanChannel::default(),
            ],
        };
        let mut trace = RecordingTrace::default();
        channel_list.encode_command_with_trace(&mut trace).unwrap();
        let allow_ht: Vec<_> = trace
            .0
            .iter()
            .filter_map(|event| match event {
                TraceEvent::Branch {
                    name: "ScanChannelList.channels[].allow_ht",
                    taken,
                } => Some(*taken),
                _ => None,
            })
            .collect();
        assert_eq!(allow_ht, [true, false]);
        assert!(trace.0.contains(&TraceEvent::Branch {
            name: "ScanChannelList.append",
            taken: true,
        }));
    }

    #[cfg(feature = "proptest")]
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(16))]

        #[test]
        fn scan_start_strategy_only_generates_encodable_requests(
            request in ScanStart::strategy()
        ) {
            prop_assert!(request.ssids.len() <= MAX_SCAN_SSIDS);
            prop_assert!(request.ssids.iter().all(|ssid| ssid.len() <= MAX_SSID_LEN));
            prop_assert!(request.bssids.len() <= MAX_SCAN_BSSIDS);
            prop_assert!(request.short_ssid_hints.len() <= MAX_SCAN_HINTS);
            prop_assert!(request.bssid_hints.len() <= MAX_SCAN_HINTS);
            prop_assert!(request.channels.len() * 4 <= u16::MAX as usize);
            prop_assert!(request.extra_ie.len().div_ceil(4) * 4 <= u16::MAX as usize);
            prop_assert!(request.encode_command().is_ok());
        }

        #[test]
        fn scan_channel_list_strategy_only_generates_encodable_requests(
            request in ScanChannelList::strategy()
        ) {
            prop_assert!(!request.channels.is_empty());
            prop_assert!(request.channels.len() * 28 - 4 <= u16::MAX as usize);
            prop_assert!(request.encode_command().is_ok());
        }
    }
}
