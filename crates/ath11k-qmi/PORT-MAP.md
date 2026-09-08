<!-- PORT-MAP-SCHEMA: C symbol | C file:lines | Rust item | status ∈ {ported, wcn6750-specific, local-seam, replaced-by-fuchsia-mlme, kernel-substrate, deferred, blocked} | note -->

| C symbol | C file:lines | Rust item | status | note |
|---|---|---|---|---|
| `ath11k_base` | `qmi.h:43-43` | — | kernel-substrate | Only forward-declared here; device lifecycle, MMIO, DMA, recovery, firmware lookup, and logging remain driver-owned. Range verified against oracle commit 509ce3d952d550f93b544c8d94c99e798f09a9b4, materialized with nix build .#ath11k-reference-source. |
| `ath11k_qmi_file_type` | `qmi.h:45-50` | `src/wire.rs::FileType` | ported | BDF/caldata/EEPROM selection. |
| `ath11k_qmi_bdf_type` | `qmi.h:52-56` | `src/wire.rs::BdfType` | ported | BIN, ELF, and regdb wire values. |
| `ath11k_qmi_event_type` | `qmi.h:58-74` | `src/handshake.rs::DriverEvent` | ported | Reachable protocol events are represented; Linux-only events stay with the driver owner. |
| `ath11k_qmi_driver_event` | `qmi.h:76-80` | `src/handshake.rs::DriverEvent` | local-seam | Rust state-machine events replace list allocation and workqueue plumbing. |
| `ath11k_qmi_ce_cfg` | `qmi.h:82-91` | `src/wire.rs::WlanConfigRequest` | local-seam | Core/CE supplies owned configuration instead of Linux pointers. |
| `ath11k_qmi_event_msg` | `qmi.h:93-96` | — | deferred | Unused in the pinned source. |
| `target_mem_chunk` | `qmi.h:98-109` | `src/handshake.rs::MemoryProvider; src/wire.rs::MemorySegmentResponse` | local-seam | The platform owns DMA/MMIO storage; QMI carries the resulting segment metadata. |
| `target_info` | `qmi.h:111-121` | `src/wire.rs::CapabilityResponse; src/lib.rs::FirmwareReady` | ported | Capability fields are decoded and the externally needed result is projected into FirmwareReady. |
| `m3_mem_region` | `qmi.h:123-127` | `src/handshake.rs::MemoryRegion` | local-seam | MemoryProvider owns M3 allocation and addresses. |
| `ath11k_qmi` | `qmi.h:129-147` | `src/handshake.rs::Wcn6750Handshake` | local-seam | Protocol state is retained; QRTR, workqueue, locks, DMA, and device ownership stay outside the crate. |
| `qmi_wlanfw_host_cap_req_msg_v01` | `qmi.h:161-189` | `src/wire.rs::HostCapabilityRequest` | ported | Typed QMI TLV representation. |
| `qmi_wlanfw_host_cap_resp_msg_v01` | `qmi.h:191-193` | `src/wire.rs::HostCapabilityResponse` | ported | Typed QMI TLV representation. |
| `qmi_wlanfw_ind_register_req_msg_v01` | `qmi.h:201-226` | `src/wire.rs::IndicationRegisterRequest` | ported | Typed QMI TLV representation. |
| `qmi_wlanfw_ind_register_resp_msg_v01` | `qmi.h:228-232` | `src/wire.rs::IndicationRegisterResponse` | ported | Typed QMI TLV representation. |
| `qmi_wlanfw_mem_cfg_s_v01` | `qmi.h:242-246` | `src/wire.rs::MemoryConfig` | ported | Typed QMI TLV representation. |
| `qmi_wlanfw_mem_type_enum_v01` | `qmi.h:248-257` | `src/wire.rs::MemoryType` | ported | Unknown wire values are preserved. |
| `qmi_wlanfw_mem_seg_s_v01` | `qmi.h:259-264` | `src/wire.rs::MemorySegment` | ported | Typed QMI TLV representation. |
| `qmi_wlanfw_request_mem_ind_msg_v01` | `qmi.h:266-269` | `src/wire.rs::RequestMemoryIndication` | ported | Typed QMI TLV representation. |
| `qmi_wlanfw_mem_seg_resp_s_v01` | `qmi.h:271-276` | `src/wire.rs::MemorySegmentResponse` | ported | Typed QMI TLV representation. |
| `qmi_wlanfw_respond_mem_req_msg_v01` | `qmi.h:278-281` | `src/wire.rs::RespondMemoryRequest` | ported | Typed QMI TLV representation. |
| `qmi_wlanfw_respond_mem_resp_msg_v01` | `qmi.h:283-285` | `src/wire.rs::RespondMemoryResponse` | ported | Typed QMI TLV representation. |
| `qmi_wlanfw_fw_mem_ready_ind_msg_v01` | `qmi.h:287-289` | `src/wire.rs::Indication::FirmwareMemoryReady` | ported | Empty indication payload. |
| `qmi_wlanfw_fw_ready_ind_msg_v01` | `qmi.h:291-293` | `src/wire.rs::Indication::FirmwareReady` | ported | Empty indication payload. |
| `qmi_wlanfw_fw_cold_cal_done_ind_msg_v01` | `qmi.h:295-297` | `src/wire.rs::Indication::ColdBootCalibrationDone` | ported | Empty indication payload. |
| `qmi_wlfw_fw_init_done_ind_msg_v01` | `qmi.h:299-301` | `src/wire.rs::Indication::FirmwareInitDone` | ported | Empty indication payload. |
| `qmi_wlanfw_pipedir_enum_v01` | `qmi.h:310-315` | `src/wire.rs::PipeDirection` | ported | CE configuration wire direction. |
| `qmi_wlanfw_ce_tgt_pipe_cfg_s_v01` | `qmi.h:317-323` | `src/wire.rs::TargetPipeConfig` | ported | Typed QMI TLV representation. |
| `qmi_wlanfw_ce_svc_pipe_cfg_s_v01` | `qmi.h:325-329` | `src/wire.rs::ServicePipeConfig` | ported | Typed QMI TLV representation. |
| `qmi_wlanfw_shadow_reg_cfg_s_v01` | `qmi.h:331-334` | `src/wire.rs::ShadowRegister` | ported | Typed QMI TLV representation. |
| `qmi_wlanfw_shadow_reg_v2_cfg_s_v01` | `qmi.h:336-338` | `src/wire.rs::ShadowRegister` | ported | Typed QMI TLV representation. |
| `qmi_wlanfw_memory_region_info_s_v01` | `qmi.h:340-344` | `src/wire.rs::MemoryRegionInfo` | deferred | Defined but unused and has no element-info table in the pinned qmi.c. |
| `qmi_wlanfw_rf_chip_info_s_v01` | `qmi.h:346-349` | `src/wire.rs::ChipInfo` | ported | Typed QMI TLV representation. |
| `qmi_wlanfw_rf_board_info_s_v01` | `qmi.h:351-353` | `src/wire.rs::CapabilityResponse::board_id` | ported | Typed QMI TLV representation. |
| `qmi_wlanfw_soc_info_s_v01` | `qmi.h:355-357` | `src/wire.rs::CapabilityResponse::soc_id` | ported | Typed QMI TLV representation. |
| `qmi_wlanfw_fw_version_info_s_v01` | `qmi.h:359-362` | `src/wire.rs::FirmwareVersion` | ported | Typed QMI TLV representation. |
| `qmi_wlanfw_cal_temp_id_enum_v01` | `qmi.h:364-371` | `src/wire.rs::CalibrationTemperatureId` | ported | Checked raw wire value. |
| `qmi_wlanfw_cap_resp_msg_v01` | `qmi.h:373-395` | `src/wire.rs::CapabilityResponse` | ported | Typed QMI TLV representation. |
| `qmi_wlanfw_cap_req_msg_v01` | `qmi.h:397-399` | `src/wire.rs::CapabilityRequest` | ported | Typed QMI TLV representation. |
| `qmi_wlanfw_device_info_req_msg_v01` | `qmi.h:401-403` | `src/wire.rs::DeviceInfoRequest` | ported | Typed QMI TLV representation. |
| `qmi_wlanfw_device_info_resp_msg_v01` | `qmi.h:405-411` | `src/wire.rs::DeviceInfoResponse` | ported | Typed QMI TLV representation. |
| `qmi_wlanfw_bdf_download_req_msg_v01` | `qmi.h:420-436` | `src/wire.rs::BdfDownloadRequest` | ported | Typed QMI TLV representation. |
| `qmi_wlanfw_bdf_download_resp_msg_v01` | `qmi.h:438-440` | `src/wire.rs::BdfDownloadResponse` | ported | Typed QMI TLV representation. |
| `qmi_wlanfw_m3_info_req_msg_v01` | `qmi.h:447-450` | `src/wire.rs::M3InfoRequest` | ported | Typed QMI TLV representation. |
| `qmi_wlanfw_m3_info_resp_msg_v01` | `qmi.h:452-454` | `src/wire.rs::M3InfoResponse` | ported | Typed QMI TLV representation. |
| `qmi_wlanfw_wlan_mode_req_msg_v01` | `qmi.h:472-476` | `src/wire.rs::WlanModeRequest` | ported | Typed QMI TLV representation. |
| `qmi_wlanfw_wlan_mode_resp_msg_v01` | `qmi.h:478-480` | `src/wire.rs::WlanModeResponse` | ported | Typed QMI TLV representation. |
| `qmi_wlanfw_wlan_cfg_req_msg_v01` | `qmi.h:482-501` | `src/wire.rs::WlanConfigRequest` | ported | Typed QMI TLV representation. |
| `qmi_wlanfw_wlan_cfg_resp_msg_v01` | `qmi.h:503-505` | `src/wire.rs::WlanConfigResponse` | ported | Typed QMI TLV representation. |
| `qmi_wlanfw_wlan_ini_req_msg_v01` | `qmi.h:507-511` | `src/wire.rs::WlanIniRequest` | ported | Typed QMI TLV representation. |
| `qmi_wlanfw_wlan_ini_resp_msg_v01` | `qmi.h:513-515` | `src/wire.rs::WlanIniResponse` | ported | Typed QMI TLV representation. |
| `qmi_wlanfw_host_cap_req_msg_v01_ei` | `qmi.c:33-282` | `src/wire.rs::HostCapabilityRequest::encode` | ported | Pinned TLV schema implemented by the named codec. |
| `qmi_wlanfw_host_cap_resp_msg_v01_ei` | `qmi.c:284-299` | `src/wire.rs::StandardResponse::decode` | ported | Pinned TLV schema implemented by the named codec. |
| `qmi_wlanfw_ind_register_req_msg_v01_ei` | `qmi.c:301-524` | `src/wire.rs::IndicationRegisterRequest::encode` | ported | Pinned TLV schema implemented by the named codec. |
| `qmi_wlanfw_ind_register_resp_msg_v01_ei` | `qmi.c:526-560` | `src/wire.rs::IndicationRegisterResponse::decode` | ported | Pinned TLV schema implemented by the named codec. |
| `qmi_wlanfw_mem_cfg_s_v01_ei` | `qmi.c:562-592` | `src/wire.rs::MemoryConfig` | ported | Pinned TLV schema implemented by the named codec. |
| `qmi_wlanfw_mem_seg_s_v01_ei` | `qmi.c:594-634` | `src/wire.rs::MemorySegment` | ported | Pinned TLV schema implemented by the named codec. |
| `qmi_wlanfw_request_mem_ind_msg_v01_ei` | `qmi.c:636-661` | `src/wire.rs::RequestMemoryIndication::decode` | ported | Pinned TLV schema implemented by the named codec. |
| `qmi_wlanfw_mem_seg_resp_s_v01_ei` | `qmi.c:663-701` | `src/wire.rs::MemorySegmentResponse` | ported | Pinned TLV schema implemented by the named codec. |
| `qmi_wlanfw_respond_mem_req_msg_v01_ei` | `qmi.c:703-728` | `src/wire.rs::RespondMemoryRequest::encode` | ported | Pinned TLV schema implemented by the named codec. |
| `qmi_wlanfw_respond_mem_resp_msg_v01_ei` | `qmi.c:730-746` | `src/wire.rs::StandardResponse::decode` | ported | Pinned TLV schema implemented by the named codec. |
| `qmi_wlanfw_cap_req_msg_v01_ei` | `qmi.c:748-754` | `src/wire.rs::CapabilityRequest::encode` | ported | Pinned TLV schema implemented by the named codec. |
| `qmi_wlanfw_device_info_req_msg_v01_ei` | `qmi.c:756-762` | `src/wire.rs::DeviceInfoRequest::encode` | ported | Pinned TLV schema implemented by the named codec. |
| `qmi_wlfw_device_info_resp_msg_v01_ei` | `qmi.c:764-816` | `src/wire.rs::DeviceInfoResponse::decode` | ported | Pinned TLV schema implemented by the named codec. |
| `qmi_wlanfw_rf_chip_info_s_v01_ei` | `qmi.c:818-842` | `src/wire.rs::ChipInfo` | ported | Pinned TLV schema implemented by the named codec. |
| `qmi_wlanfw_rf_board_info_s_v01_ei` | `qmi.c:844-859` | `src/wire.rs::CapabilityResponse::board_id` | ported | Pinned TLV schema implemented by the named codec. |
| `qmi_wlanfw_soc_info_s_v01_ei` | `qmi.c:861-875` | `src/wire.rs::CapabilityResponse::soc_id` | ported | Pinned TLV schema implemented by the named codec. |
| `qmi_wlanfw_fw_version_info_s_v01_ei` | `qmi.c:877-901` | `src/wire.rs::FirmwareVersion` | ported | Pinned TLV schema implemented by the named codec. |
| `qmi_wlanfw_cap_resp_msg_v01_ei` | `qmi.c:903-1102` | `src/wire.rs::CapabilityResponse::decode` | ported | Pinned TLV schema implemented by the named codec. |
| `qmi_wlanfw_bdf_download_req_msg_v01_ei` | `qmi.c:1104-1237` | `src/wire.rs::BdfDownloadRequest::encode` | ported | Pinned TLV schema implemented by the named codec. |
| `qmi_wlanfw_bdf_download_resp_msg_v01_ei` | `qmi.c:1239-1255` | `src/wire.rs::StandardResponse::decode` | ported | Pinned TLV schema implemented by the named codec. |
| `qmi_wlanfw_m3_info_req_msg_v01_ei` | `qmi.c:1257-1279` | `src/wire.rs::M3InfoRequest::encode` | ported | Pinned TLV schema implemented by the named codec. |
| `qmi_wlanfw_m3_info_resp_msg_v01_ei` | `qmi.c:1281-1296` | `src/wire.rs::StandardResponse::decode` | ported | Pinned TLV schema implemented by the named codec. |
| `qmi_wlanfw_ce_tgt_pipe_cfg_s_v01_ei` | `qmi.c:1298-1349` | `src/wire.rs::TargetPipeConfig` | ported | Pinned TLV schema implemented by the named codec. |
| `qmi_wlanfw_ce_svc_pipe_cfg_s_v01_ei` | `qmi.c:1351-1384` | `src/wire.rs::ServicePipeConfig` | ported | Pinned TLV schema implemented by the named codec. |
| `qmi_wlanfw_shadow_reg_cfg_s_v01_ei` | `qmi.c:1386-1408` | `src/wire.rs::ShadowRegister` | ported | Pinned TLV schema implemented by the named codec. |
| `qmi_wlanfw_shadow_reg_v2_cfg_s_v01_ei` | `qmi.c:1410-1425` | `src/wire.rs::ShadowRegister` | ported | Pinned TLV schema implemented by the named codec. |
| `qmi_wlanfw_wlan_mode_req_msg_v01_ei` | `qmi.c:1427-1460` | `src/wire.rs::WlanModeRequest::encode` | ported | Pinned TLV schema implemented by the named codec. |
| `qmi_wlanfw_wlan_mode_resp_msg_v01_ei` | `qmi.c:1462-1478` | `src/wire.rs::StandardResponse::decode` | ported | Pinned TLV schema implemented by the named codec. |
| `qmi_wlanfw_wlan_cfg_req_msg_v01_ei` | `qmi.c:1480-1617` | `src/wire.rs::WlanConfigRequest::encode` | ported | Pinned TLV schema implemented by the named codec. |
| `qmi_wlanfw_wlan_cfg_resp_msg_v01_ei` | `qmi.c:1619-1634` | `src/wire.rs::StandardResponse::decode` | ported | Pinned TLV schema implemented by the named codec. |
| `qmi_wlanfw_mem_ready_ind_msg_v01_ei` | `qmi.c:1636-1641` | `src/wire.rs::Indication::decode` | ported | Pinned TLV schema implemented by the named codec. |
| `qmi_wlanfw_fw_ready_ind_msg_v01_ei` | `qmi.c:1643-1648` | `src/wire.rs::Indication::decode` | ported | Pinned TLV schema implemented by the named codec. |
| `qmi_wlanfw_cold_boot_cal_done_ind_msg_v01_ei` | `qmi.c:1650-1655` | `src/wire.rs::Indication::decode` | ported | Pinned TLV schema implemented by the named codec. |
| `qmi_wlanfw_wlan_ini_req_msg_v01_ei` | `qmi.c:1657-1681` | `src/wire.rs::WlanIniRequest::encode` | ported | Pinned TLV schema implemented by the named codec. |
| `qmi_wlanfw_wlan_ini_resp_msg_v01_ei` | `qmi.c:1683-1699` | `src/wire.rs::StandardResponse::decode` | ported | Pinned TLV schema implemented by the named codec. |
| `qmi_wlfw_fw_init_done_ind_msg_v01_ei` | `qmi.c:1701-1706` | `src/wire.rs::Indication::decode` | ported | Pinned TLV schema implemented by the named codec. |
| `ath11k_qmi_host_cap_send` | `qmi.c:1710-1791` | `src/handshake.rs::Wcn6750Handshake::server_arrived` | ported | Host-capability transaction. |
| `ath11k_qmi_fw_ind_register_send` | `qmi.c:1793-1870` | `src/handshake.rs::Wcn6750Handshake::server_arrived` | ported | Indication-registration transaction. |
| `ath11k_qmi_respond_fw_mem_request` | `qmi.c:1872-1954` | `src/handshake.rs::Wcn6750Handshake::process_indication` | ported | Memory-response transaction; addresses come from MemoryProvider. |
| `ath11k_qmi_free_target_mem_chunk` | `qmi.c:1956-1977` | `src/handshake.rs::MemoryProvider` | kernel-substrate | DMA/MMIO release is platform-owned. |
| `ath11k_qmi_alloc_target_mem_chunk` | `qmi.c:1979-2037` | `src/handshake.rs::MemoryProvider::provision` | local-seam | Platform allocation policy is injected. |
| `ath11k_qmi_assign_target_mem_chunk` | `qmi.c:2039-2118` | `src/handshake.rs::MemoryProvider::provision` | wcn6750-specific | WCN6750 fixed reserved-memory assignment is supplied by the platform seam. |
| `ath11k_qmi_request_device_info` | `qmi.c:2120-2195` | `src/wire.rs::DeviceInfoRequest; src/wire.rs::DeviceInfoResponse` | deferred | Hybrid-bus BAR validation/mapping is not used by the WCN6750 handshake. |
| `ath11k_qmi_request_target_cap` | `qmi.c:2197-2295` | `src/handshake.rs::Wcn6750Handshake::capabilities` | ported | The pinned source has no PHY-capability message; the 0x0024 target-capability transaction is the capability step. |
| `ath11k_qmi_load_file_target_mem` | `qmi.c:2297-2411` | `src/handshake.rs::Wcn6750Handshake::download` | ported | Segmented BDF/caldata/EEPROM download; fixed-address copying remains platform-owned. |
| `ath11k_qmi_load_bdf_qmi` | `qmi.c:2413-2509` | `src/handshake.rs::Wcn6750Handshake::load_bdf` | ported | FirmwareAssets supplies file and board discovery. |
| `ath11k_qmi_m3_load` | `qmi.c:2511-2564` | `src/handshake.rs::FirmwareAssets::m3_firmware; src/handshake.rs::MemoryProvider::load_m3` | local-seam | Firmware acquisition and memory allocation are injected. |
| `ath11k_qmi_m3_free` | `qmi.c:2566-2577` | `src/handshake.rs::MemoryProvider` | kernel-substrate | M3 DMA release is platform-owned. |
| `ath11k_qmi_wlanfw_m3_info_send` | `qmi.c:2581-2638` | `src/handshake.rs::Wcn6750Handshake::load_bdf` | ported | M3-info transaction. |
| `ath11k_qmi_wlanfw_mode_send` | `qmi.c:2640-2693` | `src/wire.rs::WlanModeRequest::encode` | ported | WLAN-mode transaction. |
| `ath11k_qmi_wlanfw_wlan_cfg_send` | `qmi.c:2695-2784` | `src/wire.rs::WlanConfigRequest::encode` | ported | CE/service/shadow configuration transaction. |
| `ath11k_qmi_wlanfw_wlan_ini_send` | `qmi.c:2786-2826` | `src/wire.rs::WlanIniRequest::encode` | ported | Optional diagnostic initialization transaction. |
| `ath11k_qmi_firmware_stop` | `qmi.c:2828-2839` | `src/handshake.rs::Wcn6750Handshake::firmware_stop` | ported | Mode-off lifecycle transaction. |
| `ath11k_qmi_firmware_start` | `qmi.c:2841-2869` | `src/handshake.rs::Wcn6750Handshake::firmware_start` | ported | Optional INI, WLAN configuration, and mode-on sequence. |
| `ath11k_qmi_fwreset_from_cold_boot` | `qmi.c:2871-2895` | — | replaced-by-fuchsia-mlme | Post-calibration device reset orchestration belongs to the driver lifecycle owner. |
| `ath11k_qmi_process_coldboot_calibration` | `qmi.c:2898-2922` | `src/handshake.rs::Wcn6750Handshake::start_cold_boot_calibration` | ported | Calibration start and completion wait; reset remains with the lifecycle owner. |
| `ath11k_qmi_driver_event_post` | `qmi.c:2925-2945` | `src/handshake.rs::Wcn6750Handshake::process_next_event` | local-seam | Direct Rust state transitions replace list/spinlock/workqueue enqueueing. |
| `ath11k_qmi_event_mem_request` | `qmi.c:2947-2959` | `src/handshake.rs::Wcn6750Handshake::process_indication` | ported | Memory-request transition. |
| `ath11k_qmi_event_load_bdf` | `qmi.c:2961-2989` | `src/handshake.rs::Wcn6750Handshake::load_bdf` | ported | Capability and BDF-loading transition. |
| `ath11k_qmi_event_server_arrive` | `qmi.c:2991-3019` | `src/handshake.rs::Wcn6750Handshake::server_arrived` | ported | Registration and host-capability transition. |
| `ath11k_qmi_msg_mem_request_cb` | `qmi.c:3021-3065` | `src/handshake.rs::Wcn6750Handshake::process_indication` | ported | Request-memory indication handling; allocation is delegated. |
| `ath11k_qmi_msg_mem_ready_cb` | `qmi.c:3067-3077` | `src/wire.rs::Indication::FirmwareMemoryReady` | ported | Firmware-memory-ready handling. |
| `ath11k_qmi_msg_fw_ready_cb` | `qmi.c:3079-3095` | `src/wire.rs::Indication::FirmwareReady` | ported | Firmware-ready handling. |
| `ath11k_qmi_msg_cold_boot_cal_done_cb` | `qmi.c:3097-3109` | `src/wire.rs::Indication::ColdBootCalibrationDone` | ported | Cold-calibration-done handling. |
| `ath11k_qmi_msg_fw_init_done_cb` | `qmi.c:3111-3122` | `src/wire.rs::Indication::FirmwareInitDone` | ported | Firmware-init-done handling. |
| `ath11k_qmi_msg_handlers` | `qmi.c:3124-3165` | `src/wire.rs::MessageId; src/wire.rs::Indication::decode` | ported | Five indication IDs dispatch to typed decoding. |
| `ath11k_qmi_ops_new_server` | `qmi.c:3167-3190` | `src/lib.rs::Transport::start_service; src/handshake.rs::Wcn6750Handshake::server_arrived` | local-seam | Transport supplies discovery/connect; protocol arrival handling is retained. |
| `ath11k_qmi_ops_del_server` | `qmi.c:3192-3200` | `src/lib.rs::Transport` | replaced-by-fuchsia-mlme | Transport reports service loss; recovery policy belongs to the driver lifecycle owner. |
| `ath11k_qmi_ops` | `qmi.c:3202-3205` | `src/lib.rs::Transport` | local-seam | Caller-supplied transport replaces qmi_ops registration. |
| `ath11k_qmi_driver_event_work` | `qmi.c:3207-3316` | `src/handshake.rs::Wcn6750Handshake::process_next_event` | ported | Ordered protocol dispatch is retained; Linux workqueue and recovery actions are not. |
| `ath11k_qmi_init_service` | `qmi.c:3318-3354` | `src/handshake.rs::Wcn6750Handshake::init_service` | local-seam | Constructs protocol state through the caller-supplied transport. |
| `ath11k_qmi_deinit_service` | `qmi.c:3356-3363` | `src/handshake.rs::Wcn6750Handshake::deinit_service` | local-seam | Cancels protocol activity; socket/workqueue/DMA teardown stays with owners. |
| `ath11k_qmi_free_resource` | `qmi.c:3366-3370` | `src/handshake.rs::MemoryProvider` | kernel-substrate | Target and M3 memory release is platform-owned. |
