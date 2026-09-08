# MT7921 C equivalence oracle

This GPL-2.0-only crate compares valid typed inputs accepted by `mt76-core` and
`mt7921-core` with the corresponding code extracted at build time from Linux
tag `v7.1.5` (commit `155b42bec9cbb6b8cdc47dd9bd09503a81fbe493`).
It needs no Wi-Fi hardware.

Build the immutable source with `nix build .#mt76-reference-source --out-link
result-mt76`, or set `MT76_REFERENCE_DIR` to its `reference/linux-v7.1.5`
directory, then run `cargo test -p mt7921-oracle`.

The harness feeds only values accepted by the Rust API. It currently executes
the pinned `mt76_connac2_mcu_fill_message` and `mt76_dma_add_buf` C bodies for
legacy MCU command envelopes and one/two-segment DMA descriptors. Any confirmed
reference defect is documented rather than copied into Rust.
