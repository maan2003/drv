# ath11k-hal port map

Maintain one row per pinned Linux symbol. Status is stub, ported,
oracle-checked, or hardware-checked.

| C file:symbol | Rust item | status | oracle artifact |
|---|---|---|---|
| — | — | stub | — |
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
