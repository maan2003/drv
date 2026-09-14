//! Encoders for the command-building tail of the pinned ath11k `wmi.c`.
use super::{EncodeCommand, TlvWriter, one};
use crate::tags::*;
use crate::{Command, WmiError};
use alloc::vec::Vec;

const WOW_BITMAP_BYTES: usize = 148;
const MAX_PNO_NETWORKS: usize = 16;
const MAX_PNO_CHANNELS: usize = 60;
const MAX_IPV6_OFFLOADS: usize = 16;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnitTest {
    pub vdev_id: u32,
    pub module_id: u32,
    pub diag_token: u32,
    pub args: Vec<u32>,
}
impl EncodeCommand for UnitTest {
    fn encode_command(&self) -> Result<Command, WmiError> {
        let count = u32::try_from(self.args.len()).map_err(|_| WmiError::Malformed)?;
        let mut w = TlvWriter::default();
        w.tlv(WMI_TAG_UNIT_TEST_CMD, |w| {
            w.u32(self.vdev_id);
            w.u32(self.module_id);
            w.u32(count);
            w.u32(self.diag_token)
        })?;
        w.tlv(WMI_TAG_ARRAY_UINT32, |w| {
            for &arg in &self.args {
                w.u32(arg)
            }
        })?;
        w.finish(WMI_UNIT_TEST_CMDID)
    }
}

