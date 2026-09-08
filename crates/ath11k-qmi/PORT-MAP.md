# ath11k QMI port map

The source oracle is `drivers/net/wireless/ath/ath11k/qmi.[ch]` at
`sc7280-mainline/linux` commit
`509ce3d952d550f93b544c8d94c99e798f09a9b4`, materialized with
`nix build .#ath11k-reference-source`. This inventory covers every type and
function defined by those two files and every static `qmi_elem_info` table.
Protocol layouts are implemented in `src/wire.rs`; lifecycle transitions are in `src/handshake.rs`.

Status vocabulary follows the crate inventory convention: **stub** is present
only as scaffolding or wholly unimplemented, **ported** is implemented,
**oracle-checked** has byte-exact source fixtures, and **hardware-checked** has
been exercised against the target. **replaced** is a deliberate project
boundary rather than code to translate; **deferred** is a Linux/device
integration path not owned by this protocol crate. Source-derived fixtures cover representative scalar, array, nested, response, and empty-indication tables; live hardware checking remains pending.

## Current Rust surface

| Rust item | Status | C responsibility |
|---|---|---|
| `Request` / `Response` / `RawIndication` | **ported** | Message-ID-bearing, bounded checked QMI TLV bodies. |
| `FirmwareReady` / `DriverEvent` | **ported** | Typed lifecycle outcomes after firmware-ready/init-done. |
| `QmiError` | **ported** | Transport, malformed input, timeout, and QMI result/error. |
| `Transport` | **ported/replaced** | Event-driven service discovery, send, response, and unsolicited indication boundary; AF_QIPCRTR stays outside. |
| `Wcn6750Handshake` | **ported** | Registration, host/target capabilities, memory/BDF/caldata/M3 exchange, start/stop, and readiness sequencing. |
| `MemoryProvider` / `FirmwareAssets` | **replaced** | Caller-owned DMA/MMIO and firmware lookup without upward dependencies. |

## C enums

| C enum | Intended Rust item | Status / disposition |
|---|---|---|
| `ath11k_qmi_file_type` | `FileType` (board, calibration, EEPROM download selection) | **ported** |
| `ath11k_qmi_bdf_type` | `BdfType` (BIN, ELF, regdb wire value) | **ported** |
| `ath11k_qmi_event_type` | `DriverEvent` | **ported**; Linux-only queue mechanics are replaced, but its reachable state transitions are required |
| `qmi_wlanfw_mem_type_enum_v01` | `MemoryType` wire enum | **ported** |
| `qmi_wlanfw_pipedir_enum_v01` | `PipeDirection` wire enum, populated from the CE configuration boundary | **ported** |
| `qmi_wlanfw_cal_temp_id_enum_v01` | `CalibrationTemperatureId`/checked raw wire value | **ported** |

## C structs

### Driver state and integration structs

| C struct | Intended Rust item | Status / disposition |
|---|---|---|
| `ath11k_qmi_driver_event` | `DriverEvent` | **ported**; `list_head`, allocation, spinlock and workqueue are **replaced** by the eventual Rust state machine |
| `ath11k_qmi_ce_cfg` | `WlanConfigRequest` | **ported**; source data is supplied by core/CE rather than Linux pointers |
| `ath11k_qmi_event_msg` | none | **deferred**; unused in pinned `qmi.[ch]` |
| `target_mem_chunk` | `MemorySegmentResponse` from `MemoryProvider` | **replaced**; raw DMA addresses, `__iomem` pointers and allocation are **replaced** by the platform boundary |
| `target_info` | private capability state projected into `FirmwareReady` | **ported** |
| `m3_mem_region` | `MemoryRegion` from `MemoryProvider` | **replaced**; M3 firmware acquisition/allocation belongs to core/platform |
| `ath11k_qmi` | `Wcn6750Handshake` | **ported**; qmi handle/socket/workqueue/list/locks are **replaced**, protocol state is still required |

### Wire structs

