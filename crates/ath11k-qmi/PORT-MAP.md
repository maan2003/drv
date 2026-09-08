# ath11k QMI port map

The source oracle is `drivers/net/wireless/ath/ath11k/qmi.[ch]` at
`sc7280-mainline/linux` commit
`509ce3d952d550f93b544c8d94c99e798f09a9b4`, materialized with
`nix build .#ath11k-reference-source`. This inventory covers every type and
function defined by those two files and every static `qmi_elem_info` table.
Items marked stub have no protocol-complete implementation in `src/lib.rs` today.

Status vocabulary follows the crate inventory convention: **stub** is present
only as scaffolding or wholly unimplemented, **ported** is implemented,
**oracle-checked** has byte-exact source fixtures, and **hardware-checked** has
been exercised against the target. **replaced** is a deliberate project
boundary rather than code to translate; **deferred** is a Linux/device
integration path not owned by this protocol crate. No mapped protocol item is
yet oracle-checked or hardware-checked.

## Current Rust surface and material gaps

| Rust item | Status | C responsibility |
|---|---|---|
| `Request` | **stub** | Opaque, length-bounded TLV bytes only; it implements none of the typed request layouts or element encoders below. |
| `Response` | **stub** | Opaque bytes only; it implements none of the response/indication decoders or QMI result validation below. |
| `FirmwareReady` | **stub** | Only `firmware_version` and `target_mem_mode`; it does not represent `target_info` or prove that the source handshake reached FW-ready/init-done. |
| `QmiError` | **ported** | Portable error vocabulary replacing Linux errno at the public boundary. |
| `Transport` | **stub/replaced** | Replaces AF_QIPCRTR and Linux QMI transactions with caller-supplied transport, but `transact` has no server-discovery or unsolicited-indication receive seam. The source handshake cannot yet be implemented through it. |
| `Handshake` | **stub** | Intended owner of registration, capabilities, memory/BDF/M3 exchange, configuration, mode selection, and readiness sequencing. |

## C enums

| C enum | Intended Rust item | Status / disposition |
|---|---|---|
| `ath11k_qmi_file_type` | `FileType` (board, calibration, EEPROM download selection) | **stub** |
| `ath11k_qmi_bdf_type` | `BdfType` (BIN, ELF, regdb wire value) | **stub** |
| `ath11k_qmi_event_type` | private handshake event/state enum | **stub**; Linux-only queue mechanics are replaced, but its reachable state transitions are required |
| `qmi_wlanfw_mem_type_enum_v01` | `MemoryType` wire enum | **stub** |
| `qmi_wlanfw_pipedir_enum_v01` | `PipeDirection` wire enum, populated from the CE configuration boundary | **stub** |
| `qmi_wlanfw_cal_temp_id_enum_v01` | `CalibrationTemperatureId`/checked raw wire value | **stub** |

## C structs

### Driver state and integration structs

| C struct | Intended Rust item | Status / disposition |
|---|---|---|
| `ath11k_qmi_driver_event` | private owned handshake event | **stub**; `list_head`, allocation, spinlock and workqueue are **replaced** by the eventual Rust state machine |
| `ath11k_qmi_ce_cfg` | borrowed/owned `FirmwareConfig` input containing target pipes, service map and shadow registers | **stub**; source data is supplied by core/CE rather than Linux pointers |
| `ath11k_qmi_event_msg` | none | **deferred**; unused in pinned `qmi.[ch]` |
| `target_mem_chunk` | `TargetMemoryChunk` using a platform DMA/MMIO token | **stub**; raw DMA addresses, `__iomem` pointers and allocation are **replaced** by the platform boundary |
| `target_info` | `TargetInfo`, with a selected subset returned as `FirmwareReady` | **stub** (`FirmwareReady.firmware_version` only) |
| `m3_mem_region` | `M3Region` using a platform DMA token | **deferred**; M3 firmware acquisition/allocation belongs to core/platform |
| `ath11k_qmi` | concrete handshake state machine | **stub**; qmi handle/socket/workqueue/list/locks are **replaced**, protocol state is still required |