/// The fixed DFS unit-test invocation used by `ath11k_wmi_simulate_radar`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SimulateRadar {
    pub vdev_id: u32,
    pub pdev_id: u32,
}
impl EncodeCommand for SimulateRadar {
    fn encode_command(&self) -> Result<Command, WmiError> {
        UnitTest {
            vdev_id: self.vdev_id,
            module_id: 0x2b,
            diag_token: 0xaa,
            args: alloc::vec![0, self.pdev_id, 0],
        }
        .encode_command()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DebugLogConfig {
    LogLevel(u32),
    VdevEnable(u32),
    VdevDisable(u32),
    VdevEnableBitmap(u32),
    ModuleEnableBitmap { value: u32, modules: [u32; 16] },
    WowModuleEnableBitmap { value: u32, modules: [u32; 16] },
}
impl EncodeCommand for DebugLogConfig {
    fn encode_command(&self) -> Result<Command, WmiError> {
        let (param, value, modules) = match self {
            Self::LogLevel(v) => (1, *v, None),
            Self::VdevEnable(v) => (2, *v, None),
            Self::VdevDisable(v) => (3, *v, None),
            Self::VdevEnableBitmap(v) => (4, *v, None),
            Self::ModuleEnableBitmap { value, modules } => (5, *value, Some(modules)),
            Self::WowModuleEnableBitmap { value, modules } => (6, *value, Some(modules)),
        };
        let mut w = TlvWriter::default();
        w.tlv(WMI_TAG_DEBUG_LOG_CONFIG_CMD, |w| {
            w.u32(param);
            w.u32(value)
        })?;
        w.tlv(WMI_TAG_ARRAY_UINT32, |w| {
            for value in modules.into_iter().flatten() {
                w.u32(*value)
            }
            if modules.is_none() {
                w.zeros(16 * 4)
            }
        })?;
        w.finish(WMI_DBGLOG_CFG_CMDID)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HwDataFilter {
    pub vdev_id: u32,
    pub enabled: bool,
    pub filter_bitmap: u32,
}
impl EncodeCommand for HwDataFilter {
    fn encode_command(&self) -> Result<Command, WmiError> {
        one(WMI_HW_DATA_FILTER_CMDID, WMI_TAG_HW_DATA_FILTER_CMD, |w| {
            w.u32(self.vdev_id);
            w.u32(u32::from(self.enabled));
            w.u32(if self.enabled {
                self.filter_bitmap
            } else {
                u32::MAX
            })
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WowHostWakeup;
impl EncodeCommand for WowHostWakeup {
    fn encode_command(&self) -> Result<Command, WmiError> {
        one(
            WMI_WOW_HOSTWAKEUP_FROM_SLEEP_CMDID,
            WMI_TAG_WOW_HOSTWAKEUP_FROM_SLEEP_CMD,
            |w| w.u32(0),
        )
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WowEnable;
impl EncodeCommand for WowEnable {
    fn encode_command(&self) -> Result<Command, WmiError> {
        one(WMI_WOW_ENABLE_CMDID, WMI_TAG_WOW_ENABLE_CMD, |w| {
            w.u32(1);
            w.u32(0);
            w.u32(0)
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScanProbeRequestOui {
    pub mac_addr: [u8; 6],
}
impl EncodeCommand for ScanProbeRequestOui {
    fn encode_command(&self) -> Result<Command, WmiError> {
        let oui = u32::from(self.mac_addr[0]) << 16
            | u32::from(self.mac_addr[1]) << 8
            | u32::from(self.mac_addr[2]);
        one(
            WMI_SCAN_PROB_REQ_OUI_CMDID,
            WMI_TAG_SCAN_PROB_REQ_OUI_CMD,
            |w| w.u32(oui),
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum WowWakeEvent {
    BeaconMiss = 0,
    BetterAp,
    DeauthReceived,
    MagicPacketReceived,
    GtkError,
    FourWayHandshake,
    EapolReceived,
    NloDetected,
    DisassocReceived,
    PatternMatch,
    CsaIe,
    ProbeRequestWpsIe,
    AuthRequest,
    AssocRequest,
    Htt,
    RouterAdvertisement,
    HostAutoShutdown,
    IoacMagic,
    IoacShort,
    IoacExtend,
    IoacTimer,
    DfsRadar,
    Beacon,
    ClientKickout,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WowWakeEventConfig {
    pub vdev_id: u32,
    pub event: WowWakeEvent,
    pub enabled: bool,
}
impl EncodeCommand for WowWakeEventConfig {
    fn encode_command(&self) -> Result<Command, WmiError> {
        one(
            WMI_WOW_ENABLE_DISABLE_WAKE_EVENT_CMDID,
            WMI_TAG_WOW_ADD_DEL_EVT_CMD,
            |w| {
                w.u32(self.vdev_id);
                w.u32(u32::from(self.enabled));
                w.u32(1_u32 << (self.event as u8))
            },
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WowAddPattern {
    pub vdev_id: u32,
    pub pattern_id: u32,
    pub pattern: Vec<u8>,
    pub mask: Vec<u8>,
    pub offset: u32,
}
impl EncodeCommand for WowAddPattern {
    fn encode_command(&self) -> Result<Command, WmiError> {
        if self.pattern.len() != self.mask.len() || self.pattern.len() > WOW_BITMAP_BYTES {
            return Err(WmiError::Malformed);
        }
        let pattern_len = u32::try_from(self.pattern.len()).map_err(|_| WmiError::Malformed)?;
        let mut w = TlvWriter::default();
        w.tlv(WMI_TAG_WOW_ADD_PATTERN_CMD, |w| {
            w.u32(self.vdev_id);
            w.u32(self.pattern_id);
            w.u32(0)
        })?;
        w.tlv(WMI_TAG_ARRAY_STRUCT, |w| {
            let _ = w.tlv(WMI_TAG_WOW_BITMAP_PATTERN_T, |w| {
                w.bytes(&self.pattern);
                w.zeros(WOW_BITMAP_BYTES - self.pattern.len());
                w.bytes(&self.mask);
                w.zeros(WOW_BITMAP_BYTES - self.mask.len());
                w.u32(self.offset);
                w.u32(pattern_len);
                w.u32(pattern_len);
                w.u32(self.pattern_id)
            });
        })?;
        for _ in 0..3 {
            w.tlv(WMI_TAG_ARRAY_STRUCT, |_| {})?;
        }
        w.tlv(WMI_TAG_ARRAY_UINT32, |_| {})?;
        w.tlv(WMI_TAG_ARRAY_UINT32, |w| w.u32(0))?;
        w.finish(WMI_WOW_ADD_WAKE_PATTERN_CMDID)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WowDeletePattern {
    pub vdev_id: u32,
    pub pattern_id: u32,
}
impl EncodeCommand for WowDeletePattern {
    fn encode_command(&self) -> Result<Command, WmiError> {
        one(
            WMI_WOW_DEL_WAKE_PATTERN_CMDID,
            WMI_TAG_WOW_DEL_PATTERN_CMD,
            |w| {
                w.u32(self.vdev_id);
                w.u32(self.pattern_id);
                w.u32(0)
            },
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PnoNetwork {
    pub ssid: Vec<u8>,
    pub rssi_threshold: i32,
    pub broadcast_type: u32,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PnoStart {
    pub vdev_id: u32,
    pub networks: Vec<PnoNetwork>,
    pub channels: Vec<u32>,
    pub fast_scan_period: u32,
    pub slow_scan_period: u32,
    pub fast_scan_max_cycles: u32,
    pub passive: bool,
    pub delay_start_time: u32,
    pub active_dwell_time: u32,
    pub passive_dwell_time: u32,
    pub random_mac: Option<([u8; 6], [u8; 6])>,
}
impl EncodeCommand for PnoStart {
    fn encode_command(&self) -> Result<Command, WmiError> {
        if self.networks.is_empty()
            || self.networks.len() > MAX_PNO_NETWORKS
            || self.channels.len() > MAX_PNO_CHANNELS
            || self
                .networks
                .iter()
                .any(|n| n.ssid.is_empty() || n.ssid.len() > 32)
        {
            return Err(WmiError::Malformed);
        }
        let mut flags = (1 << 1) | (1 << 6);
        if self.passive {
            flags |= 1 << 8;
        }
        if self.random_mac.is_some() {
            flags |= (1 << 10) | (1 << 11);
        }
        let mut w = TlvWriter::default();
        w.tlv(WMI_TAG_NLO_CONFIG_CMD, |w| {
            for value in [
                flags,
                self.vdev_id,
                self.fast_scan_max_cycles,
                self.active_dwell_time,
                self.passive_dwell_time,
                0,
                0,
                0,
                0,
                self.fast_scan_period,
                self.slow_scan_period,
                self.networks.len() as u32,
                self.channels.len() as u32,
                self.delay_start_time,
            ] {
                w.u32(value);
            }
            if let Some((mac, mask)) = self.random_mac {
                w.mac(&mac);
                w.mac(&mask);
            } else {
                w.zeros(16);
            }
            w.zeros(8 * 4);
            w.u32(0);
            w.u32(0)
        })?;
        w.tlv(WMI_TAG_ARRAY_STRUCT, |w| {
            for network in &self.networks {
                let _ = w.tlv(WMI_TAG_ARRAY_BYTE, |w| {
                    w.u32(1);
                    w.u32(network.ssid.len() as u32);
                    w.bytes(&network.ssid);
                    w.zeros(32 - network.ssid.len());
                    w.u32(0);
                    w.u32(0);
                    w.u32(0);
                    w.u32(0);
                    if network.rssi_threshold != 0 && network.rssi_threshold > -300 {
                        w.u32(1);
                        w.u32(network.rssi_threshold as u32)
                    } else {
                        w.u32(0);
                        w.u32(0)
                    }
                    w.u32(1);
                    w.u32(network.broadcast_type)
                });
            }
        })?;
        w.tlv(WMI_TAG_ARRAY_UINT32, |w| {
            for &channel in &self.channels {
                w.u32(channel)
            }
        })?;
        w.finish(WMI_NETWORK_LIST_OFFLOAD_CONFIG_CMDID)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PnoStop {
    pub vdev_id: u32,
}
impl EncodeCommand for PnoStop {
    fn encode_command(&self) -> Result<Command, WmiError> {
        one(
            WMI_NETWORK_LIST_OFFLOAD_CONFIG_CMDID,
            WMI_TAG_NLO_CONFIG_CMD,
            |w| {
                w.u32(1);
                w.u32(self.vdev_id);
                w.zeros(104)
            },
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NsOffload {
    pub target_ipv6: [u8; 16],
    pub solicitation_ipv6: [u8; 16],
    pub target_mac: [u8; 6],
    pub anycast: bool,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArpNsOffload {
    pub vdev_id: u32,
    pub enabled: bool,
    pub ipv4: Vec<[u8; 4]>,
    pub ipv6: Vec<NsOffload>,
}
fn ns_tuple(w: &mut TlvWriter, value: Option<&NsOffload>) {
    let _ = w.tlv(WMI_TAG_NS_OFFLOAD_TUPLE, |w| {
        if let Some(value) = value {
            let mac_valid = value.target_mac != [0; 6];
            w.u32(1 | if mac_valid { 2 } else { 0 } | if value.anycast { 8 } else { 0 });
            w.bytes(&value.target_ipv6);
            w.zeros(16);
            w.bytes(&value.solicitation_ipv6);
            w.zeros(16);
            w.mac(&value.target_mac)
        } else {
            w.zeros(76)
        }
    });
}
impl EncodeCommand for ArpNsOffload {
    fn encode_command(&self) -> Result<Command, WmiError> {
        if self.ipv4.len() > 2 || self.ipv6.len() > MAX_IPV6_OFFLOADS {
            return Err(WmiError::Malformed);
        }
        let ext = self.ipv6.len().saturating_sub(2);
        let mut w = TlvWriter::default();
        w.tlv(WMI_TAG_SET_ARP_NS_OFFLOAD_CMD, |w| {
            w.u32(0);
            w.u32(self.vdev_id);
            w.u32(ext as u32)
        })?;
        w.tlv(WMI_TAG_ARRAY_STRUCT, |w| {
            for i in 0..2 {
                ns_tuple(w, self.enabled.then(|| self.ipv6.get(i)).flatten())
            }
        })?;
        w.tlv(WMI_TAG_ARRAY_STRUCT, |w| {
            for i in 0..2 {
                let _ = w.tlv(WMI_TAG_ARP_OFFLOAD_TUPLE, |w| {
                    if let Some(ip) = self.enabled.then(|| self.ipv4.get(i)).flatten() {
                        w.u32(1);
                        w.bytes(ip);
                        w.zeros(4);
                        w.zeros(8)
                    } else {
                        w.zeros(20)
                    }
                });
            }
        })?;
        if ext != 0 {
            w.tlv(WMI_TAG_ARRAY_STRUCT, |w| {
                for value in &self.ipv6[2..] {
                    ns_tuple(w, self.enabled.then_some(value))
                }
            })?;
        }
        w.finish(WMI_SET_ARP_NS_OFFLOAD_CMDID)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GtkRekey {
    Enable {
        vdev_id: u32,
        kek: [u8; 16],
        kck: [u8; 16],
        replay_counter: u64,
    },
    Disable {
        vdev_id: u32,
    },
    RequestStatus {
        vdev_id: u32,
    },
}
impl EncodeCommand for GtkRekey {
    fn encode_command(&self) -> Result<Command, WmiError> {
        let (vdev, flags) = match self {
            Self::Enable { vdev_id, .. } => (*vdev_id, 0x0100_0000),
            Self::Disable { vdev_id } => (*vdev_id, 0x0200_0000),
            Self::RequestStatus { vdev_id } => (*vdev_id, 0x0400_0000),
        };
        one(WMI_GTK_OFFLOAD_CMDID, WMI_TAG_GTK_OFFLOAD_CMD, |w| {
            w.u32(vdev);
            w.u32(flags);
            if let Self::Enable {
                kek,
                kck,
                replay_counter,
                ..
            } = self
            {
                w.bytes(kek);
                w.bytes(kck);
                w.bytes(&replay_counter.to_le_bytes())
            } else {
                w.zeros(40)
            }
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BiosSarTable {
    pub pdev_id: u32,
    pub values: [u8; 22],
}
impl EncodeCommand for BiosSarTable {
    fn encode_command(&self) -> Result<Command, WmiError> {
        let mut w = TlvWriter::default();
        w.tlv(WMI_TAG_PDEV_SET_BIOS_SAR_TABLE_CMD, |w| {
            w.u32(self.pdev_id);
            w.u32(22);
            w.u32(6)
        })?;
        w.byte_array(&self.values)?;
        w.tlv(WMI_TAG_ARRAY_BYTE, |w| w.zeros(8))?;
        w.finish(WMI_PDEV_SET_BIOS_SAR_TABLE_CMDID)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BiosGeoTable {
    pub pdev_id: u32,
}
impl EncodeCommand for BiosGeoTable {
    fn encode_command(&self) -> Result<Command, WmiError> {
        let mut w = TlvWriter::default();
        w.tlv(WMI_TAG_PDEV_SET_BIOS_GEO_TABLE_CMD, |w| {
            w.u32(self.pdev_id);
            w.u32(18)
        })?;
        w.tlv(WMI_TAG_ARRAY_BYTE, |w| w.zeros(20))?;
        w.finish(WMI_PDEV_SET_BIOS_GEO_TABLE_CMDID)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum KeepaliveMethod {
    NullFrame = 1,
    UnsolicitedArpResponse = 2,
    EthernetLoopback = 3,
    GratuitousArpRequest = 4,
    ManagementVendorAction = 5,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StaKeepalive {
    pub vdev_id: u32,
    pub enabled: bool,
    pub method: KeepaliveMethod,
    pub interval: u32,
    pub src_ipv4: u32,
    pub dest_ipv4: u32,
    pub dest_mac: [u8; 6],
}
impl EncodeCommand for StaKeepalive {
    fn encode_command(&self) -> Result<Command, WmiError> {
        let mut w = TlvWriter::default();
        w.tlv(WMI_TAG_STA_KEEPALIVE_CMD, |w| {
            w.u32(self.vdev_id);
            w.u32(u32::from(self.enabled));
            w.u32(self.method as u32);
            w.u32(self.interval)
        })?;
        w.tlv(WMI_TAG_STA_KEEPALIVE_ARP_RESPONSE, |w| {
            if matches!(
                self.method,
                KeepaliveMethod::UnsolicitedArpResponse | KeepaliveMethod::GratuitousArpRequest
            ) {
                w.u32(self.src_ipv4);
                w.u32(self.dest_ipv4);
                w.mac(&self.dest_mac)
            } else {
                w.zeros(16)
            }
        })?;
        w.finish(WMI_STA_KEEPALIVE_CMDID)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn header(tag: TlvTag, len: u16) -> [u8; 4] {
        ((u32::from(tag.0) << 16) | u32::from(len)).to_le_bytes()
    }

    #[test]
    fn fixed_commands_match_c_struct_layouts() {
        let command = HwDataFilter {
            vdev_id: 7,
            enabled: false,
            filter_bitmap: 3,
        }
        .encode_command()
        .unwrap();
        let mut expected = header(WMI_TAG_HW_DATA_FILTER_CMD, 12).to_vec();
        expected.extend_from_slice(&7_u32.to_le_bytes());
        expected.extend_from_slice(&0_u32.to_le_bytes());
        expected.extend_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(command.id, WMI_HW_DATA_FILTER_CMDID);
        assert_eq!(command.tlvs(), expected);

        let command = StaKeepalive {
            vdev_id: 3,
            enabled: true,
            method: KeepaliveMethod::UnsolicitedArpResponse,
            interval: 30,
            src_ipv4: 0x01020304,
            dest_ipv4: 0x05060708,
            dest_mac: [1, 2, 3, 4, 5, 6],
        }
        .encode_command()
        .unwrap();
        let mut expected = header(WMI_TAG_STA_KEEPALIVE_CMD, 16).to_vec();
        for v in [3, 1, 2, 30] {
            expected.extend_from_slice(&v_u32(v));
        }
        expected.extend_from_slice(&header(WMI_TAG_STA_KEEPALIVE_ARP_RESPONSE, 16));
        expected.extend_from_slice(&v_u32(0x01020304));
        expected.extend_from_slice(&v_u32(0x05060708));
        expected.extend_from_slice(&[1, 2, 3, 4, 5, 6, 0, 0]);
        assert_eq!(command.tlvs(), expected);
    }
    fn v_u32(v: u32) -> [u8; 4] {
        v.to_le_bytes()
    }

    #[test]
    fn unit_test_and_debuglog_match_nested_c_tlvs() {
        let command = UnitTest {
            vdev_id: 1,
            module_id: 0x2b,
            diag_token: 0xaa,
            args: vec![0, 2, 0],
        }
        .encode_command()
        .unwrap();
        assert_eq!(
            command.tlvs(),
            &[
                16, 0, 0x47, 1, 1, 0, 0, 0, 0x2b, 0, 0, 0, 3, 0, 0, 0, 0xaa, 0, 0, 0, 12, 0, 16, 0,
                0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0
            ]
        );

        let command = DebugLogConfig::LogLevel(4).encode_command().unwrap();
        assert_eq!(command.tlvs().len(), 80);
        assert_eq!(
            &command.tlvs()[..16],
            &[8, 0, 0xbe, 0, 1, 0, 0, 0, 4, 0, 0, 0, 64, 0, 16, 0]
        );
        assert!(command.tlvs()[16..].iter().all(|&b| b == 0));
    }

    #[test]
    fn wow_pattern_has_fixed_148_byte_buffers_and_tail_arrays() {
        let command = WowAddPattern {
            vdev_id: 1,
            pattern_id: 9,
            pattern: vec![0xaa, 0xbb],
            mask: vec![0xff, 0],
            offset: 4,
        }
        .encode_command()
        .unwrap();
        assert_eq!(command.id, WMI_WOW_ADD_WAKE_PATTERN_CMDID);
        assert_eq!(command.tlvs().len(), 360);
        assert_eq!(&command.tlvs()[20..28], &[56, 1, 0xb4, 0, 0xaa, 0xbb, 0, 0]);
        assert_eq!(&command.tlvs()[172..176], &[0xff, 0, 0, 0]);
        assert_eq!(
            &command.tlvs()[320..336],
            &[4, 0, 0, 0, 2, 0, 0, 0, 2, 0, 0, 0, 9, 0, 0, 0]
        );
        assert_eq!(
            &command.tlvs()[336..],
            &[
                0, 0, 0x12, 0, 0, 0, 0x12, 0, 0, 0, 0x12, 0, 0, 0, 16, 0, 4, 0, 16, 0, 0, 0, 0, 0
            ]
        );
    }

    #[test]
    fn pno_start_matches_c_fixed_and_nested_layouts() {
        let command = PnoStart {
            vdev_id: 3,
            networks: vec![PnoNetwork {
                ssid: b"ab".to_vec(),
                rssi_threshold: -42,
                broadcast_type: 2,
            }],
            channels: vec![2412, 5180],
            fast_scan_period: 10,
            slow_scan_period: 20,
            fast_scan_max_cycles: 4,
            passive: true,
            delay_start_time: 30,
            active_dwell_time: 40,
            passive_dwell_time: 110,
            random_mac: None,
        }
        .encode_command()
        .unwrap();
        assert_eq!(command.id, WMI_NETWORK_LIST_OFFLOAD_CONFIG_CMDID);
        assert_eq!(command.tlvs().len(), 208);
        assert_eq!(
            &command.tlvs()[..16],
            &[112, 0, 0x90, 0, 0x42, 1, 0, 0, 3, 0, 0, 0, 4, 0, 0, 0]
        );
        assert_eq!(
            &command.tlvs()[40..60],
            &[
                10, 0, 0, 0, 20, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 30, 0, 0, 0
            ]
        );
        assert_eq!(
            &command.tlvs()[116..128],
            &[76, 0, 0x12, 0, 72, 0, 0x11, 0, 1, 0, 0, 0]
        );
        assert_eq!(&command.tlvs()[128..136], &[2, 0, 0, 0, b'a', b'b', 0, 0]);
        assert_eq!(
            &command.tlvs()[188..200],
            &[1, 0, 0, 0, 2, 0, 0, 0, 8, 0, 16, 0]
        );
        assert_eq!(&command.tlvs()[200..], &[0x6c, 9, 0, 0, 0x3c, 20, 0, 0]);
    }

    #[test]
    fn arp_ns_and_gtk_match_c_packed_layouts() {
        let command = ArpNsOffload {
            vdev_id: 5,
            enabled: true,
            ipv4: vec![[192, 0, 2, 1]],
            ipv6: vec![NsOffload {
                target_ipv6: [0x11; 16],
                solicitation_ipv6: [0x22; 16],
                target_mac: [1, 2, 3, 4, 5, 6],
                anycast: true,
            }],
        }
        .encode_command()
        .unwrap();
        assert_eq!(command.id, WMI_SET_ARP_NS_OFFLOAD_CMDID);
        assert_eq!(command.tlvs().len(), 232);
        assert_eq!(
            &command.tlvs()[..20],
            &[
                12, 0, 0x7b, 0, 0, 0, 0, 0, 5, 0, 0, 0, 0, 0, 0, 0, 160, 0, 0x12, 0
            ]
        );
        assert_eq!(&command.tlvs()[20..28], &[76, 0, 0x7d, 0, 11, 0, 0, 0]);
        assert_eq!(&command.tlvs()[28..44], &[0x11; 16]);
        assert_eq!(&command.tlvs()[60..76], &[0x22; 16]);
        assert_eq!(&command.tlvs()[100..108], &[76, 0, 0x7d, 0, 0, 0, 0, 0]);
        assert_eq!(
            &command.tlvs()[180..192],
            &[48, 0, 0x12, 0, 20, 0, 0x7c, 0, 1, 0, 0, 0]
        );
        assert_eq!(&command.tlvs()[192..196], &[192, 0, 2, 1]);

        let command = GtkRekey::Enable {
            vdev_id: 7,
            kek: [0xaa; 16],
            kck: [0xbb; 16],
            replay_counter: 0x0102_0304_0506_0708,
        }
        .encode_command()
        .unwrap();
        assert_eq!(command.tlvs().len(), 52);
        assert_eq!(
            &command.tlvs()[..12],
            &[48, 0, 0x5b, 0, 7, 0, 0, 0, 0, 0, 0, 1]
        );
        assert_eq!(&command.tlvs()[12..28], &[0xaa; 16]);
        assert_eq!(&command.tlvs()[28..44], &[0xbb; 16]);
        assert_eq!(&command.tlvs()[44..], &[8, 7, 6, 5, 4, 3, 2, 1]);
    }

    #[test]
    fn c_boundary_checks_return_errors_instead_of_panicking() {
        assert_eq!(
            WowAddPattern {
                vdev_id: 0,
                pattern_id: 0,
                pattern: vec![0; 149],
                mask: vec![0; 149],
                offset: 0
            }
            .encode_command(),
            Err(WmiError::Malformed)
        );
        let empty = PnoStart {
            vdev_id: 0,
            networks: vec![],
            channels: vec![],
            fast_scan_period: 0,
            slow_scan_period: 0,
            fast_scan_max_cycles: 0,
            passive: false,
            delay_start_time: 0,
            active_dwell_time: 0,
            passive_dwell_time: 0,
            random_mac: None,
        };
        assert_eq!(empty.encode_command(), Err(WmiError::Malformed));
        assert_eq!(
            ArpNsOffload {
                vdev_id: 0,
                enabled: true,
                ipv4: vec![[0; 4]; 3],
                ipv6: vec![]
            }
            .encode_command(),
            Err(WmiError::Malformed)
        );
    }

    #[test]
    fn sar_and_geo_use_c_aligned_reserved_arrays() {
        let sar = BiosSarTable {
            pdev_id: 2,
            values: [0x5a; 22],
        }
        .encode_command()
        .unwrap();
        assert_eq!(sar.tlvs().len(), 56);
        assert_eq!(&sar.tlvs()[16..20], &[24, 0, 0x11, 0]);
        assert_eq!(&sar.tlvs()[20..42], &[0x5a; 22]);
        assert_eq!(&sar.tlvs()[44..48], &[8, 0, 0x11, 0]);
        let geo = BiosGeoTable { pdev_id: 2 }.encode_command().unwrap();
        assert_eq!(geo.tlvs().len(), 36);
        assert_eq!(&geo.tlvs()[12..16], &[20, 0, 0x11, 0]);
    }
}