| C struct | Intended Rust wire item | Status / disposition |
|---|---|---|
| `qmi_wlanfw_host_cap_req_msg_v01` | `HostCapabilityRequest` | **ported** |
| `qmi_wlanfw_host_cap_resp_msg_v01` | `HostCapabilityResponse` | **ported** |
| `qmi_wlanfw_ind_register_req_msg_v01` | `IndicationRegisterRequest` | **ported** |
| `qmi_wlanfw_ind_register_resp_msg_v01` | `IndicationRegisterResponse` | **ported** |
| `qmi_wlanfw_mem_cfg_s_v01` | `MemoryConfig` | **ported** |
| `qmi_wlanfw_mem_seg_s_v01` | `MemorySegmentRequest` | **ported** |
| `qmi_wlanfw_request_mem_ind_msg_v01` | `RequestMemoryIndication` | **ported** |
| `qmi_wlanfw_mem_seg_resp_s_v01` | `MemorySegmentResponse` | **ported** |
| `qmi_wlanfw_respond_mem_req_msg_v01` | `RespondMemoryRequest` | **ported** |
| `qmi_wlanfw_respond_mem_resp_msg_v01` | `RespondMemoryResponse` | **ported** |
| `qmi_wlanfw_fw_mem_ready_ind_msg_v01` | `FirmwareMemoryReadyIndication` (empty payload) | **ported** |
| `qmi_wlanfw_fw_ready_ind_msg_v01` | `FirmwareReadyIndication` (empty payload) | **ported**; not equivalent to current summary `FirmwareReady` |
| `qmi_wlanfw_fw_cold_cal_done_ind_msg_v01` | `ColdCalibrationDoneIndication` (empty payload) | **ported** |
| `qmi_wlfw_fw_init_done_ind_msg_v01` | `FirmwareInitDoneIndication` (empty payload) | **ported** |
| `qmi_wlanfw_ce_tgt_pipe_cfg_s_v01` | `TargetPipeConfig` | **ported** |
| `qmi_wlanfw_ce_svc_pipe_cfg_s_v01` | `ServicePipeConfig` | **ported** |
| `qmi_wlanfw_shadow_reg_cfg_s_v01` | `ShadowRegisterConfig` | **ported**; source currently sends v1 as invalid |
| `qmi_wlanfw_shadow_reg_v2_cfg_s_v01` | `ShadowRegisterV2Config` | **ported** |
| `qmi_wlanfw_memory_region_info_s_v01` | `MemoryRegionInfo` | **deferred**; defined but unused and has no element-info table in pinned `qmi.c` |
| `qmi_wlanfw_rf_chip_info_s_v01` | `RfChipInfo` | **ported** |
| `qmi_wlanfw_rf_board_info_s_v01` | `RfBoardInfo` | **ported** |
| `qmi_wlanfw_soc_info_s_v01` | `SocInfo` | **ported** |
| `qmi_wlanfw_fw_version_info_s_v01` | `FirmwareVersionInfo` | **ported**; only the destination projection `FirmwareReady.firmware_version` exists |
| `qmi_wlanfw_cap_resp_msg_v01` | `CapabilityResponse` | **ported** |
| `qmi_wlanfw_cap_req_msg_v01` | `CapabilityRequest` (empty payload) | **ported** |
| `qmi_wlanfw_device_info_req_msg_v01` | `DeviceInfoRequest` (empty payload) | **ported**; hybrid-bus-only path |
| `qmi_wlanfw_device_info_resp_msg_v01` | `DeviceInfoResponse` | **ported**; BAR mapping is **replaced** by platform/core |
| `qmi_wlanfw_bdf_download_req_msg_v01` | `BdfDownloadRequest` | **ported** |
| `qmi_wlanfw_bdf_download_resp_msg_v01` | `BdfDownloadResponse` | **ported** |
| `qmi_wlanfw_m3_info_req_msg_v01` | `M3InfoRequest` | **ported** |
| `qmi_wlanfw_m3_info_resp_msg_v01` | `M3InfoResponse` | **ported** |
| `qmi_wlanfw_wlan_mode_req_msg_v01` | `WlanModeRequest` | **ported** |
| `qmi_wlanfw_wlan_mode_resp_msg_v01` | `WlanModeResponse` | **ported** |
| `qmi_wlanfw_wlan_cfg_req_msg_v01` | `WlanConfigRequest` | **ported** |
| `qmi_wlanfw_wlan_cfg_resp_msg_v01` | `WlanConfigResponse` | **ported** |
| `qmi_wlanfw_wlan_ini_req_msg_v01` | `WlanIniRequest` | **ported**; optional diagnostic-event path |
| `qmi_wlanfw_wlan_ini_resp_msg_v01` | `WlanIniResponse` | **ported** |

`struct ath11k_base` is only forward-declared here, not defined by these files;
its QMI-relevant inputs become arguments/configuration, while device lifecycle,
firmware lookup, MMIO, DMA, recovery and logging stay outside this crate.

## Static `qmi_elem_info` tables

Each table below is the pinned C schema for the corresponding typed TLV codec.
All are **ported**. Empty indication tables are recognized and require empty payloads. Empty indication tables still matter because their message IDs must be
recognized and their payload must be validated as empty.

