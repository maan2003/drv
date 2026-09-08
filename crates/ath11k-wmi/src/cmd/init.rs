use alloc::vec::Vec;

#[cfg(feature = "proptest")]
use super::CommandStrategy;
use super::{EncodeCommand, TlvWriter, trace_branch, trace_field};
use crate::tags::*;
use crate::trace::TraceSink;
use crate::{Command, WmiError};

#[cfg(feature = "proptest")]
use proptest::prelude::*;

/// Source-shaped `struct wmi_resource_config` payload.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ResourceConfig {
    pub num_vdevs: u32,
    pub num_peers: u32,
    pub num_offload_peers: u32,
    pub num_offload_reorder_buffs: u32,
    pub num_peer_keys: u32,
    pub num_tids: u32,
    pub ast_skid_limit: u32,
    pub tx_chain_mask: u32,
    pub rx_chain_mask: u32,
    pub rx_timeout_pri: [u32; 4],
    pub rx_decap_mode: u32,
    pub scan_max_pending_req: u32,
    pub bmiss_offload_max_vdev: u32,
    pub roam_offload_max_vdev: u32,
    pub roam_offload_max_ap_profiles: u32,
    pub num_mcast_groups: u32,
    pub num_mcast_table_elems: u32,
    pub mcast2ucast_mode: u32,
    pub tx_dbg_log_size: u32,
    pub num_wds_entries: u32,
    pub dma_burst_size: u32,
    pub mac_aggr_delim: u32,
    pub rx_skip_defrag_timeout_dup_detection_check: u32,
    pub vow_config: u32,
    pub gtk_offload_max_vdev: u32,
    pub num_msdu_desc: u32,
    pub max_frag_entries: u32,
    pub num_tdls_vdevs: u32,
    pub num_tdls_conn_table_entries: u32,
    pub beacon_tx_offload_max_vdev: u32,
    pub num_multicast_filter_entries: u32,
    pub num_wow_filters: u32,
    pub num_keep_alive_pattern: u32,
    pub keep_alive_pattern_size: u32,
    pub max_tdls_concurrent_sleep_sta: u32,
    pub max_tdls_concurrent_buffer_sta: u32,
    pub wmi_send_separate: u32,
    pub num_ocb_vdevs: u32,
    pub num_ocb_channels: u32,
    pub num_ocb_schedules: u32,
    pub flag1: u32,
    pub smart_ant_cap: u32,
    pub bk_minfree: u32,
    pub be_minfree: u32,
    pub vi_minfree: u32,
    pub vo_minfree: u32,
    pub alloc_frag_desc_for_data_pkt: u32,
    pub num_ns_ext_tuples_cfg: u32,
    pub bpf_instruction_size: u32,
    pub max_bssid_rx_filters: u32,
    pub use_pdev_id: u32,
    pub max_num_dbs_scan_duty_cycle: u32,
    pub max_num_group_keys: u32,
    pub peer_map_unmap_v2_support: u32,
    pub sched_params: u32,
    pub twt_ap_pdev_count: u32,
    pub twt_ap_sta_count: u32,
    pub max_nlo_ssids: u32,
    pub num_pkt_filters: u32,
    pub num_max_sta_vdevs: u32,
    pub max_bssid_indicator: u32,
    pub ul_resp_config: u32,
    pub msdu_flow_override_config0: u32,
    pub msdu_flow_override_config1: u32,
    pub flags2: u32,
    pub host_service_flags: u32,
    pub max_rnr_neighbours: u32,
    pub ema_max_vap_cnt: u32,
    pub ema_max_profile_period: u32,
}

