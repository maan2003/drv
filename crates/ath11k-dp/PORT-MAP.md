# ath11k-dp port map

<!-- PORT-MAP-SCHEMA: C symbol | C file:lines | Rust item | status ∈ {ported, ported-corrected, wcn6750-specific, local-seam, replaced-by-fuchsia-mlme, kernel-substrate, deferred, blocked} | note -->

Pinned oracle: Linux `509ce3d952d550f93b544c8d94c99e798f09a9b4`.

| C symbol | C file:lines | Rust item | status | note |
|---|---|---|---|---|
| `htt_ver_req_cmd` | `dp.h:333-337` | `version_request` | ported | Packed source fixture is byte-exact. |
| `htt_srng_setup_cmd` and HTT SRNG enums/masks | `dp.h:339-516` | `htt::SrngSetup`, `htt::SrngRingType`, `htt::SrngRingId`, `htt::SrngFlags` | ported | 52-byte packed fixture covers every WCN6750 client field. |
| `htt_rx_ring_selection_cfg_cmd` / `htt_rx_ring_tlv_filter` | `dp.h:668-994` | `htt::RxRingSelection`, `htt::RxRingFilter` | ported | 28-byte packed fixture. |
| `htt_t2h_version_conf_msg` | `dp.h:1018-1046` | `htt::HttEvent` | ported | Truncation checked; target major 3 enforced. |
| `htt_t2h_peer_map_event` | `dp.h:1048-1061` | `htt::PeerMap` | ported | V1 and V2 fixtures cover MAC, AST hash, and hardware peer ID. |
| `htt_t2h_peer_unmap_event` | `dp.h:1063-1074` | `htt::HttEvent` | ported | V1/V2 packed size checked. |
| `htt_tx_wbm_completion` | `dp.h:304-322` | `htt::TxCompletion` | ported | WBM offset-8 overlay fixture and truncation test. |
| `ath11k_dp_htt_connect` | `dp.c:942-967` | `ath11k_dp_htt_connect`, `transport::HttTransport` | ported | Binds the raw CE transport to `ServiceId::HTT_DATA_MSG`; wrong-service receive is rejected. |
| Shared HTC router HTT adapter | — | `ath11k_dp_htt_connect_service`, `transport::HtcHttTransport` | local-seam | Adapts the endpoint-bound `HtcServiceTransport` used to share one HTC router with WMI; truncated HTT headers are rejected. |
| `ath11k_dp_tx_htt_h2t_ver_req_msg` | `dp_tx.c:995-1034` | `request_target_version` | ported | Sends request, waits to deadline, and rejects incompatible major. |
| `ath11k_dp_tx_get_ring_id_type` | `dp_tx.c:810-875` | `htt::SrngRingType`, `htt::SrngRingId` | ported | Wire discriminants transcribed from `dp.h`. |
| `ath11k_dp_tx_htt_srng_setup` | `dp_tx.c:877-993` | `send_srng_setup` | ported | Source-derived byte fixture. |
| `ath11k_dp_tx_htt_rx_filter_setup` | `dp_tx.c:1071-1150` | `send_rx_ring_selection` | ported | Source-derived byte fixture. |
| `ath11k_dp_htt_htc_t2h_msg_handler` client branches | `dp_rx.c:1677-1749` | `HttTargetMessage::decode`, `htt::HttEvent` | ported | Version and peer map/unmap handled; malformed length sweeps do not panic. |
| `ath11k_dp_tx` | `dp_tx.c:83-310` | `tx::ClientDataPath::transmit`, `tx::client_tx_command_info` | ported-corrected | Generated C differentials cover field selection and the complete 32-byte TLV ring entry; recording backend proves streaming-DMA sync before publication. Correcting the ring entry from 28 to 32 bytes changes allocation/programming size, so the runner's DP-poll stage must be rebuilt even if it does not transmit. |
| `ath11k_dp_tx_encap_nwifi` | `dp_tx.c:30-46` | `encap_native_wifi` | ported | QoS control removal and subtype clearing preserved. |
| `ath11k_dp_tx_completion_handler` | `dp_tx.c:687-754` | `tx::ClientDataPath::service_tx_completions` | ported-corrected | TQM and firmware/HTT WBM completions release matching DMA ownership; unknown firmware statuses fail a matching live owner rather than stranding its DMA mapping. |
| `ath11k_dp_rxbufs_replenish` | `dp_rx.c:344-430` | `tx::ClientDataPath::ath11k_dp_rxbufs_replenish` | ported | 128-byte-aligned `StreamingDma<FromDevice>` plus typed RXDMA descriptors. |
| `ath11k_dp_process_rx` | `dp_rx.c:2650-2785` | `tx::ClientDataPath::receive_with_status` | ported | REO cookie lookup, routing-drop behavior, sync-for-CPU, and budget semantics. |
| `ath11k_dp_rx_process_msdu` | `dp_rx.c:2534-2610` | `parse_received_chain` | ported | Validates MSDU-done/length exactly before yielding payload and metadata. |
| `ath11k_dp_rx_msdu_coalesce` | `dp_rx.c:1753-1838` | `parse_received_chain` | ported | Multi-buffer continuation and first-buffer L3-pad boundary model fixture. |
| `wcn6750_ops` QCN9074 RX descriptor accessors | `hw.c:1103-1137` | `rx::Wcn6750RxDescriptor` | wcn6750-specific | Exact 388-byte QCN9074 layout selected by WCN6750; every truncation rejected. |
| `ath11k_peer_rx_tid_setup` descriptor/DMA portion | `dp_rx.c:997-1083` | `reo::ReoTid::setup` | ported | Generated C differential covers QoS/non-QoS size, every valid TID/PN type, BA-window normalization, start sequence, and all extension headers; non-coherent REO qdesc is explicitly synced for device after initialization. |
| `ath11k_peer_rx_tid_setup` / `ath11k_peer_rx_tid_reo_update` | `dp_rx.c:929-1083` | `reo::PeerRxTids::ath11k_peer_rx_tid_setup` | ported | DMA-pool ownership is published only after typed reorder-queue WMI setup succeeds; active queues use REO update. |
| `ath11k_peer_rx_tid_delete` / `ath11k_dp_rx_tid_del_func` / `ath11k_dp_reo_cache_flush` | `dp_rx.c:708-844` | `reo::PeerRxTids` delete/status/aged-flush methods | ported-corrected | Global status dispatch gates deferred extension/base cache flush; invalidation publication or execution failures and uncertain WMI setup quarantine ownership until reset. |
| `ath11k_peer_rx_tid_cleanup` / `ath11k_peer_frags_flush` | `dp_rx.c:878-931` | `reo::PeerRxTids::ath11k_peer_rx_tid_cleanup`, `ath11k_peer_frags_flush` | ported-corrected | Global keyed teardown blocks setup, invalidates TIDs 0..16, purges fragment state, and retains uncertain owners until a proven reset boundary; the unused WMI reorder-remove command is not invented here. |
| `ath11k_dp_rx_ampdu_start` / `ath11k_dp_rx_ampdu_stop` | `dp_rx.c:1085-1156` | `reo::PeerRxTids::ath11k_dp_rx_ampdu_start`, `ath11k_dp_rx_ampdu_stop` | ported | Peer-gated start delegates negotiated BA/SSN setup; stop updates only BA to one before typed WMI publication. |
| `ath11k_dp_rx_h_cmp_frags` / `ath11k_dp_rx_h_sort_frags` / `ath11k_dp_rx_h_defrag_validate_incr_pn` / `ath11k_dp_rx_frag_h_mpdu` validation/accumulation | `dp_rx.c:3521-3599,3601-3729` | `reo::PeerRxTids::ath11k_dp_rx_frag_h_mpdu` | ported | Keyed validation, sorted bitmap accumulation, first/subsequent move-only link disposition, ingress deadline enforcement, and CCMP/GCMP PN continuity yield a typed completed chain. |
| `ath11k_dp_rx_frag_timer` | `dp_rx.c:3196-3209` | `reo::PeerRxTids::expire_incomplete_fragments` | local-seam | Explicit polling expires idle chains and transfers their retained link owners; no production scheduler is wired yet. |
| `ath11k_dp_rx_h_verify_tkip_mic` / `ath11k_dp_rx_h_defrag` / `ath11k_dp_rx_h_defrag_reo_reinject` | `dp_rx.c:3245-3519` | `reo::CompletedFragmentChain` | deferred | Completed chains retain the first link descriptor plus each fragment's exact encryption/decrypted state; link-bank DMA, WBM return, buffer-cookie ownership, normalization/MMIC, and REO reinject publication require the stage-2 production substrate. |
| `ath11k_dp_tx_send_reo_cmd` | `dp_tx.c:756-808` | `reo::ReoController::ath11k_dp_tx_send_reo_cmd` | ported | Generated C differentials cover every field and resource outcome of source-supported QueueStats, FlushCache, and UpdateRxQueue commands. |
| `ath11k_dp_process_reo_status` | `dp_rx.c:4339-4411` | `reo::ReoController::ath11k_dp_process_reo_status` | ported | Generated C differential covers every status tag and uniform-header field; unknown tags and malformed descriptors fail. |
| `ath11k_dp_pdev_reo_setup` | `dp_rx.c:546-569` | `reo::ReoController::ath11k_dp_pdev_reo_setup` | wcn6750-specific | Calls HAL WCN6750 REO MMIO setup (generated C differential covers the ordered register sequence) and owns command/status rings. |
| `ath11k_dp_pdev_reo_cleanup` | `dp_rx.c:537-544` | `reo::ReoController::ath11k_dp_pdev_reo_cleanup` | ported | Typed REO controller teardown. |
| `ath11k_dp_alloc` | `dp.c:1048-1119` | `tx::ClientDataPath::ath11k_dp_alloc` | ported | DMA packet pools and HAL ring adapter allocation. |
| `ath11k_dp_free` | `dp.c:1023-1046` | `tx::ClientDataPath::ath11k_dp_free` | ported | Pending streaming mappings are released by ownership drop. |
| `ath11k_dp_pdev_pre_alloc` | `dp.c:887-909` | `tx::ClientDataPath::ath11k_dp_pdev_pre_alloc` | ported | ID/cookie pools are initialized by allocation. |
| `ath11k_dp_pdev_alloc` | `dp.c:911-940` | `tx::ClientDataPath::ath11k_dp_pdev_alloc` | ported | Client RXDMA allocation; monitor attach is deferred separately. |
| `ath11k_dp_pdev_free` | `dp.c:872-885` | `tx::ClientDataPath::ath11k_dp_pdev_free` | ported | Releases client RXDMA ownership; monitor detach is deferred. |
| `ath11k_dp_service_srng` client rings | `dp.c:770-866` | `tx::ClientDataPath::ath11k_dp_service_srng` | ported | Bounded TCL/WBM and REO/RXDMA interrupt service. |
| `DataPath::transmit` MLME frame seam | `dp_tx.c:83-310` | `DataPath::transmit` | local-seam | Vec copy is the intentional safe MLME boundary; internal buffers retain DMA ownership. QoS-control TID is the host input contract corresponding to mac80211's skb priority. |
| `DataPath::receive` MLME frame seam | `dp_rx.c:2534-2648` | `DataPath::receive` | local-seam | Vec copy is the intentional safe MLME boundary; internal buffers retain DMA ownership. |
| Native HTT golden artifact verifier | `trace.h:36-121` | `parse_jsonl`, `verify_trace`, `golden::GoldenRecord` | local-seam | Parses `artifacts/redwood-native-ath11k/htt/ordered.jsonl`, validates dynamic-array lengths/lower hex, reports exact/unmapped/first differing offset/decode failure, and runs RX descriptors through the real parser. |
| Link-descriptor error paths | `dp_rx.c:3380-3480` | — | deferred | HAL codecs exist in `ath11k-hal`; common STA REO destination path receives direct MSDU buffers. |
| Link-descriptor release paths | `dp_rx.c:3780-3885` | — | deferred | HAL codecs exist in `ath11k-hal`; common STA REO destination path receives direct MSDU buffers. |
| `ath11k_dp_tx_htt_h2t_ppdu_stats_req` | `dp_tx.c:1036-1069` | — | deferred | PPDU telemetry is not required for client bring-up. |
| `ath11k_dp_tx_htt_h2t_ext_stats_req` | `dp_tx.c:1152-1185` | — | deferred | Debugfs extended statistics. |
| `ath11k_dp_tx_htt_monitor_mode_ring_config` | `dp_tx.c:1187-1267` | — | deferred | Monitor mode. |
| `ath11k_dp_tx_htt_rx_full_mon_setup` | `dp_tx.c:1269-1305` | — | deferred | Full monitor mode. |
| `ath11k_htt_pull_ppdu_stats` and helpers | `dp_rx.c:1224-1602` | — | deferred | PPDU telemetry; native golden capture harness is prepared separately. |
| `ath11k_htt_pktlog` | `dp_rx.c:1604-1624` | — | deferred | Pktlog telemetry; native golden capture harness is prepared separately. |
| `ath11k_debugfs_htt_ext_stats_handler` | `dp_rx.c:1736-1738` | — | deferred | Debugfs extended statistics. |
| RX monitor/pktlog ring processing | `dp_rx.c:4350-5804` | — | deferred | Monitor/AP diagnostics come after the STA client milestone. |
