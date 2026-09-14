//! Control-plane command layouts ported from the pinned ath11k `wmi.c`.

use super::{EncodeCommand, TlvWriter, one};
use crate::tags::*;
use crate::{Command, WmiError};

macro_rules! u32_command {
    ($name:ident, $tag:ident, $id:ident, { $($field:ident),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
        pub struct $name { $(pub $field: u32),+ }
        impl EncodeCommand for $name {
            fn encode_command(&self) -> Result<Command, WmiError> {
                one($id, $tag, |w| { $(w.u32(self.$field);)+ })
            }
        }
    };
}

u32_command!(PdevSetRegdomain, WMI_TAG_PDEV_SET_REGDOMAIN_CMD,
    WMI_PDEV_SET_REGDOMAIN_CMDID, { pdev_id, reg_domain, reg_domain_2g,
    reg_domain_5g, conformance_test_limit_2g, conformance_test_limit_5g, dfs_domain });

#[cfg(test)]
mod reorder_queue_tests {
    use super::*;

    #[test]
    fn remove_matches_pinned_c_layout() {
        let command = PeerReorderQueueRemove {
            vdev_id: 7,
            peer_addr: [0x02, 0xd3, 0xb9, 0xdd, 0xc3, 0xd0],
            tid_mask: 0x102,
        }
        .encode_command()
        .unwrap();
        assert_eq!(command.id, WMI_PEER_REORDER_QUEUE_REMOVE_CMDID);
        assert_eq!(
            command.tlvs(),
            &[
                0x10, 0x00, 0x26, 0x02, 0x07, 0x00, 0x00, 0x00, 0x02, 0xd3, 0xb9, 0xdd, 0xc3, 0xd0,
                0x00, 0x00, 0x02, 0x01, 0x00, 0x00,
            ]
        );
        let reversed = crate::cmd::golden::reverse_map_semantic_command(command.id, command.tlvs())
            .unwrap()
            .unwrap();
        assert_eq!(reversed.encode_command().unwrap(), command);
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PeerFlushTids {
    pub vdev_id: u32,
    pub peer_addr: [u8; 6],
    pub tid_bitmap: u32,
}
impl EncodeCommand for PeerFlushTids {
    fn encode_command(&self) -> Result<Command, WmiError> {
        one(
            WMI_PEER_FLUSH_TIDS_CMDID,
            WMI_TAG_PEER_FLUSH_TIDS_CMD,
            |w| {
                w.u32(self.vdev_id);
                w.mac(&self.peer_addr);
                w.u32(self.tid_bitmap)
            },
        )
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PeerReorderQueueSetup {
    pub vdev_id: u32,
    pub peer_addr: [u8; 6],
    pub tid: u8,
    pub queue_address: u64,
    pub ba_window_size_valid: u8,
    pub ba_window_size: u32,
}
impl EncodeCommand for PeerReorderQueueSetup {
    fn encode_command(&self) -> Result<Command, WmiError> {
        one(
            WMI_PEER_REORDER_QUEUE_SETUP_CMDID,
            WMI_TAG_REORDER_QUEUE_SETUP_CMD,
            |w| {
                w.u32(self.vdev_id);
                w.mac(&self.peer_addr);
                w.u32(u32::from(self.tid));
                w.u32(self.queue_address as u32);
                w.u32((self.queue_address >> 32) as u32);
                w.u32(u32::from(self.tid));
                w.u32(u32::from(self.ba_window_size_valid));
                w.u32(self.ba_window_size);
            },
        )
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PeerReorderQueueRemove {
    pub vdev_id: u32,
    pub peer_addr: [u8; 6],
    pub tid_mask: u32,
}
impl EncodeCommand for PeerReorderQueueRemove {
    fn encode_command(&self) -> Result<Command, WmiError> {
        one(
            WMI_PEER_REORDER_QUEUE_REMOVE_CMDID,
            WMI_TAG_REORDER_QUEUE_REMOVE_CMD,
            |w| {
                w.u32(self.vdev_id);
                w.mac(&self.peer_addr);
                w.u32(self.tid_mask)
            },
        )
    }
}

u32_command!(StaPowerSaveMode, WMI_TAG_STA_POWERSAVE_MODE_CMD,
    WMI_STA_POWERSAVE_MODE_CMDID, { vdev_id, mode });
u32_command!(PdevBssChannelInfoRequest, WMI_TAG_PDEV_BSS_CHAN_INFO_REQUEST,
    WMI_PDEV_BSS_CHAN_INFO_REQUEST_CMDID, { request_type, pdev_id });

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ApPowerSavePeer {
    pub vdev_id: u32,
    pub peer_addr: [u8; 6],
    pub param: u32,
    pub value: u32,
}
impl EncodeCommand for ApPowerSavePeer {
    fn encode_command(&self) -> Result<Command, WmiError> {
        one(WMI_AP_PS_PEER_PARAM_CMDID, WMI_TAG_AP_PS_PEER_CMD, |w| {
            w.u32(self.vdev_id);
            w.mac(&self.peer_addr);
            w.u32(self.param);
            w.u32(self.value)
        })
    }
}
u32_command!(StaPowerSaveParameter, WMI_TAG_STA_POWERSAVE_PARAM_CMD,
    WMI_STA_POWERSAVE_PARAM_CMDID, { vdev_id, param, value });
u32_command!(ForceFirmwareHang, WMI_TAG_FORCE_FW_HANG_CMD,
    WMI_FORCE_FW_HANG_CMDID, { hang_type, delay_time_ms });

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StatsRequest {
    pub stats_id: u32,
    pub vdev_id: u32,
    pub pdev_id: u32,
}
impl EncodeCommand for StatsRequest {
    fn encode_command(&self) -> Result<Command, WmiError> {
        one(WMI_REQUEST_STATS_CMDID, WMI_TAG_REQUEST_STATS_CMD, |w| {
            w.u32(self.stats_id);
            w.u32(self.vdev_id);
            w.mac(&[0; 6]);
            w.u32(self.pdev_id)
        })
    }
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PdevTemperatureRequest {
    pub pdev_id: u32,
}
impl EncodeCommand for PdevTemperatureRequest {
    fn encode_command(&self) -> Result<Command, WmiError> {
        one(
            WMI_PDEV_GET_TEMPERATURE_CMDID,
            WMI_TAG_PDEV_GET_TEMPERATURE_CMD,
            |w| {
                w.u32(0);
                w.u32(self.pdev_id)
            },
        )
    }
}
u32_command!(
    DfsPhyerrOffloadEnable,
    WMI_TAG_PDEV_DFS_PHYERR_OFFLOAD_ENABLE_CMD,
    WMI_PDEV_DFS_PHYERR_OFFLOAD_ENABLE_CMDID,
    { pdev_id }
);

const fn firmware_pdev_id(pdev_id: u32) -> u32 {
    if pdev_id == 0 { 0 } else { pdev_id - 1 }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PdevPeerPktlogFilter {
    pub pdev_id: u32,
    pub peer_addr: [u8; 6],
    pub enable: bool,
}
impl EncodeCommand for PdevPeerPktlogFilter {
    fn encode_command(&self) -> Result<Command, WmiError> {
        let mut w = TlvWriter::default();
        w.tlv(WMI_TAG_PDEV_PEER_PKTLOG_FILTER_CMD, |w| {
            w.u32(firmware_pdev_id(self.pdev_id));
            w.u32(u32::from(self.enable));
            w.u32(0);
            w.u32(1);
        })?;
        w.tlv(WMI_TAG_ARRAY_STRUCT, |w| {
            let _ = w.tlv(WMI_TAG_PDEV_PEER_PKTLOG_FILTER_INFO, |w| {
                w.mac(&self.peer_addr)
            });
        })?;
        w.finish(WMI_PDEV_PKTLOG_FILTER_CMDID)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PdevPktlogEnable {
    pub pdev_id: u32,
    pub filter: u32,
}
impl EncodeCommand for PdevPktlogEnable {
    fn encode_command(&self) -> Result<Command, WmiError> {
        one(
            WMI_PDEV_PKTLOG_ENABLE_CMDID,
            WMI_TAG_PDEV_PKTLOG_ENABLE_CMD,
            |w| {
                w.u32(firmware_pdev_id(self.pdev_id));
                w.u32(self.filter);
                w.u32(1)
            },
        )
    }
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PdevPktlogDisable {
    pub pdev_id: u32,
}
impl EncodeCommand for PdevPktlogDisable {
    fn encode_command(&self) -> Result<Command, WmiError> {
        one(
            WMI_PDEV_PKTLOG_DISABLE_CMDID,
            WMI_TAG_PDEV_PKTLOG_DISABLE_CMD,
            |w| w.u32(firmware_pdev_id(self.pdev_id)),
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InitialCountry {
    Alpha([u8; 3]),
    CountryCode(u16),
    Regdomain(u16),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InitCountry {
    pub pdev_id: u32,
    pub country: InitialCountry,
}
impl EncodeCommand for InitCountry {
    fn encode_command(&self) -> Result<Command, WmiError> {
        one(
            WMI_SET_INIT_COUNTRY_CMDID,
            WMI_TAG_SET_INIT_COUNTRY_CMD,
            |w| {
                w.u32(self.pdev_id);
                match self.country {
                    InitialCountry::Alpha(a) => {
                        w.u32(0);
                        w.bytes(&a);
                        w.zeros(1);
                    }
                    InitialCountry::CountryCode(c) => {
                        w.u32(1);
                        w.u32(u32::from(c));
                    }
                    InitialCountry::Regdomain(r) => {
                        w.u32(2);
                        w.u32(u32::from(r));
                    }
                }
            },
        )
    }
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SetCurrentCountry {
    pub pdev_id: u32,
    pub alpha2: [u8; 3],
}
impl EncodeCommand for SetCurrentCountry {
    fn encode_command(&self) -> Result<Command, WmiError> {
        one(
            WMI_SET_CURRENT_COUNTRY_CMDID,
            WMI_TAG_SET_CURRENT_COUNTRY_CMD,
            |w| {
                w.u32(self.pdev_id);
                w.bytes(&self.alpha2);
                w.zeros(1)
            },
        )
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ThermalLevel {
    pub temp_low: u32,
    pub temp_high: u32,
    pub duty_cycle_off_percent: u32,
    pub priority: u32,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ThermalMitigation {
    pub pdev_id: u32,
    pub enable: u32,
    pub duty_cycle: u32,
    pub duty_cycle_per_event: u32,
    pub level: ThermalLevel,
}
impl EncodeCommand for ThermalMitigation {
    fn encode_command(&self) -> Result<Command, WmiError> {
        let mut w = TlvWriter::default();
        w.tlv(WMI_TAG_THERM_THROT_CONFIG_REQUEST, |w| {
            w.u32(self.pdev_id);
            w.u32(self.enable);
            w.u32(self.duty_cycle);
            w.u32(self.duty_cycle_per_event);
            w.u32(1);
        })?;
        w.tlv(WMI_TAG_ARRAY_STRUCT, |w| {
            let _ = w.tlv(WMI_TAG_THERM_THROT_LEVEL_CONFIG_INFO, |w| {
                w.u32(self.level.temp_low);
                w.u32(self.level.temp_high);
                w.u32(self.level.duty_cycle_off_percent);
                w.u32(self.level.priority);
            });
        })?;
        w.finish(WMI_THERM_THROT_SET_CONF_CMDID)
    }
}

u32_command!(Scan11dStart, WMI_TAG_11D_SCAN_START_CMD, WMI_11D_SCAN_START_CMDID,
    { vdev_id, scan_period_ms, start_interval_ms });
u32_command!(
    Scan11dStop,
    WMI_TAG_11D_SCAN_STOP_CMD,
    WMI_11D_SCAN_STOP_CMDID,
    { vdev_id }
);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TwtEnable {
    pub pdev_id: u32,
    pub sta_congestion_timer_ms: u32,
    pub mbss_support: u32,
    pub default_slot_size: u32,
    pub congestion_threshold_setup: u32,
    pub congestion_threshold_teardown: u32,
    pub congestion_threshold_critical: u32,
    pub interference_threshold_teardown: u32,
    pub interference_threshold_setup: u32,
    pub min_sta_setup: u32,
    pub min_sta_teardown: u32,
    pub broadcast_multicast_slots: u32,
    pub min_twt_slots: u32,
    pub max_sta_twt: u32,
    pub mode_check_interval: u32,
    pub add_sta_slot_interval: u32,
    pub remove_sta_slot_interval: u32,
}
impl TwtEnable {
    pub const fn defaults(pdev_id: u32) -> Self {
        Self {
            pdev_id,
            sta_congestion_timer_ms: 5000,
            mbss_support: 0,
            default_slot_size: 10,
            congestion_threshold_setup: 50,
            congestion_threshold_teardown: 20,
            congestion_threshold_critical: 100,
            interference_threshold_teardown: 80,
            interference_threshold_setup: 50,
            min_sta_setup: 10,
            min_sta_teardown: 2,
            broadcast_multicast_slots: 2,
            min_twt_slots: 2,
            max_sta_twt: 500,
            mode_check_interval: 10000,
            add_sta_slot_interval: 1000,
            remove_sta_slot_interval: 5000,
        }
    }
}
impl EncodeCommand for TwtEnable {
    fn encode_command(&self) -> Result<Command, WmiError> {
        one(WMI_TWT_ENABLE_CMDID, WMI_TAG_TWT_ENABLE_CMD, |w| {
            for v in [
                self.pdev_id,
                self.sta_congestion_timer_ms,
                self.mbss_support,
                self.default_slot_size,
                self.congestion_threshold_setup,
                self.congestion_threshold_teardown,
                self.congestion_threshold_critical,
                self.interference_threshold_teardown,
                self.interference_threshold_setup,
                self.min_sta_setup,
                self.min_sta_teardown,
                self.broadcast_multicast_slots,
                self.min_twt_slots,
                self.max_sta_twt,
                self.mode_check_interval,
                self.add_sta_slot_interval,
                self.remove_sta_slot_interval,
            ] {
                w.u32(v)
            }
        })
    }
}
u32_command!(
    TwtDisable,
    WMI_TAG_TWT_DISABLE_CMD,
    WMI_TWT_DISABLE_CMDID,
    { pdev_id }
);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TwtAddDialog {
    pub vdev_id: u32,
    pub peer_addr: [u8; 6],
    pub dialog_id: u32,
    pub wake_interval_us: u32,
    pub wake_interval_mantissa: u32,
    pub wake_duration_us: u32,
    pub service_period_offset_us: u32,
    pub command: u8,
    pub broadcast: bool,
    pub trigger: bool,
    pub flow_type: bool,
    pub protection: bool,
}
impl EncodeCommand for TwtAddDialog {
    fn encode_command(&self) -> Result<Command, WmiError> {
        let mut flags = u32::from(self.command);
        if self.broadcast {
            flags |= 1 << 8
        }
        if self.trigger {
            flags |= 1 << 9
        }
        if self.flow_type {
            flags |= 1 << 10
        }
        if self.protection {
            flags |= 1 << 11
        }
        one(WMI_TWT_ADD_DIALOG_CMDID, WMI_TAG_TWT_ADD_DIALOG_CMD, |w| {
            w.u32(self.vdev_id);
            w.mac(&self.peer_addr);
            w.u32(self.dialog_id);
            w.u32(self.wake_interval_us);
            w.u32(self.wake_interval_mantissa);
            w.u32(self.wake_duration_us);
            w.u32(self.service_period_offset_us);
            w.u32(flags);
        })
    }
}

macro_rules! twt_dialog {
    ($name:ident, $tag:ident, $id:ident) => {
        #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
        pub struct $name {
            pub vdev_id: u32,
            pub peer_addr: [u8; 6],
            pub dialog_id: u32,
        }
        impl EncodeCommand for $name {
            fn encode_command(&self) -> Result<Command, WmiError> {
                one($id, $tag, |w| {
                    w.u32(self.vdev_id);
                    w.mac(&self.peer_addr);
                    w.u32(self.dialog_id)
                })
            }
        }
    };
}
twt_dialog!(
    TwtDeleteDialog,
    WMI_TAG_TWT_DEL_DIALOG_CMD,
    WMI_TWT_DEL_DIALOG_CMDID
);
twt_dialog!(
    TwtPauseDialog,
    WMI_TAG_TWT_PAUSE_DIALOG_CMD,
    WMI_TWT_PAUSE_DIALOG_CMDID
);
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TwtResumeDialog {
    pub vdev_id: u32,
    pub peer_addr: [u8; 6],
    pub dialog_id: u32,
    pub service_period_offset_us: u32,
    pub next_twt_size: u32,
}
impl EncodeCommand for TwtResumeDialog {
    fn encode_command(&self) -> Result<Command, WmiError> {
        one(
            WMI_TWT_RESUME_DIALOG_CMDID,
            WMI_TAG_TWT_RESUME_DIALOG_CMD,
            |w| {
                w.u32(self.vdev_id);
                w.mac(&self.peer_addr);
                w.u32(self.dialog_id);
                w.u32(self.service_period_offset_us);
                w.u32(self.next_twt_size)
            },
        )
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ObssSpatialReuse {
    pub vdev_id: u32,
    pub enable: bool,
    pub min_offset: i32,
    pub max_offset: i32,
}
impl EncodeCommand for ObssSpatialReuse {
    fn encode_command(&self) -> Result<Command, WmiError> {
        one(
            WMI_PDEV_OBSS_PD_SPATIAL_REUSE_CMDID,
            WMI_TAG_OBSS_SPATIAL_REUSE_SET_CMD,
            |w| {
                w.u32(0);
                w.u32(u32::from(self.enable));
                w.u32(self.min_offset as u32);
                w.u32(self.max_offset as u32);
                w.u32(self.vdev_id)
            },
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObssBitmapKind {
    SrgBssColor,
    SrgPartialBssid,
    SrgColorEnable,
    SrgBssidEnable,
    NonSrgColorEnable,
    NonSrgBssidEnable,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObssBitmap {
    pub pdev_id: u32,
    pub bitmap: [u32; 2],
    pub kind: ObssBitmapKind,
}
impl EncodeCommand for ObssBitmap {
    fn encode_command(&self) -> Result<Command, WmiError> {
        let (tag, id) = match self.kind {
            ObssBitmapKind::SrgBssColor => (
                WMI_TAG_PDEV_SRG_BSS_COLOR_BITMAP_CMD,
                WMI_PDEV_SET_SRG_BSS_COLOR_BITMAP_CMDID,
            ),
            ObssBitmapKind::SrgPartialBssid => (
                WMI_TAG_PDEV_SRG_PARTIAL_BSSID_BITMAP_CMD,
                WMI_PDEV_SET_SRG_PARTIAL_BSSID_BITMAP_CMDID,
            ),
            ObssBitmapKind::SrgColorEnable => (
                WMI_TAG_PDEV_SRG_OBSS_COLOR_ENABLE_BITMAP_CMD,
                WMI_PDEV_SET_SRG_OBSS_COLOR_ENABLE_BITMAP_CMDID,
            ),
            ObssBitmapKind::SrgBssidEnable => (
                WMI_TAG_PDEV_SRG_OBSS_BSSID_ENABLE_BITMAP_CMD,
                WMI_PDEV_SET_SRG_OBSS_BSSID_ENABLE_BITMAP_CMDID,
            ),
            ObssBitmapKind::NonSrgColorEnable => (
                WMI_TAG_PDEV_NON_SRG_OBSS_COLOR_ENABLE_BITMAP_CMD,
                WMI_PDEV_SET_NON_SRG_OBSS_COLOR_ENABLE_BITMAP_CMDID,
            ),
            ObssBitmapKind::NonSrgBssidEnable => (
                WMI_TAG_PDEV_NON_SRG_OBSS_BSSID_ENABLE_BITMAP_CMD,
                WMI_PDEV_SET_NON_SRG_OBSS_BSSID_ENABLE_BITMAP_CMDID,
            ),
        };
        one(id, tag, |w| {
            w.u32(self.pdev_id);
            w.u32(self.bitmap[0]);
            w.u32(self.bitmap[1])
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ObssColorCollisionConfig {
    pub vdev_id: u32,
    pub bss_color: u8,
    pub detection_period_ms: u32,
    pub enable: bool,
}
impl EncodeCommand for ObssColorCollisionConfig {
    fn encode_command(&self) -> Result<Command, WmiError> {
        one(
            WMI_OBSS_COLOR_COLLISION_DET_CONFIG_CMDID,
            WMI_TAG_OBSS_COLOR_COLLISION_DET_CONFIG,
            |w| {
                w.u32(self.vdev_id);
                w.u32(0);
                w.u32(u32::from(self.enable));
                w.u32(u32::from(self.bss_color));
                w.u32(self.detection_period_ms);
                w.u32(200);
                w.u32(0)
            },
        )
    }
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BssColorChangeEnable {
    pub vdev_id: u32,
    pub enable: bool,
}
impl EncodeCommand for BssColorChangeEnable {
    fn encode_command(&self) -> Result<Command, WmiError> {
        one(
            WMI_BSS_COLOR_CHANGE_ENABLE_CMDID,
            WMI_TAG_BSS_COLOR_CHANGE_ENABLE,
            |w| {
                w.u32(self.vdev_id);
                w.u32(u32::from(self.enable))
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pktlog_peer_filter_nested_layout() {
        let cmd = PdevPeerPktlogFilter {
            pdev_id: 2,
            peer_addr: [0, 1, 2, 3, 4, 5],
            enable: true,
        }
        .encode_command()
        .unwrap();
        assert_eq!(cmd.id, WMI_PDEV_PKTLOG_FILTER_CMDID);
        assert_eq!(
            cmd.tlvs(),
            &[
                16, 0, 1, 3, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 12, 0, 18, 0, 8, 0, 2,
                3, 0, 1, 2, 3, 4, 5, 0, 0
            ]
        );
    }
    #[test]
    fn thermal_nested_layout() {
        let cmd = ThermalMitigation {
            pdev_id: 1,
            enable: 2,
            duty_cycle: 3,
            duty_cycle_per_event: 4,
            level: ThermalLevel {
                temp_low: 5,
                temp_high: 6,
                duty_cycle_off_percent: 7,
                priority: 8,
            },
        }
        .encode_command()
        .unwrap();
        assert_eq!(cmd.tlvs().len(), 48);
        assert_eq!(&cmd.tlvs()[24..28], &[20, 0, 18, 0]);
    }
    #[test]
    fn representative_fixed_and_default_twt_layouts() {
        let reg = PdevSetRegdomain {
            pdev_id: 1,
            reg_domain: 2,
            reg_domain_2g: 3,
            reg_domain_5g: 4,
            conformance_test_limit_2g: 5,
            conformance_test_limit_5g: 6,
            dfs_domain: 7,
        }
        .encode_command()
        .unwrap();
        assert_eq!(
            reg.tlvs(),
            &[
                28, 0, 81, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 4, 0, 0, 0, 5, 0, 0, 0, 6, 0, 0,
                0, 7, 0, 0, 0
            ]
        );
        let twt = TwtEnable::defaults(9).encode_command().unwrap();
        assert_eq!(twt.tlvs().len(), 72);
        assert_eq!(&twt.tlvs()[4..16], &[9, 0, 0, 0, 136, 19, 0, 0, 0, 0, 0, 0]);
    }
}
