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

## Beacon loss is retained as raw input but never becomes connection loss

For `MCU_EVENT_BSS_BEACON_LOSS` (0x13), Linux reads the four-byte beacon-loss
body and reports connection loss only when its BSS index selects an active
station interface with beacon filtering enabled. The Rust core has no decoder
for that body. The active userspace path classifies 0x13 as unsolicited and
retains the complete packet, but no consumer removes or acts on that event.

**Likely Rust port bug:** a connected client can miss firmware beacon-loss
notification and therefore fail to initiate Linux-equivalent disconnect and
recovery behavior.

## Scheduled scan completion has no Rust event decoder

Linux retains both `MCU_EVENT_SCAN_DONE` (0x0d) and
`MCU_EVENT_SCHED_SCAN_DONE` (0x23) for scan work. The public Rust decoder
matches the retained ordinary-scan fields, including the seven-bit scan
sequence normalization, but rejects 0x23. The active receive classifier also
omits both scan IDs from its explicit Linux-derived unsolicited list; sequence
zero still reaches the raw unsolicited queue through a later fallback, but an
event carrying the currently awaited nonzero sequence can be mistaken for a
command response. Linux routes both IDs as unsolicited regardless of the
header sequence.

**Likely Rust port bugs:** scheduled-scan completion cannot be consumed, and
scan event routing is not equivalent for a colliding nonzero sequence.

## Coredump notification never triggers firmware reset in the Rust port

For `MCU_EVENT_COREDUMP` (0xf0), Linux immediately sets `fw_assert`, retains
the skb in the coredump queue, and schedules coredump work. Reset is deliberately
deferred: after the dump stream becomes inactive, `mt7921_coredump_work`
publishes the dump and calls `mt792x_reset`. The Rust active receive path only
retains the raw 0xf0 packet; it has no coredump consumer, firmware-assert state,
delayed dump collection, or reset notification/action.

**Likely Rust port bug:** a firmware assertion does not enter the Linux
coredump-and-reset recovery path.

## Ownership timeout returns earlier than pinned Linux

Pinned `____mt76_poll_msec` uses a `do ... while (timeout-- > 0)` loop. For a
50 ms timeout and 1 ms tick it performs 51 reads and, when all fail, 51 sleeps.
Both PCIe ownership functions repeat that shape ten times. The Rust ownership
model performs the same 10 writes and 510 reads, but checks its deadline before
sleeping after each attempt's terminal read, so it performs only 500 sleeps.
With the oracle's deterministic minimum sleep, Linux returns `-EIO` after
510 ms while Rust returns its timeout after 500 ms. The same difference occurs
before every later successful retry; successful first-attempt register traces
and all write/read sequences otherwise agree. ASPM's separate 2--3 ms driver
ownership delay is preserved by both implementations.

**Likely Rust port bug:** `DRIVER_OWN_ATTEMPT_MS` is implemented as a hard
elapsed deadline rather than reproducing the pinned poll helper's terminal
sleep. Host timeout and retry timing can therefore run up to 10 ms earlier than
Linux (apart from scheduler variance and the ASPM sleep range).

## RAM download command can disagree at the Connac2 patch address

Pinned `mt76_connac_mcu_init_download` chooses `PATCH_START_REQ` (CID `0x05`)
whenever a Connac2 download address is `0x00900000`, independent of which
firmware section led to the call. The public Rust
`DownloadCommand::TargetAddressLength` accepts that address but always emits
`TARGET_ADDRESS_LEN_REQ` (CID `0x01`). The remaining 12-byte request and
Connac2 envelope fields agree. `DownloadCommand::PatchStart` emits the pinned
CID for the ordinary patch path.

**Likely Rust port bug:** the command enum exposes a caller-selected wire CID
where pinned Linux derives it from device generation and address. A valid RAM
region at the reserved Connac2 patch address would be initialized with a
different command from Linux.

## MAC/WPDMA reset recovery is absent from the Rust core

