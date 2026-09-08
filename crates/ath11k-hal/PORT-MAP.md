# ath11k-hal port map

Maintain one row per pinned Linux symbol. Status is stub, ported,
oracle-checked, or hardware-checked.

| C file:symbol | Rust item | status | oracle artifact |
|---|---|---|---|
| `hal_desc.h:ath11k_buffer_addr` / `hal_wbm_buffer_ring` | `descriptors::RxdmaBufferRing` | oracle-checked | `tests/descriptors.rs::tcl_data_command_is_byte_exact_and_checked` |
| `hal_desc.h:rx_mpdu_desc` | `descriptors::RxMpduDescriptor` | oracle-checked | `tests/descriptors.rs::reo_and_rxdma_masks_land_in_oracle_words` |
| `hal_desc.h:rx_msdu_desc` | `descriptors::RxMsduDescriptor` | oracle-checked | `tests/descriptors.rs::reo_and_rxdma_masks_land_in_oracle_words` |
| `hal_desc.h:hal_tcl_data_cmd` | `descriptors::TclDataCommand` | oracle-checked | `tests/descriptors.rs::tcl_data_command_is_byte_exact_and_checked` |
| `hal_desc.h:hal_reo_entrance_ring` | `descriptors::ReoEntranceRing` | oracle-checked | `tests/descriptors.rs::reo_and_rxdma_masks_land_in_oracle_words` |
| `hal_desc.h:hal_reo_dest_ring` | `descriptors::ReoDestinationRing` | oracle-checked | `tests/descriptors.rs::reo_and_rxdma_masks_land_in_oracle_words` |
| `hal_desc.h:hal_wbm_release_ring` | `descriptors::WbmReleaseRing` | oracle-checked | `tests/descriptors.rs::ce_and_wbm_layouts_are_little_endian` |
| `hal_desc.h:hal_ce_srng_src_desc` | `descriptors::CeSourceDescriptor` | oracle-checked | `tests/descriptors.rs::ce_and_wbm_layouts_are_little_endian` |
| `hal_desc.h:hal_ce_srng_dest_desc` | `descriptors::CeDestinationDescriptor` | oracle-checked | `tests/descriptors.rs::ce_and_wbm_layouts_are_little_endian` |
| `hal_desc.h:hal_ce_srng_dst_status_desc` | `descriptors::CeDestinationStatusDescriptor` | oracle-checked | `tests/descriptors.rs::ce_and_wbm_layouts_are_little_endian` |
| `hal_desc.h:hal_tlv_hdr` | `descriptors::RxMonitorTlvHeader` | oracle-checked | `tests/descriptors.rs::monitor_tlv_header_uses_linux_bit_positions` |
| `hal_rx.h:hal_rx_ppdu_start` | `descriptors::RxPpduStart` | oracle-checked | `tests/descriptors.rs::rx_end_family_offsets_match_oracle` |
| `hal_rx.h:hal_rx_mpdu_info_ipq8074` | `descriptors::RxMpduInfoWcn6750` | oracle-checked | `tests/descriptors.rs::rx_end_family_offsets_match_oracle`; selected by `hw.c:wcn6750_ops.mpdu_info_get_peerid` |
| `hal_rx.h:hal_rx_ppdu_end_duration` | `descriptors::RxPpduEndDuration` | oracle-checked | `tests/descriptors.rs::rx_end_family_offsets_match_oracle` |
| `hal_rx.h:hal_rx_ppdu_end_user_stats` | `descriptors::RxPpduEndUserStats` | oracle-checked | `tests/descriptors.rs::rx_end_family_offsets_match_oracle` |
| `hal_rx.h:hal_rx_ppdu_end_user_stats_ext` | `descriptors::RxPpduEndUserStatsExt` | oracle-checked | exact-length parser; the oracle declares no field masks |
| hal.c:`hw_srng_config_template` | `srng::config`, `RingType`, `Wcn6750Registers` | oracle-checked | source-derived table/unit tests |
| hw.c:`wcn6750_regs` | `Wcn6750Registers`, `srng::config` register bases/strides | oracle-checked | `wcn6750_table_matches_source` |
| hal.c:`ath11k_hal_srng_get_ring_id` | `Wcn6750Registers::ring_id` | oracle-checked | `wcn6750_table_matches_source` |
| hal.c:`ath11k_hal_srng_get_entrysize` | `Wcn6750Registers::entry_size` | oracle-checked | `wcn6750_table_matches_source` |
| hal.c:`ath11k_hal_srng_get_max_entries` | `Wcn6750Registers::max_entries` | oracle-checked | `wcn6750_table_matches_source` |
| hal.c:`ath11k_hal_srng_setup` | `Srng::setup` | oracle-checked | crate-local recording Backend sequence tests |
| hal.c:`ath11k_hal_srng_src_hw_init` | `Srng::program` source branch | oracle-checked | `source_setup_write_order_matches_hal_c` |
| hal.c:`ath11k_hal_srng_dst_hw_init` | `Srng::program` destination branch | ported | source-derived; destination sequence fixture pending |
| hal.c:`ath11k_hal_srng_src_get_next_entry` | `Srng::source_next` | oracle-checked | `ring_arithmetic_reserves_one_source_entry` |
| hal.c:`ath11k_hal_srng_dst_get_next_entry` | `Srng::destination_next` | ported | source-derived unit arithmetic |
| hal.c:`ath11k_hal_srng_{src,dst}_num_free` | `Srng::number_free` | ported | source-derived unit arithmetic |
| hal.c:`ath11k_hal_srng_{src,dst}_peek` | `Srng::peek` | ported | source-derived unit arithmetic |
| hal.c:`ath11k_hal_srng_access_begin` | `Srng::access_begin` | ported | `read_u32` acquire maps READ_ONCE + dma_rmb |
| hal.c:`ath11k_hal_srng_access_end` | `Srng::access_end` | ported | ordered `write_u32` release maps dma_wmb/mb + pointer write |
| hal.c:`ath11k_hal_ce_src_set_desc` | `descriptors::CeSourceDescriptor::for_transfer` | oracle-checked | `ce_and_wbm_layouts_are_little_endian` |
| hal.c:`ath11k_hal_ce_dst_set_desc` | `descriptors::CeDestinationDescriptor::from_address` | oracle-checked | checked 8-byte layout fixture |
| hal.c:`ath11k_hal_ce_dst_status_get_length` | `descriptors::CeDestinationStatusDescriptor::take_length` | oracle-checked | checked 16-byte layout fixture |
| hal_tx.c:`ath11k_hal_tx_cmd_desc_setup` | `descriptors::TclDataCommand::for_transmit`, `TxCommandInfo` | oracle-checked | `tcl_data_command_is_byte_exact_and_checked`; WCN6750 QCN9074 mesh bit |
| hal_tx.c:`ath11k_hal_tx_set_dscp_tid_map` | `descriptors::program_dscp_tid_map` | ported | source-derived bitstream/register sequence |
| hal_desc.h:`struct hal_wbm_release_ring` | `descriptors::WbmReleaseRing` | oracle-checked | `ce_and_wbm_layouts_are_little_endian` |
| hal.c:`ath11k_hal_srng_src_reap_next` | `Srng::source_reap_next` | oracle-checked | `ring_arithmetic_reserves_one_source_entry` |
| hal.c:`ath11k_hal_srng_src_get_next_reaped` | `Srng::source_next_reaped` | oracle-checked | `ring_arithmetic_reserves_one_source_entry` |
| hal.c:`ath11k_hal_srng_src_next_peek` | `Srng::source_next_peek` | ported | source-derived arithmetic |
| hal_rx.c:`ath11k_hal_rx_buf_addr_info_set/get` | `RxdmaBufferRing::for_buffer/info`, `BufferAddressInfo` | oracle-checked | `reo_and_rxdma_masks_land_in_oracle_words` |
| hal_rx.c:`ath11k_hal_rx_reo_ent_buf_paddr_get` | `ReoEntranceRing::received_buffer` | oracle-checked | REO/RXDMA oracle fixture |
| hal_rx.c:`ath11k_hal_rx_msdu_link_info_get` | `RxMsduLink::info`, `RxMsduLinkInfo` | oracle-checked | `msdu_link_info_stops_at_first_zero_low_address` |
| hal_desc.h:`struct hal_rx_msdu_details` | `RxMsduDetails` | oracle-checked | MSDU-link fixture |
| hal_desc.h:`struct hal_rx_msdu_link` | `RxMsduLink` | oracle-checked | 128-byte MSDU-link fixture |
