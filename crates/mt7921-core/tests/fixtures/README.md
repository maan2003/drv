# Rate-power request fixtures

`rate-tx-power-world-zero-{0..7}.bin` are the complete MCU request payloads
produced by Linux 6.18.40 `mt76_connac_mcu_rate_txpower_band()` for the pinned
MT7921 case:

- 2 GHz and 5 GHz enabled; 6 GHz disabled
- world alpha2 `00`
- final per-rate limit zero
- eight channels per batch

They exclude the 64-byte transport envelope, whose sequence is assigned at
submission time. Each fixture includes the 44-byte zero-initialized request
header and every channel's 161-entry SKU table. Linux sets the first four 5 GHz
SKU entries to `127`; all other SKU entries are zero. The expected lengths are
1340, 1016, then six times 1340 bytes.