### Wire structs

| C struct | Intended Rust wire item | Status / disposition |
|---|---|---|
| `qmi_wlanfw_host_cap_req_msg_v01` | `HostCapabilityRequest` | **stub** |
| `qmi_wlanfw_host_cap_resp_msg_v01` | `HostCapabilityResponse` | **stub** |
| `qmi_wlanfw_ind_register_req_msg_v01` | `IndicationRegisterRequest` | **stub** |
| `qmi_wlanfw_ind_register_resp_msg_v01` | `IndicationRegisterResponse` | **stub** |
| `qmi_wlanfw_mem_cfg_s_v01` | `MemoryConfig` | **stub** |
| `qmi_wlanfw_mem_seg_s_v01` | `MemorySegmentRequest` | **stub** |
| `qmi_wlanfw_request_mem_ind_msg_v01` | `RequestMemoryIndication` | **stub** |
| `qmi_wlanfw_mem_seg_resp_s_v01` | `MemorySegmentResponse` | **stub** |
| `qmi_wlanfw_respond_mem_req_msg_v01` | `RespondMemoryRequest` | **stub** |
| `qmi_wlanfw_respond_mem_resp_msg_v01` | `RespondMemoryResponse` | **stub** |
| `qmi_wlanfw_fw_mem_ready_ind_msg_v01` | `FirmwareMemoryReadyIndication` (empty payload) | **stub** |
| `qmi_wlanfw_fw_ready_ind_msg_v01` | `FirmwareReadyIndication` (empty payload) | **stub**; not equivalent to current summary `FirmwareReady` |
| `qmi_wlanfw_fw_cold_cal_done_ind_msg_v01` | `ColdCalibrationDoneIndication` (empty payload) | **stub** |
| `qmi_wlfw_fw_init_done_ind_msg_v01` | `FirmwareInitDoneIndication` (empty payload) | **stub** |
| `qmi_wlanfw_ce_tgt_pipe_cfg_s_v01` | `TargetPipeConfig` | **stub** |
| `qmi_wlanfw_ce_svc_pipe_cfg_s_v01` | `ServicePipeConfig` | **stub** |
| `qmi_wlanfw_shadow_reg_cfg_s_v01` | `ShadowRegisterConfig` | **stub**; source currently sends v1 as invalid |
| `qmi_wlanfw_shadow_reg_v2_cfg_s_v01` | `ShadowRegisterV2Config` | **stub** |
| `qmi_wlanfw_memory_region_info_s_v01` | `MemoryRegionInfo` | **deferred**; defined but unused and has no element-info table in pinned `qmi.c` |
| `qmi_wlanfw_rf_chip_info_s_v01` | `RfChipInfo` | **stub** |
| `qmi_wlanfw_rf_board_info_s_v01` | `RfBoardInfo` | **stub** |
| `qmi_wlanfw_soc_info_s_v01` | `SocInfo` | **stub** |
| `qmi_wlanfw_fw_version_info_s_v01` | `FirmwareVersionInfo` | **stub**; only the destination projection `FirmwareReady.firmware_version` exists |
| `qmi_wlanfw_cap_resp_msg_v01` | `CapabilityResponse` | **stub** |
| `qmi_wlanfw_cap_req_msg_v01` | `CapabilityRequest` (empty payload) | **stub** |
| `qmi_wlanfw_device_info_req_msg_v01` | `DeviceInfoRequest` (empty payload) | **stub**; hybrid-bus-only path |
| `qmi_wlanfw_device_info_resp_msg_v01` | `DeviceInfoResponse` | **stub**; BAR mapping is **replaced** by platform/core |
| `qmi_wlanfw_bdf_download_req_msg_v01` | `BdfDownloadRequest` | **stub** |
| `qmi_wlanfw_bdf_download_resp_msg_v01` | `BdfDownloadResponse` | **stub** |
| `qmi_wlanfw_m3_info_req_msg_v01` | `M3InfoRequest` | **stub** |
| `qmi_wlanfw_m3_info_resp_msg_v01` | `M3InfoResponse` | **stub** |
| `qmi_wlanfw_wlan_mode_req_msg_v01` | `WlanModeRequest` | **stub** |
| `qmi_wlanfw_wlan_mode_resp_msg_v01` | `WlanModeResponse` | **stub** |
| `qmi_wlanfw_wlan_cfg_req_msg_v01` | `WlanConfigRequest` | **stub** |
| `qmi_wlanfw_wlan_cfg_resp_msg_v01` | `WlanConfigResponse` | **stub** |
| `qmi_wlanfw_wlan_ini_req_msg_v01` | `WlanIniRequest` | **stub**; optional diagnostic-event path |
| `qmi_wlanfw_wlan_ini_resp_msg_v01` | `WlanIniResponse` | **stub** |

