# ath11k-wmi command port map

Status is `typed`, `encoded`, `oracle-checked`, or `hardware-checked`.
The native golden transcript is pending a tracing-enabled kexec kernel; current
fixtures are source-derived from pinned C packed layouts.

| C file:symbol | Rust item | status | oracle artifact |
|---|---|---|---|
| wmi.h:enum wmi_cmd_group | tags::WMI_GRP_* | encoded | tags unit tests |
| wmi.h:enum wmi_tlv_cmd_id | tags::WMI_*_CMDID | encoded | tags unit tests |
| wmi.h:enum wmi_tlv_event_id | tags::WMI_*_EVENTID | encoded | tags unit tests |
| wmi.h:enum wmi_tlv_tag | tags::WMI_TAG_* | encoded | tags unit tests |
| wmi.h:enum wmi_tlv_service | tags::WMI_TLV_SERVICE_* | encoded | tags unit tests |
| ath11k_wmi_vdev_create | cmd::VdevCreate | encoded | cmd::tests::vdev_create_layout |
| ath11k_wmi_vdev_delete | cmd::VdevDelete | encoded | source-derived fixed TLV |
| ath11k_wmi_vdev_stop | cmd::VdevStop | encoded | source-derived fixed TLV |
| ath11k_wmi_vdev_down | cmd::VdevDown | encoded | source-derived fixed TLV |
| ath11k_wmi_put_wmi_channel | cmd::Channel | encoded | VdevStart fixture |
| ath11k_wmi_vdev_start | cmd::VdevStart | encoded | cmd::tests::ssid_limit_matches_c |
| ath11k_wmi_vdev_up | cmd::VdevUp | encoded | source-derived fixed TLV |
| ath11k_wmi_send_peer_create_cmd | cmd::PeerCreate | encoded | source-derived fixed TLV |
| ath11k_wmi_send_peer_delete_cmd | cmd::PeerDelete | encoded | source-derived fixed TLV |
| ath11k_wmi_pdev_set_param | cmd::PdevSetParam | encoded | source-derived fixed TLV |
| ath11k_wmi_vdev_set_param_cmd | cmd::VdevSetParam | encoded | source-derived fixed TLV |
| ath11k_wmi_vdev_install_key | cmd::VdevInstallKey | encoded | cmd::tests::key_padding |
| ath11k_wmi_mgmt_send | cmd::MgmtSend | encoded | source-derived nested TLV |
| ath11k_wmi_send_scan_stop_cmd | cmd::ScanStop | encoded | source-derived fixed TLV |
| ath11k_init_cmd_send / ath11k_wmi_cmd_init | cmd::Init | encoded | cmd::init::tests::single_pdev_init_matches_fixed_c_layout |
| ath11k_wmi_send_peer_assoc_cmd | cmd::PeerAssoc | encoded | cmd::peer_assoc source-derived layout |
| ath11k_wmi_send_scan_start_cmd | cmd::ScanStart | encoded | cmd::scan source-derived layout |
| ath11k_wmi_send_scan_chan_list_cmd | cmd::ScanChannelList | encoded | cmd::scan source-derived layout |
| ath11k_wmi_set_peer_param | cmd::PeerSetParam | encoded | source-derived fixed TLV |
| ath11k_wmi_pdev_suspend | cmd::PdevSuspend | encoded | source-derived fixed TLV |
| ath11k_wmi_pdev_resume | cmd::PdevResume | encoded | source-derived fixed TLV |
| ath11k_wmi_attach | cmd::Wmi::attach | encoded | lifecycle state test pending |
| ath11k_wmi_pdev_attach | cmd::Wmi::pdev_attach | encoded | lifecycle state test pending |
| ath11k_wmi_connect | cmd::Wmi::connect | encoded | lifecycle state test pending |
| ath11k_wmi_wait_for_service_ready | cmd::Wmi::wait_for_service_ready | encoded | event lifecycle fixtures |
| ath11k_wmi_wait_for_unified_ready | cmd::Wmi::wait_for_unified_ready | encoded | event lifecycle fixtures |
| ath11k_wmi_detach | cmd::Wmi::detach | encoded | lifecycle state test pending |
| ath11k_wmi_send_pdev_set_regdomain | cmd::PdevSetRegdomain | encoded | control fixtures |
| ath11k_wmi_send_peer_flush_tids_cmd | cmd::PeerFlushTids | encoded | control fixtures |
| ath11k_wmi_peer_rx_reorder_queue_setup | cmd::PeerReorderQueueSetup | encoded | source-derived layout |
| ath11k_wmi_rx_reord_queue_remove | cmd::PeerReorderQueueRemove | encoded | source-derived layout |
| ath11k_wmi_pdev_set_ps_mode | cmd::StaPowerSaveMode | encoded | source-derived layout |
| ath11k_wmi_pdev_bss_chan_info_request | cmd::PdevBssChannelInfoRequest | encoded | source-derived layout |
| ath11k_wmi_send_set_ap_ps_param_cmd | cmd::ApPowerSavePeer | encoded | source-derived layout |
| ath11k_wmi_set_sta_ps_param | cmd::StaPowerSaveParameter | encoded | source-derived layout |
| ath11k_wmi_force_fw_hang_cmd | cmd::ForceFirmwareHang | encoded | source-derived layout |
| ath11k_wmi_send_stats_request_cmd | cmd::StatsRequest | encoded | control fixtures |
| ath11k_wmi_send_pdev_temperature_cmd | cmd::PdevTemperatureRequest | encoded | source-derived layout |
| ath11k_wmi_send_dfs_phyerr_offload_enable_cmd | cmd::DfsPhyerrOffloadEnable | encoded | source-derived layout |
| ath11k_wmi_pdev_peer_pktlog_filter | cmd::PdevPeerPktlogFilter | encoded | control fixtures |
| ath11k_wmi_pdev_pktlog_enable | cmd::PdevPktlogEnable | encoded | source-derived layout |
| ath11k_wmi_pdev_pktlog_disable | cmd::PdevPktlogDisable | encoded | source-derived layout |
| ath11k_wmi_send_init_country_cmd | cmd::InitCountry | encoded | control fixtures |
| ath11k_wmi_send_set_current_country_cmd | cmd::SetCurrentCountry | encoded | source-derived layout |
| ath11k_wmi_send_thermal_mitigation_param_cmd | cmd::ThermalMitigation | encoded | control fixtures |
| ath11k_wmi_send_11d_scan_start_cmd | cmd::Scan11dStart | encoded | source-derived layout |
| ath11k_wmi_send_11d_scan_stop_cmd | cmd::Scan11dStop | encoded | source-derived layout |
| ath11k_wmi_fill_default_twt_params | cmd::TwtEnable::defaults | encoded | control fixtures |
| ath11k_wmi_send_twt_enable_cmd | cmd::TwtEnable | encoded | control fixtures |
| ath11k_wmi_send_twt_disable_cmd | cmd::TwtDisable | encoded | source-derived layout |
| ath11k_wmi_send_twt_add_dialog_cmd | cmd::TwtAddDialog | encoded | source-derived layout |
| ath11k_wmi_send_twt_del_dialog_cmd | cmd::TwtDeleteDialog | encoded | source-derived layout |
| ath11k_wmi_send_twt_pause_dialog_cmd | cmd::TwtPauseDialog | encoded | source-derived layout |
| ath11k_wmi_send_twt_resume_dialog_cmd | cmd::TwtResumeDialog | encoded | source-derived layout |
| ath11k_wmi_send_obss_spr_cmd | cmd::ObssSpatialReuse | encoded | source-derived layout |
| ath11k_wmi_pdev_set_srg_bss_color_bitmap | cmd::ObssBitmap | encoded | source-derived layout |
| ath11k_wmi_pdev_set_srg_patial_bssid_bitmap | cmd::ObssBitmap | encoded | source-derived layout |
| ath11k_wmi_pdev_srg_obss_color_enable_bitmap | cmd::ObssBitmap | encoded | source-derived layout |
| ath11k_wmi_pdev_srg_obss_bssid_enable_bitmap | cmd::ObssBitmap | encoded | source-derived layout |
| ath11k_wmi_pdev_non_srg_obss_color_enable_bitmap | cmd::ObssBitmap | encoded | source-derived layout |
| ath11k_wmi_pdev_non_srg_obss_bssid_enable_bitmap | cmd::ObssBitmap | encoded | source-derived layout |
| ath11k_wmi_send_obss_color_collision_cfg_cmd | cmd::ObssColorCollisionConfig | encoded | source-derived layout |
| ath11k_wmi_send_bss_color_change_enable_cmd | cmd::BssColorChangeEnable | encoded | source-derived layout |
| ath11k_wmi_pdev_lro_cfg | cmd::PdevLroConfig | encoded | device fixtures |
| ath11k_wmi_set_hw_mode | cmd::PdevSetHardwareMode | encoded | device fixtures |
| ath11k_wmi_vdev_spectral_conf | cmd::VdevSpectralConfig | encoded | device fixtures |
| ath11k_wmi_vdev_spectral_enable | cmd::VdevSpectralEnable | encoded | device fixtures |
| ath11k_wmi_pdev_dma_ring_cfg | cmd::PdevDmaRingConfig | encoded | device fixtures |
| ath11k_wmi_send_unit_test_cmd | cmd::UnitTest | encoded | wow fixtures |
| ath11k_wmi_simulate_radar | cmd::SimulateRadar | encoded | wow fixtures |
| ath11k_wmi_fw_dbglog_cfg | cmd::DebugLogConfig | encoded | wow fixtures |
| ath11k_wmi_hw_data_filter_cmd | cmd::HwDataFilter | encoded | wow fixtures |
| ath11k_wmi_wow_host_wakeup_ind | cmd::WowHostWakeup | encoded | wow fixtures |
| ath11k_wmi_wow_enable | cmd::WowEnable | encoded | wow fixtures |
| ath11k_wmi_scan_prob_req_oui | cmd::ScanProbeRequestOui | encoded | source-derived layout |
| ath11k_wmi_wow_add_wakeup_event | cmd::WowWakeEventConfig | encoded | source-derived layout |
| ath11k_wmi_wow_add_pattern | cmd::WowAddPattern | encoded | wow fixtures |
| ath11k_wmi_wow_del_pattern | cmd::WowDeletePattern | encoded | source-derived layout |
| ath11k_wmi_op_gen_config_pno_start | cmd::PnoStart | encoded | wow fixtures |
| ath11k_wmi_wow_config_pno | cmd::PnoStart / cmd::PnoStop | encoded | wow fixtures |
| ath11k_wmi_arp_ns_offload | cmd::ArpNsOffload | encoded | wow fixtures |
| ath11k_wmi_gtk_rekey_offload | cmd::GtkRekey | encoded | wow fixtures |
| ath11k_wmi_gtk_rekey_getinfo | cmd::GtkRekey | encoded | wow fixtures |
| ath11k_wmi_pdev_set_bios_sar_table_param | cmd::BiosSarTable | encoded | wow fixtures |
| ath11k_wmi_pdev_set_bios_geo_table_param | cmd::BiosGeoTable | encoded | wow fixtures |
| ath11k_wmi_sta_keepalive | cmd::StaKeepalive | encoded | wow fixtures |
| AP/deferred builders | cmd::ap::* | encoded | cmd/AP-PORT-MAP.md |
