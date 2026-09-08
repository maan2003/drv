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