`struct ath11k_base` is only forward-declared here, not defined by these files;
its QMI-relevant inputs become arguments/configuration, while device lifecycle,
firmware lookup, MMIO, DMA, recovery and logging stay outside this crate.

## Static `qmi_elem_info` tables

Each table below is the pinned C schema for the corresponding typed TLV codec.
All are **stub**: `Request`/`Response` store bytes but do not implement any
table. Empty indication tables still matter because their message IDs must be
recognized and their payload must be validated as empty.

| C table | Intended Rust codec | Status |
|---|---|---|
| `qmi_wlanfw_host_cap_req_msg_v01_ei` | encode `HostCapabilityRequest` | **stub** |
| `qmi_wlanfw_host_cap_resp_msg_v01_ei` | decode `HostCapabilityResponse` | **stub** |
| `qmi_wlanfw_ind_register_req_msg_v01_ei` | encode `IndicationRegisterRequest` | **stub** |
| `qmi_wlanfw_ind_register_resp_msg_v01_ei` | decode `IndicationRegisterResponse` | **stub** |
| `qmi_wlanfw_mem_cfg_s_v01_ei` | nested `MemoryConfig` codec | **stub** |
| `qmi_wlanfw_mem_seg_s_v01_ei` | nested `MemorySegmentRequest` codec | **stub** |
| `qmi_wlanfw_request_mem_ind_msg_v01_ei` | decode `RequestMemoryIndication` | **stub** |
| `qmi_wlanfw_mem_seg_resp_s_v01_ei` | nested `MemorySegmentResponse` codec | **stub** |
| `qmi_wlanfw_respond_mem_req_msg_v01_ei` | encode `RespondMemoryRequest` | **stub** |
| `qmi_wlanfw_respond_mem_resp_msg_v01_ei` | decode `RespondMemoryResponse` | **stub** |
| `qmi_wlanfw_cap_req_msg_v01_ei` | encode empty `CapabilityRequest` | **stub** |
| `qmi_wlanfw_device_info_req_msg_v01_ei` | encode empty `DeviceInfoRequest` | **stub** |
| `qmi_wlfw_device_info_resp_msg_v01_ei` | decode `DeviceInfoResponse` (pinned spelling differs from its struct) | **stub** |
| `qmi_wlanfw_rf_chip_info_s_v01_ei` | nested `RfChipInfo` codec | **stub** |
| `qmi_wlanfw_rf_board_info_s_v01_ei` | nested `RfBoardInfo` codec | **stub** |
| `qmi_wlanfw_soc_info_s_v01_ei` | nested `SocInfo` codec | **stub** |
| `qmi_wlanfw_fw_version_info_s_v01_ei` | nested `FirmwareVersionInfo` codec | **stub** |
| `qmi_wlanfw_cap_resp_msg_v01_ei` | decode `CapabilityResponse` | **stub** |
| `qmi_wlanfw_bdf_download_req_msg_v01_ei` | encode `BdfDownloadRequest` | **stub** |
| `qmi_wlanfw_bdf_download_resp_msg_v01_ei` | decode `BdfDownloadResponse` | **stub** |
| `qmi_wlanfw_m3_info_req_msg_v01_ei` | encode `M3InfoRequest` | **stub** |
| `qmi_wlanfw_m3_info_resp_msg_v01_ei` | decode `M3InfoResponse` | **stub** |
| `qmi_wlanfw_ce_tgt_pipe_cfg_s_v01_ei` | nested `TargetPipeConfig` codec | **stub** |
| `qmi_wlanfw_ce_svc_pipe_cfg_s_v01_ei` | nested `ServicePipeConfig` codec | **stub** |
| `qmi_wlanfw_shadow_reg_cfg_s_v01_ei` | nested `ShadowRegisterConfig` codec | **stub** |
| `qmi_wlanfw_shadow_reg_v2_cfg_s_v01_ei` | nested `ShadowRegisterV2Config` codec | **stub** |
| `qmi_wlanfw_wlan_mode_req_msg_v01_ei` | encode `WlanModeRequest` | **stub** |
| `qmi_wlanfw_wlan_mode_resp_msg_v01_ei` | decode `WlanModeResponse` | **stub** |
| `qmi_wlanfw_wlan_cfg_req_msg_v01_ei` | encode `WlanConfigRequest` | **stub** |
| `qmi_wlanfw_wlan_cfg_resp_msg_v01_ei` | decode `WlanConfigResponse` | **stub** |
| `qmi_wlanfw_mem_ready_ind_msg_v01_ei` | decode empty `FirmwareMemoryReadyIndication` | **stub** |
| `qmi_wlanfw_fw_ready_ind_msg_v01_ei` | decode empty `FirmwareReadyIndication` | **stub** |
| `qmi_wlanfw_cold_boot_cal_done_ind_msg_v01_ei` | decode empty `ColdCalibrationDoneIndication` | **stub** |
| `qmi_wlanfw_wlan_ini_req_msg_v01_ei` | encode `WlanIniRequest` | **stub** |
| `qmi_wlanfw_wlan_ini_resp_msg_v01_ei` | decode `WlanIniResponse` | **stub** |
| `qmi_wlfw_fw_init_done_ind_msg_v01_ei` | decode empty `FirmwareInitDoneIndication` | **stub** |
The other static protocol dispatch objects are
`ath11k_qmi_msg_handlers` (five indication IDs/callbacks) and
`ath11k_qmi_ops` (server arrival/removal). Their behavior is **stub**; socket
registration itself is **replaced** by the caller-supplied transport.

