# ath11k port interfaces and decomposition

The source oracle is Linux commit `509ce3d952d550f93b544c8d94c99e798f09a9b4`
from `sc7280-mainline/linux`, identical to the source of Redwood's running
7.2.0 kernel. `nix build .#ath11k-reference-source` materializes the read-only
subset at `reference/linux-<commit>/`.

## Decomposition

Counts are physical lines in the pinned `drivers/net/wireless/ath/ath11k`
C/header files. They size source ownership, not expected Rust output.

| crate / owner | pinned source | lines | responsibility and public boundary |
|---|---|---:|---|
| `ath11k-qmi` | `qmi.[ch]` | 3,895 | QMI TLVs and the WLAN firmware handshake over a caller-supplied `Transport`; yields typed `FirmwareReady`. AF_QIPCRTR socket code stays outside this protocol crate. |
| `ath11k-wmi` | `wmi.[ch]` | 16,769 | Checked TLV `Command`/`Event`, separately replaceable `CommandEncoder` and `EventDecoder`, and WMI service `Transport`. Split cmd encode and event decode between two engineers inside this crate because the shared IDs/TLV model must remain one API. |
| `ath11k-hal` | `hal.[ch]`, `hal_desc.h`, `hal_{rx,tx}.[ch]` | 7,295 | WCN6750 register maps, checked descriptors, SRNG creation/publication/consumption. It owns generation-tied `CoherentDma<B, Bidirectional>` ring memory from `drv-hardware`; packet buffers retain typed coherent/streaming directions and explicit sync. |
| `ath11k-ce` | `ce.[ch]`, then the HTC framing in `htc.[ch]` | 1,290 + 1,143 | Copy-engine rings, credits and typed service frames. It depends only on HAL; WMI and DP adapt this transport without CE depending upward on either protocol. |
| `ath11k-dp` | `dp.[ch]`, `dp_{rx,tx}.[ch]` | 10,141 | HTT control plus TCL TX, REO RX, WBM completions. Public seams are `HttControl` and `DataPath`; descriptor mechanics remain in HAL and HTC carriage remains in CE. |
| `ath11k-core` | `core.[ch]`, `hw.[ch]`, `ahb.[ch]`, `hif.h`, `peer.[ch]`, hardware-facing `mac.c` | 9,686 before `mac.c` | Composes lifecycle and owns pdev/vdev/peer state. `Lifecycle` consumes QMI'"'"'s `FirmwareReady`; `RadioControl` is the hardware-effects side of WlanSoftmac. |
| `ath11k-platform-backend` | AHB host-resource portion of `ahb.c` | included above | Portable bottom contract: re-exports `drv-hardware` generation-tied bounded MMIO, directional coherent/streaming DMA with explicit sync, interrupt, reset and teardown types. A future host adapter alone may contain unsafe/VFIO details. |
| replaced rather than ported | policy/callback portions of `mac.[ch]` | 11,062 total file size | Linux `ieee80211_ops`, cfg80211/mac80211 types, scan/association policy and management-frame policy are replaced by the existing Fuchsia MLME through WlanSoftmac. The WMI-emitting pdev/vdev/peer/key/channel operations are ported behind `RadioControl`. |

The complete directory is 82,878 lines. The table deliberately does not
pretend its selected port core is the whole directory: firmware/debugfs,
spectral, regulatory, thermal, WoW, PCI/MHI and other support files account for
the remainder and must be admitted only when a client milestone requires them.
HTT has no separate `dp_htt.c` in this pinned tree; its wire definitions and
implementation live in `dp.[ch]`.

## Dependency graph

```text
ath11k-core ──▶ ath11k-qmi
     │       ├▶ ath11k-wmi
     │       ├▶ ath11k-dp ──▶ ath11k-ce ──▶ ath11k-hal
     │       └▶ ath11k-hal ─────────────────────┘
     └────────────────────────▶ ath11k-platform-backend ──▶ drv-hardware
```

Protocol crates never depend on core. HAL depends directly on the shared `drv-hardware` model re-exported by the platform crate, while
the composition root supplies implementations; higher layers cannot reach raw
host resources. Every crate forbids unsafe code. When a concrete OS adapter is
added, unsafe is permitted only in that adapter, never in these protocol crates.

## Public API contract

- **platform:** shared `drv-hardware` `Device<B>`, bounded `MmioRegion`, generation-tied `CoherentDma`/`StreamingDma` with sealed directions and explicit streaming sync, `Interrupt`, reset and teardown. Never fds.
- **HAL:** `RingKind`, `RingMemory`, `RingId`, checked `Descriptor`, and
  `Rings`. Descriptor construction rejects a wrong layout length.
- **CE:** `ServiceId`, `TxFrame`/`RxFrame`, and `Transport`.
- **QMI:** `MessageId`-bearing bounded `Request`/`Response`, checked
  `RawIndication`, and an event-driven `Transport` that starts/stops WLFW
  service discovery and sends/receives transaction-correlated responses plus
  unsolicited indications against a monotonic timeout budget.
  `Wcn6750Handshake` exposes `init_service`/`deinit_service`,
  `process_next_event`, and `firmware_start`/`firmware_stop`; it returns typed
  `DriverEvent`/`FirmwareReady` outcomes. Caller-supplied `MemoryProvider` and
  `FirmwareAssets` traits perform DMA/MMIO and firmware acquisition without a
  dependency from QMI onto HAL, platform, or core.
- **WMI:** `CommandId`/`EventId`, word-aligned checked TLV envelopes,
  `CommandEncoder`, `EventDecoder`, and `Transport`.
- **DP/HTT:** `HttHostMessage`/`HttTargetMessage`, `HttControl`,
  `DataRings`, typed packets/peer IDs, and `DataPath`.
- **core:** `Lifecycle`, typed pdev/vdev IDs and `RadioControl`. The latter
  is intentionally narrower than mac80211 and implements WlanSoftmac effects.

Bodies are scaffolding, not a fabricated implementation. Porters may add
checked source-shaped types and methods but should propose changes before
weakening these dependency directions or exposing byte arrays as unchecked
descriptors/TLVs.

## Parallel split and oracle-checked completion

Use seven engineers: QMI; WMI command; WMI event; HAL; CE/HTC; DP/HTT; and
core/AHB/mac hardware effects. The platform adapter remains with the B3
feasibility owner until VFIO-platform semantics are measured.

Each protocol porter is done when every reachable native message in its
subsystem'"'"'s golden transcript round-trips byte-exactly (encode for commands,
decode/re-encode for events), all source enum/layout cases used by WCN6750 have
goldens, and malformed/truncated inputs fail without panic. HAL is done when
descriptor and register/ring configuration fixtures match pinned C byte for
byte; current tracepoints do not expose raw descriptors, so its oracle initially
comes from source-derived fixtures and later DMA snapshots. CE is done when HTC
headers, service connection, credit accounting and ring publications match
traces. DP is done when HTT messages match traces and TCL/REO/WBM fixtures match
DMA snapshots. Core is done when the ordered lifecycle plus WlanSoftmac effects
produce the same QMI/WMI/HTT calls and state transitions without importing
mac80211 policy.

Run any crate independently with `cargo test -p <crate>`.

Each crate maintains `PORT-MAP.md`: pinned C symbol, Rust item, status, and oracle artifact.