| C table | Intended Rust codec | Status |
|---|---|---|
| `qmi_wlanfw_host_cap_req_msg_v01_ei` | encode `HostCapabilityRequest` | **ported** |
| `qmi_wlanfw_host_cap_resp_msg_v01_ei` | decode `HostCapabilityResponse` | **ported** |
| `qmi_wlanfw_ind_register_req_msg_v01_ei` | encode `IndicationRegisterRequest` | **ported** |
| `qmi_wlanfw_ind_register_resp_msg_v01_ei` | decode `IndicationRegisterResponse` | **ported** |
| `qmi_wlanfw_mem_cfg_s_v01_ei` | nested `MemoryConfig` codec | **ported** |
| `qmi_wlanfw_mem_seg_s_v01_ei` | nested `MemorySegmentRequest` codec | **ported** |
| `qmi_wlanfw_request_mem_ind_msg_v01_ei` | decode `RequestMemoryIndication` | **ported** |
| `qmi_wlanfw_mem_seg_resp_s_v01_ei` | nested `MemorySegmentResponse` codec | **ported** |
| `qmi_wlanfw_respond_mem_req_msg_v01_ei` | encode `RespondMemoryRequest` | **ported** |
| `qmi_wlanfw_respond_mem_resp_msg_v01_ei` | decode `RespondMemoryResponse` | **ported** |
| `qmi_wlanfw_cap_req_msg_v01_ei` | encode empty `CapabilityRequest` | **ported** |
| `qmi_wlanfw_device_info_req_msg_v01_ei` | encode empty `DeviceInfoRequest` | **ported** |
| `qmi_wlfw_device_info_resp_msg_v01_ei` | decode `DeviceInfoResponse` (pinned spelling differs from its struct) | **ported** |
| `qmi_wlanfw_rf_chip_info_s_v01_ei` | nested `RfChipInfo` codec | **ported** |
| `qmi_wlanfw_rf_board_info_s_v01_ei` | nested `RfBoardInfo` codec | **ported** |
| `qmi_wlanfw_soc_info_s_v01_ei` | nested `SocInfo` codec | **ported** |
| `qmi_wlanfw_fw_version_info_s_v01_ei` | nested `FirmwareVersionInfo` codec | **ported** |
| `qmi_wlanfw_cap_resp_msg_v01_ei` | decode `CapabilityResponse` | **ported** |
| `qmi_wlanfw_bdf_download_req_msg_v01_ei` | encode `BdfDownloadRequest` | **ported** |
| `qmi_wlanfw_bdf_download_resp_msg_v01_ei` | decode `BdfDownloadResponse` | **ported** |
| `qmi_wlanfw_m3_info_req_msg_v01_ei` | encode `M3InfoRequest` | **ported** |
| `qmi_wlanfw_m3_info_resp_msg_v01_ei` | decode `M3InfoResponse` | **ported** |
| `qmi_wlanfw_ce_tgt_pipe_cfg_s_v01_ei` | nested `TargetPipeConfig` codec | **ported** |
| `qmi_wlanfw_ce_svc_pipe_cfg_s_v01_ei` | nested `ServicePipeConfig` codec | **ported** |
| `qmi_wlanfw_shadow_reg_cfg_s_v01_ei` | nested `ShadowRegisterConfig` codec | **ported** |
| `qmi_wlanfw_shadow_reg_v2_cfg_s_v01_ei` | nested `ShadowRegisterV2Config` codec | **ported** |
| `qmi_wlanfw_wlan_mode_req_msg_v01_ei` | encode `WlanModeRequest` | **ported** |
| `qmi_wlanfw_wlan_mode_resp_msg_v01_ei` | decode `WlanModeResponse` | **ported** |
| `qmi_wlanfw_wlan_cfg_req_msg_v01_ei` | encode `WlanConfigRequest` | **ported** |
| `qmi_wlanfw_wlan_cfg_resp_msg_v01_ei` | decode `WlanConfigResponse` | **ported** |
| `qmi_wlanfw_mem_ready_ind_msg_v01_ei` | decode empty `FirmwareMemoryReadyIndication` | **ported** |
| `qmi_wlanfw_fw_ready_ind_msg_v01_ei` | decode empty `FirmwareReadyIndication` | **ported** |
| `qmi_wlanfw_cold_boot_cal_done_ind_msg_v01_ei` | decode empty `ColdCalibrationDoneIndication` | **ported** |
| `qmi_wlanfw_wlan_ini_req_msg_v01_ei` | encode `WlanIniRequest` | **ported** |
| `qmi_wlanfw_wlan_ini_resp_msg_v01_ei` | decode `WlanIniResponse` | **ported** |
| `qmi_wlfw_fw_init_done_ind_msg_v01_ei` | decode empty `FirmwareInitDoneIndication` | **ported** |
The other static protocol dispatch objects are
`ath11k_qmi_msg_handlers` (five indication IDs/callbacks) and
`ath11k_qmi_ops` (server arrival/removal). Their protocol behavior is **ported**; socket registration itself is **replaced** by the caller-supplied transport.

## C functions

