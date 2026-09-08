# ath11k-ce port map

<!-- PORT-MAP-SCHEMA: C symbol | C file:lines | Rust item | status ∈ {ported, ported-corrected, wcn6750-specific, local-seam, replaced-by-fuchsia-mlme, kernel-substrate, deferred, blocked} | note -->

Pinned source: Linux `509ce3d952d550f93b544c8d94c99e798f09a9b4`. WCN6750 selects the QCA6390 CE tables.

| C symbol | C file:lines | Rust item | status | note |
|---|---|---|---|---|
| `ath11k_host_ce_config_qca6390` | `ce.c:118-196` | `WCN6750_HOST_CE_CONFIG` | wcn6750-specific | Exact 9-entry fixture; shared with QCA6390 |
| `ath11k_target_ce_config_wlan_qca6390` | `hw.c:1623-1714` | `WCN6750_TARGET_CE_CONFIG` | wcn6750-specific | Exact 9-entry fixture selected by WCN6750 |
| `ath11k_target_service_to_ce_map_wlan_qca6390` | `hw.c:1720-1799` | `WCN6750_SERVICE_TO_PIPE` | wcn6750-specific | Exact 14-entry fixture selected by WCN6750 |
| `service_to_pipe` | `ce.h:72-76` | `ServicePipeMap` | ported | Little-endian byte fixture |
| `ce_pipe_config` | `ce.h:84-91` | `TargetPipeConfig` | ported | Little-endian byte fixture |
| `ath11k_hal_ce_src_set_desc` | `hal.c:572-586` | `CeTxBuffer::descriptor_before_sync` | ported | HAL checked descriptor; model sync fixture |
| `ath11k_hal_ce_dst_set_desc` | `hal.c:588-596` | `CeRxBuffer::descriptor` | ported | Allocation-derived DeviceAddress |
| `ath11k_hal_ce_dst_status_get_length` | `hal.c:598-607` | `CePipes::completed_recv_next` | ported | Clears length; full destination sync |
| `ath11k_ce_send` | `ce.c:709-797` | `CePipes::send` | ported | Descriptor < streaming sync < release HP proved |
| `ath11k_ce_rx_buf_enqueue_pipe` | `ce.c:272-319` | `CePipes::post_receive` | ported | Full lifecycle model fixture |
| `ath11k_ce_completed_recv_next` | `ce.c:371-415` | `CePipes::completed_recv_next` | ported | Remote-pointer acquire and payload fixture |
| `ath11k_ce_completed_send_next` | `ce.c:457-496` | `CePipes::completed_send_next` | ported | HAL source-reap fixture |
| `ath11k_ce_alloc_pipes` | `ce.c:1025-1050` | `CeAllocatedPipes::alloc_pipes` | ported | All nine WCN6750 pipes |
| `ath11k_ce_init_pipes` | `ce.c:914-970` | `CeAllocatedPipes::init_pipes` | ported | Full lifecycle model fixture |
| `ath11k_ce_free_pipes` | `ce.c:972-1022` | `CeAllocatedPipes::free_pipes, CePipes::free_pipes` | ported | Generation-tied DMA drop ownership |
| `ath11k_ce_get_attr_flags` | `ce.c:1072-1078` | `CePipes::get_attr_flags` | ported | CE4 fixture |
| `ath11k_ce_per_engine_service` | `ce.c:687-697` | `CePipes::per_engine_service` | ported | Send/receive completion paths |
| `ath11k_ce_rx_post_buf` | `ce.c:883-904` | `CePipes::rx_post_buf` | ported | Streaming destination pool |
| `ath11k_htc_hdr` | `htc.h:58-61` | `HtcHeader` | ported | Byte-exact fixture |
| `ath11k_htc_ready` | `htc.h:99-102` | `ReadyMessage` | ported | Ready handshake fixture |
| `ath11k_htc_ready_extended` | `htc.h:104-107` | `ReadyExtendedMessage` | ported | Extended-ready fixture |
| `ath11k_htc_conn_svc` | `htc.h:109-112` | `ConnectServiceMessage` | ported | Byte-exact request |
| `ath11k_htc_conn_svc_resp` | `htc.h:114-118` | `ConnectServiceResponse` | ported | Byte-exact response |
| `ath11k_htc_setup_complete_extended` | `htc.h:122-126` | `setup_complete_message` | ported | Global credit flag fixture |
| `ath11k_htc_record_hdr` | `htc.h:138-143` | `Htc::process_trailer` | ported | Malformed trailer tests |
| `ath11k_htc_credit_report` | `htc.h:145-150` | `Htc::process_trailer` | ported | Credit report fixture |
| `ath11k_htc_svc_id` | `htc.h:173-196` | `ServiceId` | ported | All source service IDs |
| `ath11k_htc_init` | `htc.c:798-845` | `Htc::new` | ported | Endpoint lifecycle |
| `ath11k_htc_wait_target` | `htc.c:524-594` | `Htc::wait_target` | ported | Ignored helper error and WCN credit quirk preserved |
| `ath11k_htc_connect_service` | `htc.c:596-764` | `Htc::connect_request, Htc::connect_service` | ported | Connect/credit state fixture |
| `ath11k_htc_start` | `htc.c:766-796` | `Htc::start` | ported | Setup-complete fixture |
| `ath11k_htc_send` | `htc.c:74-148` | `Htc::send` | ported | Credit exhaustion and rollback |
| `ath11k_htc_process_credit_report` | `htc.c:151-183` | `Htc::process_trailer` | ported | Credit report fixture |
| `ath11k_htc_process_trailer` | `htc.c:185-241` | `Htc::process_trailer` | ported | Malformed records fail without panic |
| `ath11k_htc_rx_completion_handler` | `htc.c:285-416` | `Htc::receive` | ported | Frame/trailer demultiplex |
| `ath11k_htc_tx_completion_handler` | `htc.c:255-278` | `Htc::tx_completion` | ported | Endpoint lookup |
| — | — | `Htc::stop` | local-seam | No pinned C symbol; core reset seam |
| — | — | `HtcPacketIo, HtcTransport` | local-seam | Raw CE/HTC composition and credit rollback |
| `ath11k_htc_rx_completion_handler` | `htc.c:285-416` | `HtcRouter, BoundService` | ported | Shared endpoint router mirrors eid callback dispatch |
| — | — | `HtcServiceTransport` | local-seam | Endpoint-bound WMI/HTT payload seam |