## C functions

| C function | Intended Rust owner/item | Status / disposition |
|---|---|---|
| `ath11k_qmi_host_cap_send` | handshake host-capability transaction | **stub** |
| `ath11k_qmi_fw_ind_register_send` | handshake indication-registration transaction | **stub** |
| `ath11k_qmi_respond_fw_mem_request` | handshake memory-response transaction | **stub**; DMA addresses come from platform tokens |
| `ath11k_qmi_free_target_mem_chunk` | platform/core memory lifecycle | **replaced** |
| `ath11k_qmi_alloc_target_mem_chunk` | platform DMA allocation policy | **replaced**; retry/delayed-response semantics remain handshake inputs |
| `ath11k_qmi_assign_target_mem_chunk` | platform reserved-memory assignment | **replaced/deferred**; DT, `ioremap` and physical addresses cannot enter this crate |
| `ath11k_qmi_request_device_info` | handshake device-info transaction | **stub** for protocol; hybrid BAR validation/mapping is **deferred** to platform/core |
| `ath11k_qmi_request_target_cap` | handshake capability transaction and `TargetInfo` projection | **stub**; only destination field `FirmwareReady.firmware_version` exists |
| `ath11k_qmi_load_file_target_mem` | segmented BDF/caldata/EEPROM download transactions | **stub**; fixed-address copy is **deferred** to platform |
| `ath11k_qmi_load_bdf_qmi` | core firmware selection plus handshake BDF/regdb download | protocol send **stub**; file/board discovery **deferred** to core |
| `ath11k_qmi_m3_load` | core firmware acquisition plus platform DMA allocation | **deferred** |
| `ath11k_qmi_m3_free` | platform/core M3 memory lifecycle | **replaced/deferred** |
| `ath11k_qmi_wlanfw_m3_info_send` | handshake M3-info transaction | **stub** |
| `ath11k_qmi_wlanfw_mode_send` | handshake WLAN-mode transaction | **stub** |
| `ath11k_qmi_wlanfw_wlan_cfg_send` | handshake CE/service/shadow configuration transaction | **stub** |
| `ath11k_qmi_wlanfw_wlan_ini_send` | optional handshake diagnostic initialization transaction | **stub** |
| `ath11k_qmi_firmware_stop` | lifecycle stop using WLAN mode-off | **stub**; no Rust stop contract exists |
| `ath11k_qmi_firmware_start` | lifecycle start tail (optional INI, WLAN config, mode) | **stub**; intended beneath `Handshake::start`/core lifecycle |
| `ath11k_qmi_fwreset_from_cold_boot` | core/platform reset orchestration | **deferred**; QMI only reports calibration completion |
| `ath11k_qmi_process_coldboot_calibration` | handshake cold-boot mode and completion wait | **stub**; reset remains platform/core-owned |
| `ath11k_qmi_driver_event_post` | private handshake event enqueue | **stub**; Linux list/spinlock/workqueue mechanics **replaced** |
| `ath11k_qmi_event_mem_request` | handshake memory-request transition | **stub** |
| `ath11k_qmi_event_load_bdf` | handshake capability/device-info/BDF transition | **stub** |
| `ath11k_qmi_event_server_arrive` | handshake registration/host-cap transition | **stub**; discovery/connect supplied by transport |
| `ath11k_qmi_msg_mem_request_cb` | decode and handle request-memory indication | **stub**; allocation delegated to platform |
| `ath11k_qmi_msg_mem_ready_cb` | handle firmware-memory-ready indication | **stub** |
| `ath11k_qmi_msg_fw_ready_cb` | handle firmware-ready indication | **stub** |
| `ath11k_qmi_msg_cold_boot_cal_done_cb` | handle calibration-done indication | **stub** |
| `ath11k_qmi_msg_fw_init_done_cb` | handle firmware-init-done indication | **stub** |
| `ath11k_qmi_ops_new_server` | transport service-arrival notification | protocol transition **stub**; AF_QIPCRTR `kernel_connect` **replaced** |
| `ath11k_qmi_ops_del_server` | transport service-loss notification/recovery signal | protocol transition **stub**; Linux recovery flags **deferred** to core |
| `ath11k_qmi_driver_event_work` | ordered handshake state-machine dispatch | **stub**; Linux workqueue/flags/recovery calls **replaced/deferred** |
| `ath11k_qmi_init_service` | construct handshake and register WLFW service/indications | **stub**; `qmi_handle`, lookup and workqueue setup **replaced** |
| `ath11k_qmi_deinit_service` | cancel handshake and release protocol-owned resources | **stub**; socket/workqueue/DMA teardown **replaced** by owners |
| `ath11k_qmi_free_resource` | release target/M3 memory | **replaced/deferred** to platform/core |

The immediate blocker to implementing the mapped state machine is the current
one-way `Transport::transact` contract: the pinned flow depends on asynchronous
request-memory, memory-ready, firmware-ready, calibration-done, init-done, and
service-loss events. Typed codecs alone cannot close that gap.
