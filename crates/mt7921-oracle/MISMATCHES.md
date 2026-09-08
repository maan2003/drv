# Confirmed C/Rust semantic differences

The pinned v7.1.5 source names the MT7921 entry point
`mt7921_mac_add_txs` (there is no `mt7921_mac_add_txs_info` symbol at this
tag). The oracle executes that body and its
`mt76_connac2_mac_add_txs_skb`/`mt76_connac2_mac_fill_txs` callees.

All cases below use a complete 8-dword TXS record in a valid 40-byte
`PKT_TYPE_TXS` envelope, an in-range MT7921 WCID, and a live matching status
skb. They are not malformed-input tests.

## Reserved packet IDs are reported by Rust but ignored by Linux

`mt7921_mac_add_txs` drops every TXS whose PID is below
`MT_PACKET_ID_FIRST` (3). `parse_mt7921_tx_status` accepts PIDs 0, 1, and 2
and returns them as completions.

**Likely Rust port bug:** the Rust parser omits Linux's PID ownership guard and
can expose a reserved/uncorrelatable packet ID as a management completion.

## Linux updates per-WCID rate state; Rust discards every rate field

On a reportable MPDU TXS, the extracted Linux path decodes TX rate mode, MCS,
NSS, STBC, bandwidth, HE GI/DCM, and legacy bitrate and stores the resulting
`rate_info` in the WCID. It also marks ACK/AMPDU status and sets the skb's
first status-rate index to -1. The Rust `Mt7921TxStatus` retains only WCID,
PID, and ACK state. Differential tests confirm those three values agree for
the shared reportable domain, while the Linux rate outputs have no Rust
counterpart.

**Likely Rust port bug:** status/rate reporting is incomplete. Any consumer
expecting Linux-equivalent station rate telemetry cannot obtain it from the
Rust completion path.
