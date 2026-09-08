# Confirmed C/Rust semantic differences

## Management TXD7 subtype remains Authentication

For every valid non-Authentication management subtype accepted by
`encode_client_management_tx`, TXD2 contains the caller's subtype but TXD7
retains subtype 11 (Authentication) from the auth-shaped encoder it reuses.
Pinned `mt76_connac2_mac_write_txwi_80211` assigns the actual frame subtype to
both TXD2 and TXD7. The differential test compares every other TXWI/TXP byte
and explicitly asserts this sole normalized difference. Authentication frames
(subtype 11) match without normalization.

## Negative half-dBm RSSI rounds toward zero

For odd RCPI values below 220, `parse_connac2_rx_frame` converts
`(rcpi - 220) / 2` after promoting RCPI to signed `i16`, so Rust division
rounds the negative half-dBm value toward zero. Pinned Linux's `to_rssi`
macro subtracts from the unsigned `FIELD_GET` result before division and its
subsequent `s8` conversion produces the floor instead. For example RCPI 101 is
-59 dBm in Rust and -60 dBm in Linux. The differential test separately asserts
both exact results; all RX group offsets, frame bytes, channel, and PN still
compare directly.
