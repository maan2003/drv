# ath11k-dp port map

Pinned oracle: Linux `509ce3d952d550f93b544c8d94c99e798f09a9b4`.
`source-fixture` means the checked test vector is calculated directly from the
packed struct and `FIELD_*` definitions at that revision. Native descriptor
DMA snapshots are not yet available.

| C file:symbol | Rust item | status | oracle artifact |
|---|---|---|---|
| `dp.c:ath11k_dp_htt_connect` | `transport::HttTransport` | ported | HTC service `0x0300` unit coverage |
| `dp_tx.c:ath11k_dp_tx_htt_h2t_ver_req_msg` | `htt::request_target_version`, `version_request` | oracle-checked | `version_request_matches_htt_ver_req_cmd` |
| `dp_tx.c:ath11k_dp_tx_get_ring_id_type` | `htt::{SrngRingType,SrngRingId}` | oracle-checked | enum discriminants + SRNG fixtures |
| `dp_tx.c:ath11k_dp_tx_htt_srng_setup` | `htt::{SrngSetup,send_srng_setup}` | oracle-checked | `srng_setup_matches_dp_h_layout` |
| `dp_tx.c:ath11k_dp_tx_htt_rx_filter_setup` | `htt::{RxRingSelection,RxRingFilter,send_rx_ring_selection}` | oracle-checked | `rx_selection_matches_dp_h_layout` |
| `dp_rx.c:ath11k_dp_htt_htc_t2h_msg_handler` version branch | `htt::HttEvent::VersionConfirm` | oracle-checked | version event fixture + truncation sweep |
| `dp_rx.c:ath11k_dp_htt_htc_t2h_msg_handler` peer map/map2 | `htt::{HttEvent::PeerMap,PeerMap}` | oracle-checked | v1/v2 byte fixtures + truncation sweep |
| `dp_rx.c:ath11k_dp_htt_htc_t2h_msg_handler` peer unmap/unmap2 | `htt::HttEvent::PeerUnmap` | oracle-checked | packed-size decode coverage |
| `dp.h:htt_tx_wbm_completion` | `htt::TxCompletion` | oracle-checked | `tx_completion_overlay_is_checked` |
| `dp_tx.c:ath11k_dp_tx` DMA map | `dma::TxBuffer::map` | model-checked | recording backend asserts `SyncForDevice` |
| `dp_rx.c:ath11k_dp_rxbufs_replenish` DMA map | `dma::RxBuffer::replenish` | model-checked | directional model backend |
| `dp_rx.c:ath11k_dp_process_rx` DMA sync/unmap boundary | `dma::RxBuffer::sync_and_read` | model-checked | recording backend asserts `SyncForCpu` before read |
| `hw.c:wcn6750_ops` QCN9074 rx-desc selection | `rx::Wcn6750RxDescriptor` | oracle-checked | `qcn9074_layout_fixture_matches_wcn6750_ops` |
| `hw.c:ath11k_hw_qcn9074_rx_desc_get_*` | `rx::Wcn6750RxDescriptor::{status,address2,header_status,payload}` | oracle-checked | complete truncation sweep + field fixture |
| `dp.c:ath11k_dp_alloc` / `ath11k_dp_free` | `tx::ClientDataPath::{ath11k_dp_alloc,ath11k_dp_free}` | model-checked | TX/RX model lifecycle tests |
| `dp.c:ath11k_dp_pdev_pre_alloc` | `tx::ClientDataPath::ath11k_dp_pdev_pre_alloc` | ported | ID/cookie pools initialized at alloc |
| `dp.c:ath11k_dp_pdev_alloc` / `ath11k_dp_pdev_free` | `tx::ClientDataPath::{ath11k_dp_pdev_alloc,ath11k_dp_pdev_free}` | model-checked | directional RX mapping ownership |
| `dp.c:ath11k_dp_service_srng` | `tx::ClientDataPath::ath11k_dp_service_srng` | model-checked | bounded TX/WBM and REO service paths |
| `dp_tx.c:ath11k_dp_tx` TCL descriptor | `tx::ClientDataPath::transmit`, HAL `TclDataCommand::for_transmit` | model-checked | exact descriptor fields and sync-before-publish test |
| `dp_tx.c:ath11k_dp_tx_encap_nwifi` | `tx::encap_native_wifi` | oracle-checked | QoS control removal fixture |
| `dp_tx.c:ath11k_dp_tx_completion_handler` | `tx::ClientDataPath::service_tx_completions` | model-checked | WBM cookie/status fixture |
| `dp_rx.c:ath11k_dp_process_rx` direct REO processing | `tx::ClientDataPath::receive_with_status` | model-checked | checked REO cookie and RX descriptor path |
| `dp_rx.c:ath11k_dp_rxbufs_replenish` RXDMA descriptor | `tx::ClientDataPath::ath11k_dp_rxbufs_replenish` | model-checked | HAL `RxdmaBufferRing::for_buffer` + streaming DMA |
| `dp_rx.c:ath11k_dp_process_rx` MSDU-link processing | — | blocked | HAL parser exists; link-bank DMA lookup seam is not yet exposed |
| `dp_rx.c:ath11k_peer_rx_tid_setup` and REO command family | — | blocked | requested typed REO command setup from HAL owner |
| `dp_rx.c:ath11k_dp_rx_msdu_coalesce` | `tx::parse_received_chain` | model-checked | two-buffer boundary/L3-pad fixture |
| `dp_tx.c:ath11k_dp_tx_htt_h2t_ppdu_stats_req` | — | deferred | PPDU/pktlog telemetry, after client path |
| `dp_tx.c:ath11k_dp_tx_htt_h2t_ext_stats_req` | — | deferred | debugfs extended statistics |
| `dp_tx.c:ath11k_dp_tx_htt_monitor_mode_ring_config` | — | deferred | monitor mode |
| `dp_tx.c:ath11k_dp_tx_htt_rx_full_mon_setup` | — | deferred | full monitor mode |
| `dp_rx.c:ath11k_htt_pull_ppdu_stats` and helpers | — | deferred | PPDU telemetry |
| `dp_rx.c:ath11k_htt_pktlog` | — | deferred | pktlog telemetry |
| `dp_rx.c:ath11k_debugfs_htt_ext_stats_handler` | — | deferred | debugfs extended statistics |
| `dp_rx.c:ath11k_dp_rx_process_mon_rings` and monitor helpers | — | deferred | monitor mode |