impl ResourceConfig {
    pub const fn from_words(words: [u32; 72]) -> Self {
        Self {
            num_vdevs: words[0],
            num_peers: words[1],
            num_offload_peers: words[2],
            num_offload_reorder_buffs: words[3],
            num_peer_keys: words[4],
            num_tids: words[5],
            ast_skid_limit: words[6],
            tx_chain_mask: words[7],
            rx_chain_mask: words[8],
            rx_timeout_pri: [words[9], words[10], words[11], words[12]],
            rx_decap_mode: words[13],
            scan_max_pending_req: words[14],
            bmiss_offload_max_vdev: words[15],
            roam_offload_max_vdev: words[16],
            roam_offload_max_ap_profiles: words[17],
            num_mcast_groups: words[18],
            num_mcast_table_elems: words[19],
            mcast2ucast_mode: words[20],
            tx_dbg_log_size: words[21],
            num_wds_entries: words[22],
            dma_burst_size: words[23],
            mac_aggr_delim: words[24],
            rx_skip_defrag_timeout_dup_detection_check: words[25],
            vow_config: words[26],
            gtk_offload_max_vdev: words[27],
            num_msdu_desc: words[28],
            max_frag_entries: words[29],
            num_tdls_vdevs: words[30],
            num_tdls_conn_table_entries: words[31],
            beacon_tx_offload_max_vdev: words[32],
            num_multicast_filter_entries: words[33],
            num_wow_filters: words[34],
            num_keep_alive_pattern: words[35],
            keep_alive_pattern_size: words[36],
            max_tdls_concurrent_sleep_sta: words[37],
            max_tdls_concurrent_buffer_sta: words[38],
            wmi_send_separate: words[39],
            num_ocb_vdevs: words[40],
            num_ocb_channels: words[41],
            num_ocb_schedules: words[42],
            flag1: words[43],
            smart_ant_cap: words[44],
            bk_minfree: words[45],
            be_minfree: words[46],
            vi_minfree: words[47],
            vo_minfree: words[48],
            alloc_frag_desc_for_data_pkt: words[49],
            num_ns_ext_tuples_cfg: words[50],
            bpf_instruction_size: words[51],
            max_bssid_rx_filters: words[52],
            use_pdev_id: words[53],
            max_num_dbs_scan_duty_cycle: words[54],
            max_num_group_keys: words[55],
            peer_map_unmap_v2_support: words[56],
            sched_params: words[57],
            twt_ap_pdev_count: words[58],
            twt_ap_sta_count: words[59],
            max_nlo_ssids: words[60],
            num_pkt_filters: words[61],
            num_max_sta_vdevs: words[62],
            max_bssid_indicator: words[63],
            ul_resp_config: words[64],
            msdu_flow_override_config0: words[65],
            msdu_flow_override_config1: words[66],
            flags2: words[67],
            host_service_flags: words[68],
            max_rnr_neighbours: words[69],
            ema_max_vap_cnt: words[70],
            ema_max_profile_period: words[71],
        }
    }

