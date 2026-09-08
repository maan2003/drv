# ath11k-ce port map

Pinned source: Linux `509ce3d952d550f93b544c8d94c99e798f09a9b4`.
WCN6750 selects the QCA6390 CE tables in `core.c`.

| C file:symbol | Rust item | status | oracle artifact |
|---|---|---|---|
| `ce.c:ath11k_host_ce_config_qca6390` | `WCN6750_HOST_CE_CONFIG` | oracle-checked | source-derived table test |
| `hw.c:ath11k_target_ce_config_wlan_qca6390` | `WCN6750_TARGET_CE_CONFIG` | oracle-checked | source-derived table test |
| `hw.c:ath11k_target_service_to_ce_map_wlan_qca6390` | `WCN6750_SERVICE_TO_PIPE` | oracle-checked | source-derived table test |
| `ce.h:service_to_pipe` | `ServicePipeMap::to_le_bytes` | oracle-checked | byte fixture |
| `ce.h:ce_pipe_config` | `TargetPipeConfig::to_le_bytes` | oracle-checked | byte fixture |
| `hal.c:ath11k_hal_ce_src_set_desc` | HAL `descriptors::CeSourceDescriptor`, used by `CeTxBuffer::descriptor` | oracle-checked | descriptor + model-backend sync fixture |
| `hal.c:ath11k_hal_ce_dst_set_desc` | HAL `descriptors::CeDestinationDescriptor`, used by `CeRxBuffer::descriptor` | oracle-checked | descriptor fixture |
| `hal.c:ath11k_hal_ce_dst_status_get_length` | HAL `descriptors::CeDestinationStatusDescriptor::take_length` | oracle-checked | completion fixture |
| `ce.c:ath11k_ce_send` | `CePipes::send` | oracle-checked | full lifecycle ordering test |
| `ce.c:ath11k_ce_rx_buf_enqueue_pipe` | `CePipes::post_receive` | oracle-checked | full lifecycle ordering test |
| `ce.c:ath11k_ce_completed_recv_next` | `CePipes::completed_recv_next` | oracle-checked | payload + full-range CPU sync fixture |
| `ce.c:ath11k_ce_completed_send_next` | `CePipes::completed_send_next` | oracle-checked | source-reap fixture |
| `ce.c:ath11k_ce_alloc_pipes` | `CeAllocatedPipes::alloc_pipes` | oracle-checked | full WCN6750 lifecycle model test |
| `ce.c:ath11k_ce_init_pipes` | `CeAllocatedPipes::init_pipes` | oracle-checked | full WCN6750 lifecycle model test |
| `ce.c:ath11k_ce_free_pipes` | `CeAllocatedPipes::free_pipes`, `CePipes::free_pipes` | ported | generation-tied drop ownership |
| `ce.c:ath11k_ce_get_attr_flags` | `CePipes::get_attr_flags` | oracle-checked | CE4 fixture |
| `ce.c:ath11k_ce_per_engine_service` | `CePipes::per_engine_service` | ported | send/receive completion fixtures |
| `ce.c:ath11k_ce_rx_post_buf` | `CePipes::rx_post_buf` | ported | destination pool state exercised by post fixture |
| `htc.h:ath11k_htc_hdr` | `HtcHeader` | oracle-checked | byte fixture |
| `htc.h:ath11k_htc_ready` | `ReadyMessage` | oracle-checked | ready handshake test |
| `htc.h:ath11k_htc_ready_extended` | `ReadyExtendedMessage` | oracle-checked | byte fixture |
| `htc.h:ath11k_htc_conn_svc` | `ConnectServiceMessage` | oracle-checked | byte fixture |
| `htc.h:ath11k_htc_conn_svc_resp` | `ConnectServiceResponse` | oracle-checked | byte fixture |
| `htc.h:ath11k_htc_setup_complete_extended` | `setup_complete_message` | oracle-checked | byte fixture |
| `htc.h:ath11k_htc_record_hdr` / `ath11k_htc_credit_report` | `Htc::process_trailer` | oracle-checked | credit/malformed trailer tests |
| `htc.h:ath11k_htc_svc_id` | `ServiceId` constants | oracle-checked | service map fixture |
| `htc.c:ath11k_htc_init` | `Htc::new` | oracle-checked | endpoint lifecycle test |
| `htc.c:ath11k_htc_wait_target` | `Htc::wait_target` | oracle-checked | ready handshake test |
| `htc.c:ath11k_htc_connect_service` | `Htc::connect_request`, `Htc::connect_service` | oracle-checked | connect/credit test |
| `htc.c:ath11k_htc_start` | `Htc::start`, `setup_complete_message` | oracle-checked | lifecycle byte fixture |
| `htc.c:ath11k_htc_send` | `Htc::send` | oracle-checked | credit exhaustion test |
| `htc.c:ath11k_htc_send` + `hif.h:ath11k_hif_map_service_to_pipe` | `HtcTransport::send` | oracle-checked | WMI service framing fixture |
| `htc.c:ath11k_htc_process_credit_report` | `Htc::process_trailer` | oracle-checked | credit report test |
| `htc.c:ath11k_htc_process_trailer` | `Htc::process_trailer` | oracle-checked | malformed trailer tests |
| `htc.c:ath11k_htc_rx_completion_handler` | `Htc::receive` | oracle-checked | frame/trailer tests |
| CE/WMI/DP composition seam | `HtcServiceTransport`, `BoundService` | oracle-checked | service multiplex/demultiplex fixture |
| `htc.c:ath11k_htc_tx_completion_handler` | `Htc::tx_completion` | oracle-checked | lifecycle test |
| no pinned C symbol | `Htc::stop` | local lifecycle seam | endpoint reset test |
