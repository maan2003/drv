# ath11k-wmi port map

<!-- PORT-MAP-SCHEMA: C symbol | C file:lines | Rust item | status ∈ {ported, wcn6750-specific, local-seam, replaced-by-fuchsia-mlme, kernel-substrate, deferred, blocked} | note -->

Pinned source: Linux `509ce3d952d550f93b544c8d94c99e798f09a9b4`.
Native golden verification is blocked pending a tracing-enabled kexec kernel;
notes identify the checked-in source-derived evidence used meanwhile.

| C symbol | C file:lines | Rust item | status | note |
|---|---|---|---|---|
| enum wmi_cmd_group | wmi.h:150-215 | tags::WMI_GRP_* | ported | tags unit tests |
| enum wmi_tlv_cmd_id | wmi.h:222-628 | tags::WMI_*_CMDID | ported | tags unit tests |
| enum wmi_tlv_event_id | wmi.h:630-842 | tags::WMI_*_EVENTID | ported | tags unit tests |
| enum wmi_tlv_tag | wmi.h:1141-1908 | tags::WMI_TAG_* | ported | tags unit tests |
| enum wmi_tlv_service | wmi.h:1910-2146 | tags::WMI_TLV_SERVICE_* | ported | tags unit tests |
| ath11k_wmi_mgmt_send | wmi.c:653-715 | cmd::MgmtSend | ported | source-derived nested TLV |
| ath11k_wmi_vdev_create | wmi.c:717-795 | cmd::VdevCreate | ported | cmd::tests::vdev_create_layout |
| ath11k_wmi_vdev_delete | wmi.c:797-822 | cmd::VdevDelete | ported | source-derived fixed TLV |
| ath11k_wmi_vdev_stop | wmi.c:824-850 | cmd::VdevStop | ported | source-derived fixed TLV |
| ath11k_wmi_vdev_down | wmi.c:852-878 | cmd::VdevDown | ported | source-derived fixed TLV |
| ath11k_wmi_put_wmi_channel | wmi.c:880-932 | cmd::Channel | ported | VdevStart fixture |
| ath11k_wmi_vdev_start | wmi.c:934-1023 | cmd::VdevStart | ported | cmd::tests::ssid_limit_matches_c |
| ath11k_wmi_vdev_up | wmi.c:1025-1077 | cmd::VdevUp | ported | source-derived fixed TLV |
| ath11k_wmi_send_peer_create_cmd | wmi.c:1079-1110 | cmd::PeerCreate | ported | source-derived fixed TLV |
| ath11k_wmi_send_peer_delete_cmd | wmi.c:1112-1142 | cmd::PeerDelete | ported | source-derived fixed TLV |
| ath11k_wmi_send_pdev_set_regdomain | wmi.c:1144-1182 | cmd::PdevSetRegdomain | ported | control fixtures |
| ath11k_wmi_set_peer_param | wmi.c:1184-1215 | cmd::PeerSetParam | ported | source-derived fixed TLV |
| ath11k_wmi_send_peer_flush_tids_cmd | wmi.c:1217-1250 | cmd::PeerFlushTids | ported | control fixtures |
| ath11k_wmi_peer_rx_reorder_queue_setup | wmi.c:1252-1293 | cmd::PeerReorderQueueSetup | ported | source-derived layout |
| ath11k_wmi_rx_reord_queue_remove | wmi.c:1296-1330 | cmd::PeerReorderQueueRemove | ported | source-derived layout |
| ath11k_wmi_pdev_set_param | wmi.c:1332-1362 | cmd::PdevSetParam | ported | source-derived fixed TLV |
| ath11k_wmi_pdev_set_ps_mode | wmi.c:1364-1393 | cmd::StaPowerSaveMode | ported | source-derived layout |
| ath11k_wmi_pdev_suspend | wmi.c:1395-1425 | cmd::PdevSuspend | ported | source-derived fixed TLV |
| ath11k_wmi_pdev_resume | wmi.c:1427-1454 | cmd::PdevResume | ported | source-derived fixed TLV |
| ath11k_wmi_pdev_bss_chan_info_request | wmi.c:1460-1492 | cmd::PdevBssChannelInfoRequest | ported | source-derived layout |
| ath11k_wmi_send_set_ap_ps_param_cmd | wmi.c:1494-1527 | cmd::ApPowerSavePeer | ported | source-derived layout |
| ath11k_wmi_set_sta_ps_param | wmi.c:1529-1561 | cmd::StaPowerSaveParameter | ported | source-derived layout |
| ath11k_wmi_force_fw_hang_cmd | wmi.c:1563-1593 | cmd::ForceFirmwareHang | ported | source-derived layout |
| ath11k_wmi_vdev_set_param_cmd | wmi.c:1595-1627 | cmd::VdevSetParam | ported | source-derived fixed TLV |
| ath11k_wmi_send_stats_request_cmd | wmi.c:1629-1660 | cmd::StatsRequest | ported | control fixtures |
| ath11k_wmi_send_pdev_temperature_cmd | wmi.c:1662-1688 | cmd::PdevTemperatureRequest | ported | source-derived layout |
| ath11k_wmi_send_bcn_offload_control_cmd | wmi.c:1690-1722 | cmd::BeaconOffloadControl | ported | source-derived AP fixture |
| ath11k_wmi_p2p_go_bcn_ie | wmi.c:1724-1761 | cmd::P2pGoBeaconIe | ported | source-derived AP fixture |
| ath11k_wmi_bcn_tmpl | wmi.c:1763-1832 | cmd::BeaconTemplate | ported | source-derived AP fixture |
| ath11k_wmi_vdev_install_key | wmi.c:1834-1884 | cmd::VdevInstallKey | ported | cmd::tests::key_padding |
| ath11k_wmi_send_peer_assoc_cmd | wmi.c:1970-2135 | cmd::PeerAssoc | ported | cmd::peer_assoc source-derived layout |
| ath11k_wmi_send_scan_start_cmd | wmi.c:2255-2442 | cmd::ScanStart | ported | cmd::scan source-derived layout |
| ath11k_wmi_send_vdev_set_tpc_power | wmi.c:2444-2506 | cmd::VdevSetTpcPower | ported | source-derived nested TLV |
| ath11k_wmi_send_scan_stop_cmd | wmi.c:2508-2556 | cmd::ScanStop | ported | source-derived fixed TLV |
| ath11k_wmi_send_scan_chan_list_cmd | wmi.c:2558-2680 | cmd::ScanChannelList | ported | cmd::scan source-derived layout |
| ath11k_wmi_send_wmm_update_cmd_tlv | wmi.c:2682-2752 | cmd::WmmUpdate | ported | source-derived AP fixture |
| ath11k_wmi_send_dfs_phyerr_offload_enable_cmd | wmi.c:2754-2786 | cmd::DfsPhyerrOffloadEnable | ported | source-derived layout |
| ath11k_wmi_delba_send | wmi.c:2788-2822 | cmd::DelbaSend | ported | source-derived AP fixture |
| ath11k_wmi_addba_set_resp | wmi.c:2824-2858 | cmd::AddbaSetResponse | ported | source-derived AP fixture |
| ath11k_wmi_addba_send | wmi.c:2860-2893 | cmd::AddbaSend | ported | source-derived AP fixture |
| ath11k_wmi_addba_clear_resp | wmi.c:2895-2926 | cmd::AddbaClearResponse | ported | source-derived AP fixture |
| ath11k_wmi_pdev_peer_pktlog_filter | wmi.c:2928-2976 | cmd::PdevPeerPktlogFilter | ported | control fixtures |
| ath11k_wmi_send_init_country_cmd | wmi.c:2979-3036 | cmd::InitCountry | ported | control fixtures |
| ath11k_wmi_send_set_current_country_cmd | wmi.c:3038-3072 | cmd::SetCurrentCountry | ported | source-derived layout |
| ath11k_wmi_send_thermal_mitigation_param_cmd | wmi.c:3075-3136 | cmd::ThermalMitigation | ported | control fixtures |
| ath11k_wmi_send_11d_scan_start_cmd | wmi.c:3138-3173 | cmd::Scan11dStart | ported | source-derived layout |
| ath11k_wmi_send_11d_scan_stop_cmd | wmi.c:3175-3205 | cmd::Scan11dStop | ported | source-derived layout |
| ath11k_wmi_pdev_pktlog_enable | wmi.c:3207-3237 | cmd::PdevPktlogEnable | ported | source-derived layout |
| ath11k_wmi_pdev_pktlog_disable | wmi.c:3239-3267 | cmd::PdevPktlogDisable | ported | source-derived layout |
| ath11k_wmi_fill_default_twt_params | wmi.c:3269-3293 | cmd::TwtEnable::defaults | ported | control fixtures |
| ath11k_wmi_send_twt_enable_cmd | wmi.c:3295-3343 | cmd::TwtEnable | ported | control fixtures |
| ath11k_wmi_send_twt_disable_cmd | wmi.c:3346-3377 | cmd::TwtDisable | ported | source-derived layout |
| ath11k_wmi_send_twt_add_dialog_cmd | wmi.c:3379-3431 | cmd::TwtAddDialog | ported | source-derived layout |
| ath11k_wmi_send_twt_del_dialog_cmd | wmi.c:3433-3470 | cmd::TwtDeleteDialog | ported | source-derived layout |
| ath11k_wmi_send_twt_pause_dialog_cmd | wmi.c:3472-3510 | cmd::TwtPauseDialog | ported | source-derived layout |
| ath11k_wmi_send_twt_resume_dialog_cmd | wmi.c:3512-3553 | cmd::TwtResumeDialog | ported | source-derived layout |
| ath11k_wmi_send_obss_spr_cmd | wmi.c:3556-3592 | cmd::ObssSpatialReuse | ported | source-derived layout |
| ath11k_wmi_pdev_set_srg_bss_color_bitmap | wmi.c:3595-3630 | cmd::ObssBitmap | ported | source-derived layout |
| ath11k_wmi_pdev_set_srg_patial_bssid_bitmap | wmi.c:3633-3669 | cmd::ObssBitmap | ported | source-derived layout |
| ath11k_wmi_pdev_srg_obss_color_enable_bitmap | wmi.c:3672-3708 | cmd::ObssBitmap | ported | source-derived layout |
| ath11k_wmi_pdev_srg_obss_bssid_enable_bitmap | wmi.c:3711-3747 | cmd::ObssBitmap | ported | source-derived layout |
| ath11k_wmi_pdev_non_srg_obss_color_enable_bitmap | wmi.c:3750-3786 | cmd::ObssBitmap | ported | source-derived layout |
| ath11k_wmi_pdev_non_srg_obss_bssid_enable_bitmap | wmi.c:3789-3825 | cmd::ObssBitmap | ported | source-derived layout |
| ath11k_wmi_send_obss_color_collision_cfg_cmd | wmi.c:3828-3871 | cmd::ObssColorCollisionConfig | ported | source-derived layout |
| ath11k_wmi_send_bss_color_change_enable_cmd | wmi.c:3873-3907 | cmd::BssColorChangeEnable | ported | source-derived layout |
| ath11k_wmi_fils_discovery_tmpl | wmi.c:3909-3954 | cmd::FilsDiscoveryTemplate | ported | source-derived AP fixture |
| ath11k_wmi_peer_set_cfr_capture_conf | wmi.c:3956-3995 | cmd::PeerCfrCapture | ported | source-derived AP fixture |
| ath11k_wmi_probe_resp_tmpl | wmi.c:3997-4052 | cmd::ProbeResponseTemplate | ported | source-derived AP fixture |
| ath11k_wmi_fils_discovery | wmi.c:4054-4091 | cmd::FilsDiscovery | ported | source-derived AP fixture |
| ath11k_init_cmd_send | wmi.c:4249-4364 | cmd::Init | ported | cmd::init::tests::single_pdev_init_matches_fixed_c_layout |
| ath11k_wmi_pdev_lro_cfg | wmi.c:4366-4399 | cmd::PdevLroConfig | ported | device fixtures |
| ath11k_wmi_wait_for_service_ready | wmi.c:4401-4411 | cmd::Wmi::wait_for_service_ready | ported | event lifecycle fixtures |
| ath11k_wmi_wait_for_unified_ready | wmi.c:4413-4423 | cmd::Wmi::wait_for_unified_ready | ported | event lifecycle fixtures |
| ath11k_wmi_set_hw_mode | wmi.c:4425-4458 | cmd::PdevSetHardwareMode | ported | device fixtures |
| ath11k_wmi_cmd_init | wmi.c:4460-4489 | cmd::Init | ported | cmd::init::tests::single_pdev_init_matches_fixed_c_layout |
| ath11k_wmi_vdev_spectral_conf | wmi.c:4491-4525 | cmd::VdevSpectralConfig | ported | device fixtures |
| ath11k_wmi_vdev_spectral_enable | wmi.c:4527-4563 | cmd::VdevSpectralEnable | ported | device fixtures |
| ath11k_wmi_pdev_dma_ring_cfg | wmi.c:4565-4609 | cmd::PdevDmaRingConfig | ported | device fixtures |
| ath11k_wmi_send_unit_test_cmd | wmi.c:9073-9129 | cmd::UnitTest | ported | wow fixtures |
| ath11k_wmi_simulate_radar | wmi.c:9131-9164 | cmd::SimulateRadar | ported | wow fixtures |
| ath11k_wmi_fw_dbglog_cfg | wmi.c:9166-9220 | cmd::DebugLogConfig | ported | wow fixtures |
| ath11k_wmi_connect | wmi.c:9222-9235 | cmd::Wmi::connect | ported | lifecycle state test pending |
| ath11k_wmi_pdev_attach | wmi.c:9245-9261 | cmd::Wmi::pdev_attach | ported | lifecycle state test pending |
| ath11k_wmi_attach | wmi.c:9263-9283 | cmd::Wmi::attach | ported | lifecycle state test pending |
| ath11k_wmi_detach | wmi.c:9285-9295 | cmd::Wmi::detach | ported | lifecycle state test pending |
| ath11k_wmi_hw_data_filter_cmd | wmi.c:9297-9334 | cmd::HwDataFilter | ported | wow fixtures |
| ath11k_wmi_wow_host_wakeup_ind | wmi.c:9336-9362 | cmd::WowHostWakeup | ported | wow fixtures |
| ath11k_wmi_wow_enable | wmi.c:9364-9390 | cmd::WowEnable | ported | wow fixtures |
| ath11k_wmi_scan_prob_req_oui | wmi.c:9392-9424 | cmd::ScanProbeRequestOui | ported | source-derived layout |
| ath11k_wmi_wow_add_wakeup_event | wmi.c:9426-9458 | cmd::WowWakeEventConfig | ported | source-derived layout |
| ath11k_wmi_wow_add_pattern | wmi.c:9460-9570 | cmd::WowAddPattern | ported | wow fixtures |
| ath11k_wmi_wow_del_pattern | wmi.c:9572-9603 | cmd::WowDeletePattern | ported | source-derived layout |
| ath11k_wmi_op_gen_config_pno_start | wmi.c:9606-9716 | cmd::PnoStart | ported | wow fixtures |
| ath11k_wmi_wow_config_pno | wmi.c:9742-9763 | cmd::PnoStart / cmd::PnoStop | ported | wow fixtures |
| ath11k_wmi_arp_ns_offload | wmi.c:9870-9926 | cmd::ArpNsOffload | ported | wow fixtures |
| ath11k_wmi_gtk_rekey_offload | wmi.c:9928-9974 | cmd::GtkRekey | ported | wow fixtures |
| ath11k_wmi_gtk_rekey_getinfo | wmi.c:9976-10004 | cmd::GtkRekey | ported | wow fixtures |
| ath11k_wmi_pdev_set_bios_sar_table_param | wmi.c:10006-10051 | cmd::BiosSarTable | ported | wow fixtures |
| ath11k_wmi_pdev_set_bios_geo_table_param | wmi.c:10053-10088 | cmd::BiosGeoTable | ported | wow fixtures |
| ath11k_wmi_sta_keepalive | wmi.c:10090-10137 | cmd::StaKeepalive | ported | wow fixtures |
| transcript verifier (no C symbol) | — | cmd::golden | local-seam | Parses ordered.jsonl and reports exact/first-offset results |