    pub const fn words(&self) -> [u32; 72] {
        [
            self.num_vdevs,
            self.num_peers,
            self.num_offload_peers,
            self.num_offload_reorder_buffs,
            self.num_peer_keys,
            self.num_tids,
            self.ast_skid_limit,
            self.tx_chain_mask,
            self.rx_chain_mask,
            self.rx_timeout_pri[0],
            self.rx_timeout_pri[1],
            self.rx_timeout_pri[2],
            self.rx_timeout_pri[3],
            self.rx_decap_mode,
            self.scan_max_pending_req,
            self.bmiss_offload_max_vdev,
            self.roam_offload_max_vdev,
            self.roam_offload_max_ap_profiles,
            self.num_mcast_groups,
            self.num_mcast_table_elems,
            self.mcast2ucast_mode,
            self.tx_dbg_log_size,
            self.num_wds_entries,
            self.dma_burst_size,
            self.mac_aggr_delim,
            self.rx_skip_defrag_timeout_dup_detection_check,
            self.vow_config,
            self.gtk_offload_max_vdev,
            self.num_msdu_desc,
            self.max_frag_entries,
            self.num_tdls_vdevs,
            self.num_tdls_conn_table_entries,
            self.beacon_tx_offload_max_vdev,
            self.num_multicast_filter_entries,
            self.num_wow_filters,
            self.num_keep_alive_pattern,
            self.keep_alive_pattern_size,
            self.max_tdls_concurrent_sleep_sta,
            self.max_tdls_concurrent_buffer_sta,
            self.wmi_send_separate,
            self.num_ocb_vdevs,
            self.num_ocb_channels,
            self.num_ocb_schedules,
            self.flag1,
            self.smart_ant_cap,
            self.bk_minfree,
            self.be_minfree,
            self.vi_minfree,
            self.vo_minfree,
            self.alloc_frag_desc_for_data_pkt,
            self.num_ns_ext_tuples_cfg,
            self.bpf_instruction_size,
            self.max_bssid_rx_filters,
            self.use_pdev_id,
            self.max_num_dbs_scan_duty_cycle,
            self.max_num_group_keys,
            self.peer_map_unmap_v2_support,
            self.sched_params,
            self.twt_ap_pdev_count,
            self.twt_ap_sta_count,
            self.max_nlo_ssids,
            self.num_pkt_filters,
            self.num_max_sta_vdevs,
            self.max_bssid_indicator,
            self.ul_resp_config,
            self.msdu_flow_override_config0,
            self.msdu_flow_override_config1,
            self.flags2,
            self.host_service_flags,
            self.max_rnr_neighbours,
            self.ema_max_vap_cnt,
            self.ema_max_profile_period,
        ]
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
    fn trace_fields(&self, sink: &mut dyn TraceSink) {
        macro_rules! resource_fields {
            ($($field:ident),+ $(,)?) => {
                $(trace_field(
                    sink,
                    concat!("Init.resource_config.", stringify!($field)),
                    self.resource_config.$field,
                );)+
            };
        }
        resource_fields!(
            num_vdevs,
            num_peers,
            num_offload_peers,
            num_offload_reorder_buffs,
            num_peer_keys,
            num_tids,
            ast_skid_limit,
            tx_chain_mask,
            rx_chain_mask,
        );
        for (index, value) in self.resource_config.rx_timeout_pri.iter().enumerate() {
            trace_field(
                sink,
                [
                    "Init.resource_config.rx_timeout_pri[0]",
                    "Init.resource_config.rx_timeout_pri[1]",
                    "Init.resource_config.rx_timeout_pri[2]",
                    "Init.resource_config.rx_timeout_pri[3]",
                ][index],
                *value,
            );
        }
        resource_fields!(
            rx_decap_mode,
            scan_max_pending_req,
            bmiss_offload_max_vdev,
            roam_offload_max_vdev,
            roam_offload_max_ap_profiles,
            num_mcast_groups,
            num_mcast_table_elems,
            mcast2ucast_mode,
            tx_dbg_log_size,
            num_wds_entries,
            dma_burst_size,
            mac_aggr_delim,
            rx_skip_defrag_timeout_dup_detection_check,
            vow_config,
            gtk_offload_max_vdev,
            num_msdu_desc,
            max_frag_entries,
            num_tdls_vdevs,
            num_tdls_conn_table_entries,
            beacon_tx_offload_max_vdev,
            num_multicast_filter_entries,
            num_wow_filters,
            num_keep_alive_pattern,
            keep_alive_pattern_size,
            max_tdls_concurrent_sleep_sta,
            max_tdls_concurrent_buffer_sta,
            wmi_send_separate,
            num_ocb_vdevs,
            num_ocb_channels,
            num_ocb_schedules,
            flag1,
            smart_ant_cap,
            bk_minfree,
            be_minfree,
            vi_minfree,
            vo_minfree,
            alloc_frag_desc_for_data_pkt,
            num_ns_ext_tuples_cfg,
            bpf_instruction_size,
            max_bssid_rx_filters,
            use_pdev_id,
            max_num_dbs_scan_duty_cycle,
            max_num_group_keys,
            peer_map_unmap_v2_support,
            sched_params,
            twt_ap_pdev_count,
            twt_ap_sta_count,
            max_nlo_ssids,
            num_pkt_filters,
            num_max_sta_vdevs,
            max_bssid_indicator,
            ul_resp_config,
            msdu_flow_override_config0,
            msdu_flow_override_config1,
            flags2,
            host_service_flags,
            max_rnr_neighbours,
            ema_max_vap_cnt,
            ema_max_profile_period,
        );

        trace_field(
            sink,
            "Init.memory_chunks.len",
            self.memory_chunks.len() as u64,
        );
        for chunk in &self.memory_chunks {
            trace_field(sink, "Init.memory_chunks[].request_id", chunk.request_id);
            trace_field(
                sink,
                "Init.memory_chunks[].physical_address",
                chunk.physical_address,
            );
            trace_field(sink, "Init.memory_chunks[].size", chunk.size);
        }

        trace_branch(
            sink,
            "Init.hardware_mode.is_some",
            self.hardware_mode.is_some(),
        );
        if let Some(hardware_mode) = self.hardware_mode {
            trace_field(sink, "Init.hardware_mode", hardware_mode);
        }
        trace_field(sink, "Init.bands.len", self.bands.len() as u64);
        for band in &self.bands {
            trace_field(sink, "Init.bands[].pdev_id", band.pdev_id);
            trace_field(sink, "Init.bands[].start_freq", band.start_freq);
            trace_field(sink, "Init.bands[].end_freq", band.end_freq);
        }
    }

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
            let mut words = self.resource_config.words();
            // ath11k_wmi_copy_resource_config copies only this pinned subset
            // into a zeroed wire struct.
            for index in [
                44usize, 45, 46, 47, 48, 49, 50, 54, 55, 60, 61, 62, 63, 64, 65, 66, 69,
            ] {
                words[index] = 0;
            }
            words[67] = 1 << 9; // WMI_RSRC_CFG_FLAG2_CALC_NEXT_DTIM_COUNT_SET
            words[68] &= 1 << 4; // only REG_CC_EXT is copied by the C builder
            for value in words {
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
        let reserved_chunk_tail = if self.memory_chunks.is_empty() {
            0
        } else {
            (32 - self.memory_chunks.len()) * 16
        };

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
        // The C allocation reserves 32 chunk entries but advances past only
        // live chunks before writing optional hw-mode TLVs, leaving this tail
        // at the very end of the skb.
        w.zeros(reserved_chunk_tail);
        w.finish(WMI_INIT_CMDID)
    }
}

#[cfg(feature = "proptest")]
impl CommandStrategy for Init {
    fn strategy() -> BoxedStrategy<Self> {
        let resource_config = any::<[u32; 72]>().prop_map(ResourceConfig::from_words);
        let memory_chunk = (any::<u32>(), any::<u32>(), any::<u32>()).prop_map(
            |(request_id, physical_address, size)| HostMemoryChunk {
                request_id,
                physical_address,
                size,
            },
        );
        let band = (any::<u32>(), any::<u32>(), any::<u32>()).prop_map(
            |(pdev_id, start_freq, end_freq)| BandToMac {
                pdev_id,
                start_freq,
                end_freq,
            },
        );
        let hardware = prop_oneof![
            Just((None, Vec::new())),
            (any::<u32>(), prop::collection::vec(band, 0..=3))
                .prop_map(|(mode, bands)| (Some(mode), bands)),
        ];

        (
            resource_config,
            prop::collection::vec(memory_chunk, 0..=32),
            hardware,
        )
            .prop_map(
                |(resource_config, memory_chunks, (hardware_mode, bands))| Self {
                    resource_config,
                    memory_chunks,
                    hardware_mode,
                    bands,
                },
            )
            .boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::TraceEvent;
    use alloc::vec;

    #[derive(Default)]
    struct Trace(Vec<TraceEvent>);

    impl TraceSink for Trace {
        fn record(&mut self, event: TraceEvent) {
            self.0.push(event);
        }
    }

    #[test]
    fn resource_copy_zeroes_omitted_fields_and_forces_flags() {
        let command = Init {
            resource_config: ResourceConfig::from_words([u32::MAX; 72]),
            memory_chunks: Vec::new(),
            hardware_mode: None,
            bands: Vec::new(),
        }
        .encode_command()
        .unwrap();
        let payload = &command.tlvs()[36..324];
        let words: Vec<u32> = payload
            .chunks_exact(4)
            .map(|x| u32::from_le_bytes(x.try_into().unwrap()))
            .collect();
        for i in [
            44usize, 45, 46, 47, 48, 49, 50, 54, 55, 60, 61, 62, 63, 64, 65, 66, 69,
        ] {
            assert_eq!(words[i], 0);
        }
        assert_eq!(words[67], 1 << 9);
        assert_eq!(words[68], 1 << 4);
    }

    #[test]
    fn hardware_mode_precedes_reserved_chunk_tail() {
        let command = Init {
            resource_config: ResourceConfig::default(),
            memory_chunks: vec![HostMemoryChunk {
                request_id: 1,
                physical_address: 2,
                size: 3,
            }],
            hardware_mode: Some(4),
            bands: vec![BandToMac {
                pdev_id: 0,
                start_freq: 2400,
                end_freq: 2500,
            }],
        }
        .encode_command()
        .unwrap();
        // Fixed init + resource + chunk-array header + one chunk = 344.
        assert_eq!(
            u32::from_le_bytes(command.tlvs()[344..348].try_into().unwrap()),
            (u32::from(WMI_TAG_PDEV_SET_HW_MODE_CMD.0) << 16) | 12
        );
        assert!(command.tlvs()[384..].iter().all(|byte| *byte == 0));
    }

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
        assert_eq!(
            u32::from_le_bytes(command.tlvs()[304..308].try_into().unwrap()),
            1 << 9
        );
    }