Pinned PCIe `mt7921e_mac_reset` first acquires conn-on driver ownership, masks
host and PCI MAC interrupts, and performs forced WPDMA recovery. The WPDMA
path disables TX/RX DMA and pointer chaining, waits for both busy bits to
clear, bypasses DMASHDL, toggles the DMASHDL/logic reset bits, resets queues,
then restores the MT7921 prefetch table, all TX indices, global configuration,
TX/RX DMA, and interrupt state in that order. Only after those operations does
Linux acquire top driver ownership and reload firmware.

The public Rust core exposes WFSYS reset and disabled-state RX/TX ring
replacement separately, but no typed MAC reset or WPDMA disable/reset/restore
transaction. In particular, it cannot reproduce the ordered global-config,
DMASHDL, logic-reset, prefetch, DMA-enable, and interrupt transitions above.

**Likely Rust port bug:** firmware assertion or MAC recovery has no
Linux-equivalent core transaction, so a caller cannot safely recover WPDMA and
reload firmware without rebuilding source-sensitive ordering outside the
core.

## The reversible ownership helper has the wrong post-reset ownership order

For an initially firmware-owned device, Rust's only combined ownership helper,
`round_trip_driver_ownership`, emits conn-on `CLR_OWN` and later conn-on
`SET_OWN`. Pinned `mt7921e_mac_reset` instead emits conn-on driver-own before
WPDMA reset, restores DMA and interrupts, emits top driver-own, and reloads
firmware. It does not issue firmware-own during the reset transaction.

**Likely Rust port bug if used for recovery:** restoring firmware ownership
before firmware reload is not source-equivalent and can surrender the device
at the point reset recovery still requires driver ownership. The round-trip
helper remains correct for its documented reversible-probe purpose; a reset
owner must not substitute it for a dedicated recovery transaction.

## Block-ack session programming is absent from the Rust core

For each TX or RX BA enable/disable, pinned Linux sends two acknowledged UNI
`STA_REC_UPDATE` commands. The first carries `STA_REC_WTBL` with a nested
`WTBL_BA`; the second carries `STA_REC_BA`. Together they publish TID, role,
SSN, receive window, AMSDU policy, peer identity/reset selection, and the
per-TID enable bitmap. TX enable with AMSDU disabled also clears the WCID's
AMSDU capability. The oracle executes those assignments for valid AMPDU
parameters and retains both exact command bodies.

The public Rust core has no BA session command or AMPDU-action state model, so
there is no Rust event or byte stream to compare with either Linux command.

**Likely Rust port bug:** aggregation negotiation cannot install or remove the
firmware/WTBL BA state required by Linux, and TX AMSDU policy cannot follow the
negotiated AMPDU parameters.

## Deep-sleep and monitor/sniffer transitions are absent from the Rust core

Pinned Linux programs Connac2 deep sleep with the unacknowledged CE
`CHIP_CONFIG` strings `KeepFullPwr 0` and `KeepFullPwr 1`. On a monitor toggle
it then orders sniffer enable, runtime-PM suppression, deep-sleep suppression
and its MCU command; monitor enable additionally tears down the beacon filter.
The oracle executes the exact deep-sleep/sniffer request assignments and
normalizes that ordered state transition.

The public Rust core exposes neither command and has no monitor transition
that couples them to runtime power management and beacon filtering.

**Likely Rust port bug:** entering monitor mode cannot request firmware
sniffer delivery or prevent runtime/deep sleep from suppressing captures.

## Beacon-filter disable lacks Linux's BSS-abort command

Beacon-filter enable is byte-equivalent: Rust exposes the same acknowledged
`UNI_BSS_INFO_BCNFT` command followed by the same unacknowledged CE RX-filter
bitmap set. Its RX-filter clear command also matches Linux. But Linux disable
first sends an unacknowledged four-byte CE `SET_BSS_ABORT` request and only
then clears the beacon-drop bit. No public Rust encoder or higher-level core
operation emits that first command.

**Likely Rust port bug:** disabling beacon filtering can leave firmware BSS
power-management state active even after the receive filter is cleared.
