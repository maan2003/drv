# MT7921 C equivalence oracle

This GPL-2.0-only crate compares valid typed inputs accepted by `mt76-core` and
`mt7921-core` with the corresponding code extracted at build time from Linux
tag `v7.1.5` (commit `155b42bec9cbb6b8cdc47dd9bd09503a81fbe493`).
It needs no Wi-Fi hardware.

Build the immutable source with `nix build .#mt76-reference-source --out-link
result-mt76`, or set `MT76_REFERENCE_DIR` to its `reference/linux-v7.1.5`
directory, then run `cargo test -p mt7921-oracle`.

The harness feeds only values accepted by the Rust API. It currently executes
the pinned `mt76_connac2_mcu_fill_message`, `mt7921_mcu_parse_response`, and
`mt76_dma_add_buf` C bodies for legacy MCU command envelopes, MCU reply/event
headers and payload boundaries, patch reply scalars, EEPROM replies, and
one/two-segment DMA descriptors. It also differentially covers client data and
management TXWI/TXP encoding plus Connac2 normal/authentication RX descriptors.

The MAC TX/RX consumers are too coupled to mac80211, station/vif, PHY, and skb
state to extract usefully. Their oracle wrappers therefore use the pinned
macros and exact descriptor assignments with those inputs fixed to the public
MT7921 client seam. RX comparison is normalized to the group walk, payload
offset, channel, two-chain signal, and GROUP1 PN retained by
`parse_connac2_rx_frame`; unrelated Linux status/radiotap bookkeeping is not
modeled. TX comparison assumes the associated 802.3 path is WME and uses the
same WCID, queue, basic rate, and single-buffer TXP selected by the Rust API.
Any confirmed valid-domain difference is documented rather than copied into
Rust.