| C function | Intended Rust owner/item | Status / disposition |
|---|---|---|
| `ath11k_qmi_host_cap_send` | handshake host-capability transaction | **ported** |
| `ath11k_qmi_fw_ind_register_send` | handshake indication-registration transaction | **ported** |
| `ath11k_qmi_respond_fw_mem_request` | handshake memory-response transaction | **ported**; DMA addresses come from platform tokens |
| `ath11k_qmi_free_target_mem_chunk` | platform/core memory lifecycle | **replaced** |
| `ath11k_qmi_alloc_target_mem_chunk` | platform DMA allocation policy | **replaced**; retry/delayed-response semantics remain handshake inputs |
| `ath11k_qmi_assign_target_mem_chunk` | platform reserved-memory assignment | **replaced/deferred**; DT, `ioremap` and physical addresses cannot enter this crate |
| `ath11k_qmi_request_device_info` | handshake device-info transaction | **ported** for protocol; hybrid BAR validation/mapping is **deferred** to platform/core |
| `ath11k_qmi_request_target_cap` | handshake capability transaction and `TargetInfo` projection | **ported**; only destination field `FirmwareReady.firmware_version` exists |
| `ath11k_qmi_load_file_target_mem` | segmented BDF/caldata/EEPROM download transactions | **ported**; fixed-address copy is **deferred** to platform |
| `ath11k_qmi_load_bdf_qmi` | core firmware selection plus handshake BDF/regdb download | protocol send **ported**; file/board discovery **deferred** to core |
| `ath11k_qmi_m3_load` | core firmware acquisition plus platform DMA allocation | **deferred** |
| `ath11k_qmi_m3_free` | platform/core M3 memory lifecycle | **replaced/deferred** |
| `ath11k_qmi_wlanfw_m3_info_send` | handshake M3-info transaction | **ported** |
| `ath11k_qmi_wlanfw_mode_send` | handshake WLAN-mode transaction | **ported** |
| `ath11k_qmi_wlanfw_wlan_cfg_send` | handshake CE/service/shadow configuration transaction | **ported** |
| `ath11k_qmi_wlanfw_wlan_ini_send` | optional handshake diagnostic initialization transaction | **ported** |
| `ath11k_qmi_firmware_stop` | lifecycle stop using WLAN mode-off | **ported**; no Rust stop contract exists |
| `ath11k_qmi_firmware_start` | lifecycle start tail (optional INI, WLAN config, mode) | **ported**; intended beneath `Handshake::start`/core lifecycle |
| `ath11k_qmi_fwreset_from_cold_boot` | core/platform reset orchestration | **deferred**; QMI only reports calibration completion |
| `ath11k_qmi_process_coldboot_calibration` | handshake cold-boot mode and completion wait | **stub**; reset remains platform/core-owned |
| `ath11k_qmi_driver_event_post` | private handshake event enqueue | **ported**; Linux list/spinlock/workqueue mechanics **replaced** |
| `ath11k_qmi_event_mem_request` | handshake memory-request transition | **ported** |
| `ath11k_qmi_event_load_bdf` | handshake capability/device-info/BDF transition | **ported** |
| `ath11k_qmi_event_server_arrive` | handshake registration/host-cap transition | **ported**; discovery/connect supplied by transport |
| `ath11k_qmi_msg_mem_request_cb` | decode and handle request-memory indication | **ported**; allocation delegated to platform |
| `ath11k_qmi_msg_mem_ready_cb` | handle firmware-memory-ready indication | **ported** |
| `ath11k_qmi_msg_fw_ready_cb` | handle firmware-ready indication | **ported** |
| `ath11k_qmi_msg_cold_boot_cal_done_cb` | handle calibration-done indication | **ported** |
| `ath11k_qmi_msg_fw_init_done_cb` | handle firmware-init-done indication | **ported** |
| `ath11k_qmi_ops_new_server` | transport service-arrival notification | protocol transition **ported**; AF_QIPCRTR `kernel_connect` **replaced** |
| `ath11k_qmi_ops_del_server` | transport service-loss notification/recovery signal | protocol transition **ported**; Linux recovery flags **deferred** to core |
| `ath11k_qmi_driver_event_work` | ordered handshake state-machine dispatch | **ported**; Linux workqueue/flags/recovery calls **replaced/deferred** |
| `ath11k_qmi_init_service` | construct handshake and register WLFW service/indications | **ported**; `qmi_handle`, lookup and workqueue setup **replaced** |
| `ath11k_qmi_deinit_service` | cancel handshake and release protocol-owned resources | **ported**; socket/workqueue/DMA teardown **replaced** by owners |
| `ath11k_qmi_free_resource` | release target/M3 memory | **replaced/deferred** to platform/core |

## Known oracle limitation

The pinned source contains no PHY-capability QMI message or element-info table; the
`0x0024` target-capability transaction is the capability step ported here. Native
kernel tracepoints do not expose QMI payload bytes, so hardware-check status remains
pending a live firmware integration; source-derived TLV fixtures are the current oracle.
