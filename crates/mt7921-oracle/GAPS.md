# MT7921 oracle gap ledger

This is the consolidated follow-up index for valid-input differences proven by
the pinned Linux v7.1.5 differential oracle. [`MISMATCHES.md`](MISMATCHES.md)
owns the reproducer and behavioral detail for each row. Implementation remains
owned by `mt7921-core`; the oracle does not carry compatibility workarounds.

`Deferred post-Cut1` means the core owner confirmed that the deterministic
Cut1 baseline does not exercise the missing behavior. `Owner triage` means the
finding has been routed but its scheduling has not yet been classified.

| Gap | Pinned C evidence | Rust surface | Owner | Status |
|---|---|---|---|---|
| Reject reserved TXS packet IDs 0–2 | `mt7921/mac.c:446-462`; `mt76.h:485` | `parse_mt7921_tx_status` | `mt7921-core` / eng-0lja | Deferred post-Cut1; narrow parser fix |
| Preserve per-WCID TX rate telemetry | `mt76_connac_mac.c:615-738,740-770` | `Mt7921TxStatus` | `mt7921-core` / eng-0lja | Deferred post-Cut1; telemetry extension |
| Turn beacon loss into connection loss | `mt7921/mcu.c:299-348` | unsolicited MCU receive path | `mt7921-core` / eng-0lja | Deferred post-Cut1; recovery behavior |
| Decode and unconditionally route scheduled/ordinary scan completion | `mt7921/mcu.c:299-382`; `mt76_connac_mcu.h:1031-1043` | scan event decoder/classifier | `mt7921-core` / eng-0lja | Deferred post-Cut1; current adapter scan transcript remains exact |
| Coredump assertion, collection, and deferred reset | `mt7921/mcu.c:299-348`; `mt7921/mac.c:703+` | unsolicited MCU receive/recovery | `mt7921-core` / eng-0lja | Deferred post-Cut1 |
| Include terminal poll sleep in ownership timeout | `util.c:27+`; ownership callers in `mt7921/pci_mcu.c` | ownership timeout state machine | `mt7921-core` / eng-0lja | Deferred post-Cut1; 510 vs 500 minimum sleeps |
| Derive patch-start CID at address `0x00900000` | `mt76_connac_mcu.c:54-78` | `DownloadCommand::TargetAddressLength` | `mt7921-core` / eng-0lja | Deferred post-Cut1 |
| Add ordered MAC/WPDMA reset and reload transaction | `mt7921/pci_mac.c:56-104`; `mt792x_dma.c:213-241` | reset/ring/ownership APIs | `mt7921-core` / eng-0lja | Deferred post-Cut1; `round_trip_driver_ownership` is not a recovery substitute |
| Program TX/RX BA and negotiated AMPDU/AMSDU state | `mt7921/mcu.c:390-410`; `mt76_connac_mcu.c:1097-1140,1241-1322` | association/aggregation MCU state | `mt7921-core` / eng-0lja | Owner triage |
| Program deep-sleep and monitor/sniffer transitions | `mt7921/main.c:602-630`; `mt76_connac_mcu.c:1989-2000`; `mt7921/mcu.c:1131-1161` | power/monitor MCU state | `mt7921-core` / eng-0lja | Owner triage |
| Send `SET_BSS_ABORT` before beacon-filter clear | `mt7921/mcu.c:1035-1044`; `mt76_connac_mcu.h:1348` | beacon-filter disable | `mt7921-core` / eng-0lja | Owner triage |

Resolved differences are removed from this ledger once the oracle compares the
fixed Rust behavior directly with C and its `MISMATCHES.md` entry is deleted.
