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
one/two-segment DMA descriptors. It also executes the pinned
`mt76_dma_dequeue` and `mt76_dma_rx_cleanup` bodies on ordinary non-WED queue
states. Dequeue's done-gated tail/queued transition is compared with the public
`WfdmaRing::reclaim_one` model; RX cleanup is checked directly for forced
dequeue, buffer release, wraparound, and partial-frame cleanup. It also
differentially covers client data and
management TXWI/TXP encoding, Connac2 normal/authentication RX descriptors,
passive HW scan start/cancel, station BSS/initial STA_REC, and KEY_V2 install
and disable requests. The remaining connect checkpoint covers pre-key EAPOL
ordering through PTK/GTK STA_REC commands, the post-association interface
STA_REC update, and uniform conservative SET_RATE_TX_POWER batches. TX status
coverage executes the pinned `mt7921_mac_add_txs`,
`mt76_connac2_mac_add_txs_skb`, and `mt76_connac2_mac_fill_txs` bodies with a
minimal station/status-queue stub. It compares MPDU ACK/PID/WCID behavior and
exercises the rate mode, MCS, NSS/STBC, bandwidth, legacy, and HE fields read
by Linux.

Unsolicited MCU coverage applies the pinned
`mt7921_mcu_rx_event`/`mt7921_mcu_rx_unsolicited_event` dispatch assignments
to beacon loss, ordinary and scheduled scan completion, and coredump events,
plus the separate UNI unsolicited ROC path. The C side normalizes ownership
(free versus retain), connection-loss filtering, firmware-assert/reset timing,
and the event fields Linux consumes. Ordinary scan completion and ROC grant
fields are compared with the public Rust decoders; event types without a Rust
decoder are compared at the common MCU header/raw-body boundary and recorded
in `MISMATCHES.md` rather than papered over with a test-only decoder.

Most MAC TX/RX consumers are too coupled to mac80211, station/vif, PHY, and
skb state to extract usefully. Their oracle wrappers therefore use pinned
macros and exact descriptor assignments with those inputs fixed to the public
MT7921 client seam; TXS is the exception described above. RX comparison is
normalized to the group walk, payload
offset, channel, two-chain signal, and GROUP1 PN retained by
`parse_connac2_rx_frame`; unrelated Linux status/radiotap bookkeeping is not
modeled. TX comparison assumes the associated 802.3 path is WME and uses the
same WCID, queue, basic rate, and single-buffer TXP selected by the Rust API.
Any confirmed valid-domain difference is documented rather than copied into
Rust.

Channel-programming coverage executes the pinned
`mt7921_mcu_set_chan_info`, `mt76_connac_mcu_set_channel_domain`, and
`__mt7921_mcu_set_clc` bodies behind minimal MCU/skb shims. It compares the
complete legacy command envelopes emitted by the public Rust encoders for
RX-path setup, normal and off-channel switches (including 20/40/80/160 and
80+80 identities), the conservative world/indoor channel domain, and opaque
world CLC rules. The channel-domain and CLC wrappers use only ordered enabled
2/5 GHz records and valid public command fields; the shared capture shim is
serialized because the extracted kernel send boundary is process-global.

Firmware power and download coverage executes the pinned
`____mt76_poll_msec`, `__mt792xe_mcu_drv_pmctrl`,
`mt792xe_mcu_fw_pmctrl`, `mt76_connac_mcu_init_download`,
`mt76_connac_mcu_patch_sem_ctrl`, `mt76_connac_mcu_start_patch`, and
`mt76_connac_mcu_start_firmware` bodies. Minimal MMIO/time stubs normalize
ownership writes, status reads, sleeps, return status, and successful retry
position. The command wrappers run the pinned request assignments before the
existing extracted Connac2 envelope builder, so patch semaphore, patch/RAM
section initialization, patch finish, and firmware start are compared as
complete bytes rather than using a Rust-produced payload as C input. Two
valid-domain differences found by this checkpoint are recorded in
`MISMATCHES.md`.

Reset coverage pins the ordered register and branch operations in
`mt7921e_mac_reset` and its forced `mt792x_wpdma_reset` disable/restore path.
The C normalization varies valid pre-reset register contents and the successful
busy-poll position, and preserves the disable, DMASHDL/reset, prefetch, index,
global-enable, interrupt, and ownership order. Linux acquires conn-on driver
ownership before reset and top
driver ownership after DMA/interrupt restoration, then reloads firmware; it
does not restore firmware ownership in this transaction. The public Rust core
has safe pieces for WFSYS reset, disabled ring replacement, and a reversible
ownership probe, but no MAC/WPDMA recovery transaction to compare end-to-end.

`mt7921-core` does not currently expose an RX ring entry ownership or cleanup
API: its public RX surface prepares descriptor arrays, while `WfdmaRing` is a
TX ring model. Consequently the RX cleanup checkpoint executes and asserts the
pinned C ownership path but does not claim a Rust equivalence or add a test-only
RX ownership adapter.