    #[test]
    fn trace_uses_stable_full_paths_for_nested_init_data() {
        let init = Init {
            resource_config: ResourceConfig {
                num_vdevs: 7,
                rx_timeout_pri: [11, 12, 13, 14],
                ..ResourceConfig::default()
            },
            memory_chunks: vec![HostMemoryChunk {
                request_id: 21,
                physical_address: 22,
                size: 23,
            }],
            hardware_mode: Some(31),
            bands: vec![BandToMac {
                pdev_id: 41,
                start_freq: 42,
                end_freq: 43,
            }],
        };
        let mut trace = Trace::default();
        init.encode_command_with_trace(&mut trace).unwrap();

        for expected in [
            TraceEvent::Field {
                name: "Init.resource_config.num_vdevs",
                value: 7,
            },
            TraceEvent::Field {
                name: "Init.resource_config.rx_timeout_pri[2]",
                value: 13,
            },
            TraceEvent::Field {
                name: "Init.memory_chunks[].physical_address",
                value: 22,
            },
            TraceEvent::Branch {
                name: "Init.hardware_mode.is_some",
                taken: true,
            },
            TraceEvent::Field {
                name: "Init.hardware_mode",
                value: 31,
            },
            TraceEvent::Field {
                name: "Init.bands[].end_freq",
                value: 43,
            },
        ] {
            assert!(trace.0.contains(&expected), "missing {expected:?}");
        }
    }

    #[cfg(feature = "proptest")]
    proptest! {
        #[test]
        fn init_strategy_only_generates_encodable_shapes(init in Init::strategy()) {
            prop_assert!(init.memory_chunks.len() <= 32);
            prop_assert!(init.bands.len() <= 3);
            prop_assert!(init.hardware_mode.is_some() || init.bands.is_empty());
            prop_assert!(init.encode_command().is_ok());
        }
    }
}
