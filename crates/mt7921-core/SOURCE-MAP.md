<!-- SPDX-License-Identifier: GPL-2.0-only -->

# MT7921 pinned-Linux source map

This manifest governs the source-corresponding Rust port of Linux mt76/MT7921.
The pinned source authority is Linux tag `v7.1.5`. Its reached MT76/MT7921 files are byte-identical to the prior commit `e8efe09d4f378992c890d181d65e2ed8d8cb1194` snapshot.
Linux marks these files GPL-2.0-only; translations, transaction fixtures, and
compiled-C oracles derived from them must remain in an explicitly GPL-2.0-only
package. They must not be represented as independently MIT/Apache-2.0 code.

Status vocabulary:

- **exact**: source control flow and data representation are translated.
- **adapted**: semantics are preserved behind a documented userspace/typed boundary.
- **stubbed**: the symbol exists and returns an explicit unsupported result.
- **tested**: exact/adapted code has fixture, oracle, or differential coverage.
- **unmapped**: inventoried, but no Rust symbol exists yet.

No physical operation may be enabled unless its complete transitive row set is
exact/adapted, deterministically tested, and independently reviewed. Stubbed or
unmapped dependencies must fail closed.

## Scope and dependency boundary

| Linux surface | Files | LOC | Initial status | Rust boundary |
|---|---:|---:|---|---|
| MT7921 common | `mt7921/{init,mac,main,mcu,mcu.h,mt7921.h,regs.h}.c/.h` | 4,875 | unmapped/partial spike | GPL hardware package |
| MT7921 PCI | `mt7921/{pci,pci_mac,pci_mcu}.c` | 824 | partial adapted spike | typed PCI/VFIO traits |
| Other MT7921 transports/tools | `mt7921/{sdio,sdio_mac,sdio_mcu,usb,debugfs,testmode}.c` | 1,551 | stubbed target | explicit unsupported transport/tool traits |
| MT792x common | `mt792x_{core,dma,mac}.c`, `mt792x.h`, `mt792x_regs.h` | 2,801 | partial spike | GPL hardware package |
| MT792x platform/tools | `mt792x_{acpi_sar,debugfs,trace,usb}*` | 1,194 | stubbed target | platform/tool traits |
| Required mt76 core | `mt76.h`, `dma.[ch]`, `mcu.c`, `mmio.c`, `pci.c`, `tx.c`, `util.[ch]` | 5,135 | unmapped/partial spike | DMA/MMIO/clock/workqueue traits |
| Required connac | `mt76_connac.h`, `mt76_connac2_mac.h`, `mt76_connac_{mac,mcu}.c`, `mt76_connac_mcu.h` | 7,409 | partial spike | firmware protocol package |
| Linux policy | `mac80211.c`, cfg80211/mac80211 calls reached above | reference only | replaced by Fuchsia SoftMAC | never a Rust runtime/compatibility layer |

The production MLME/SME/RSN/EAPOL/frame path is the pinned BSD Fuchsia WLAN
common/client SoftMAC closure. Every Linux mac80211/cfg80211 dependency is
**replaced by Fuchsia SoftMAC**, not stubbed for later implementation and never
used as a runtime or compatibility layer. Linux policy references are retained
only to identify where hardware results cross the narrow Fuchsia-compatible
hardware trait.

## Current translated item map

[`SOURCE-ITEMS.tsv`](./SOURCE-ITEMS.tsv) is the exhaustive declaration-level
inventory for the scoped pinned files (functions/prototypes, types, members,
enumerators, macros/constants, and globals). Each of its 5,570 entries is
explicitly marked; the initial majority is `unmapped`.

| Linux item | Linux file | Rust item | Status | Verification |
|---|---|---|---|---|
| `mt792x_dma_prefetch` constants/order | `mt792x_dma.c` | constants intentionally not active | unmapped | physical trace proved zero is a valid pre-init state |
| `mt792x_dma_enable` ring/IRQ ordering subset | `mt792x_dma.c` | `prepare_global_{tx,rx}_rings`, active boot-ROM adapter | adapted | 44 unit tests; physical RX0 MSI/three-response trace |
| `mt76_queue` TX descriptor fields | `mt76.h`, `dma.c` | `TxRingState`, `DmaDescriptor` | adapted, tested | unit fixtures/readback traces |
| `mt76_connac2_mcu_fill_message` download subset | `mt76_connac_mcu.c` | `encode_download_command` | adapted, tested | pinned-format fixtures |
| `mt76_connac2_get_data_mode` | `mt76_connac_mcu.c` | `patch_download_mode` | adapted, tested | plain/AES/scramble/unsupported fixtures; unsupported encryption fails closed |
| `mt76_connac_mcu_gen_dl_mode` | `mt76_connac_mcu.h` | `firmware_download_mode` | adapted, tested | RAM feature-bit fixtures; not wired to active DMA |
| `mt76_connac_mcu_start_patch` request | `mt76_connac_mcu.c` | `DownloadCommand::PatchFinish` | adapted, tested | pinned-format fixture; not wired to active DMA |
| `mt76_connac_mcu_start_firmware` request | `mt76_connac_mcu.c` | `DownloadCommand::FirmwareStart` | adapted, tested | pinned-format fixture; not wired to active DMA |
| `mt792x_load_firmware`, connac2 patch/RAM loaders | `mt792x_core.c`, `mt76_connac_mcu.c` | `load_mt7921_firmware`, `FirmwareLoaderTransport`, `VfioFirmwareLoader` | MT7961 installed-artifact subset, tested | golden transaction trace, bounded polls/completions, per-operation failure injection, reset-while-pinned VFIO adapter |
| `mt7921_mcu_get_nic_capability` | `mt7921/mcu.c`, `mt76_connac_mcu.h` | `DownloadCommand::GetNicCapability`, `parse_nic_capability` | adapted, tested | source-exact command fixture; bounded known/unknown TLV fixtures; no radio mutation |
| `mt7921_mcu_read_eeprom`, pre-`SET_CLC` `mt7921_load_clc` | `mt7921/mcu.c`, `mcu.h`, `mt7921.h` | `DownloadCommand::ReadEepromBlock`, `parse_eeprom_block`, `discover_clc` | adapted, tested | exact EFUSE query fixture; bounded response/CLC fixtures; mutating CLC application excluded |
| `__mt7921_mcu_set_clc` request/response format | `mt7921/mcu.c`, `pci_mcu.c`, `pci.c`, `mt792x_acpi_sar.c`, `mt792x_acpi_sar.h`, `mcu.h`, `mt76_connac_mcu.h` | `world_clc_commands`, `ClcSetCommand::expects_response`, `encode_clc_set_command`, `parse_clc_set_response`, dual-ring loader transport | adapted, tested | installed-artifact rule fixture; `wait_resp` follows cap bit 0 and no-response success advances; TX ring 17/Q0 plus simultaneous WM ring 0/bit 0 and WM2 ring 4/bit 22 ownership, ack, drain, wrap, correlation, timeout and failure cleanup fixtures; physical pending |
| `mt76_channels_{2,5,6}ghz`, band gates | `mac80211.c`, `mt7921/mcu.c` | `candidate_channels`, `CandidateChannelSummary` | adapted, tested | exact physical channel universe fixture; explicitly not regulatory-valid |
| connac2 patch header/sections | `mt76_connac_mcu.h` | `Patch` parser | adapted, tested | malformed/bounded fixtures |
| probe identity reads (`MT_HW_CHIPID`, `MT_HW_BOUND`, `MT_HW_REV`) | `mt7921/pci.c`, `mt792x_regs.h` | `read_dynamic_identity_status`, narrowed SAE preflight | adapted, tested | source-order fixture; all three physically read under one restored selector |
| `mt792x_wfsys_reset` | `mt7921/pci.c` | `reset_wfsys`, dynamic-L1 adapter | adapted, tested | ordering/timeout tests; physical ready at 57 ms |
| driver ownership transitions | `mt7921/pci_mac.c`, connac registers | ownership state machines | adapted, tested | transition/error tests; physical first-attempt CLR_OWN response |
| PCI interrupt disable (`pci_intx(pdev, 0)`) | Linux PCI core call site | `disable_pci_intx` | adapted, tested | command-bit readback; physical run |
| kernel DMA allocation/mapping | mt76 DMA/core | `DmaArena` | adapted | iommufd pin/unmap tests; incomplete call graph |
| IRQ lifecycle/eventfd | mt76 PCI/IRQ paths | `IrqLifecycle`, `VfioIrq` | adapted, tested | state/UAPI tests; physical source-masked MSI RX0 trace |
| Connac2 PCI management TXWI/TXP and DMA publish shape | `mt7921/pci_mac.c::mt7921e_tx_prepare_skb`, `mt792x_core.c::mt792x_tx`, `mt7921/main.c`, `mt76_connac_mac.c::{mt76_connac2_mac_write_txwi,mt76_connac_write_hw_txp}`, `dma.c` | `encode_mt7921_5ghz_auth_tx`, `run_mt7921_privacy_safe_mgmt_matrix` | exact format subset, tested; physical blocked by `NO_IR` | Golden word-level short and 176-byte group-20 fixtures plus a non-authenticating A-E offline matrix; serialized unique token/PID correlation; DMASHDL-bypass pre/post gates; raw TX_FREE/TXS retention; mandatory frame wipe/reclaim; pre-association first-vif WCID 19; frame/IOVA/token/PID/20-entry-WTBL bounds |
| Management TX completion | `mt7921/mac.c::{mt7921_mac_tx_free,mt7921_mac_add_txs}`, `mt76_connac2_mac.h` | `parse_mt7921_{tx_free,tx_status}` | exact one-MSDU subset, tested; physical blocked by `NO_IR` | TXRX_NOTIFY packet type 6, RXD byte-count bounds, token/PID/WCID/ACK fixtures; paired/batched/out-of-WTBL completions fail closed |
| Connac2 rate/SAR power initialization | `mt7921/main.c::mt7921_set_tx_sar_pwr`, `mt76_connac_mcu.c::{mt76_connac_mcu_set_rate_txpower,mt76_connac_mcu_rate_txpower_band,mt76_connac_mcu_build_sku,mt76_connac_mcu_reg_rr}`, `mt7921/mcu.c::mt7921_mcu_parse_response`, `mt76_connac_mcu.h` | `encode_conservative_rate_tx_power_commands`, `encode_pse_reg_read_command`, `RateTxPowerAuthorizer` | adapted conservative uniform-rate subset, tested; temporary VFIO setup pending | Source channel lists/eight-channel batches/161-entry SKU and globally sequenced CE commands; each SET DMA consumption is followed by CE_QUERY(REG_READ) of `MT_PSE_BASE`, whose legacy response is `MCU_EVENT_REG_ACCESS` (`0x05`, distinct from `MCU_EVENT_ACCESS_REG`); world regulatory and SAR bounds plus a project-owned cap are mandatory, while CLC remains separate opaque firmware policy |
| Connac2 authentication RX metadata strip | `mt7921/mac.c::mt7921_mac_fill_rx`, `mt76_connac2_mac.h` | `parse_mt7921_auth_rx` | adapted, tested; SAE interpretation excluded | Raw SAE fields delivered unchanged after RX error/header-format validation |

This table is not yet exhaustive. The next inventory pass must list every
function, struct/union/enum, macro/constant, and global in every scoped file,
including an explicit `unmapped` row. Until then the source-complete dependency
gate is **closed**.

## Current blockers

1. The transitive mt76 call graph and exact LOC inventory are incomplete.
2. Existing spike code is MIT/Apache-2.0; source-corresponding GPL code needs a
   separately licensed crate before translation expands.
3. Linux kernel primitives (SKBs, NAPI, workqueues, timers, RCU, page pools,
   DMA APIs, PCI power/reset, firmware loading) need explicit safe typed traits.
4. The pinned Fuchsia common/client portable closure and upstream tests must be
   established first. Hardware-returned data then crosses only the narrow trait
   matching Fuchsia SoftMAC semantics; mac80211/cfg80211 is replaced, not ported.
5. The read-only WFDMA mapping that caused the first ring-ownership write to
   SIGSEGV is fixed and guarded by operation-permission and VFIO-region tests.
   A physical trace now owns and verifies all 18 inactive TX rings, resets while
   IOVAs remain pinned, and restores the kernel driver. Driver ownership also
   succeeds, but N9 is not ready. The bounded boot-ROM path now owns all eight
   RX slots, installs MSI/MSI-X before unmasking RX0, sends only NIC power and
   patch-semaphore commands, conditionally releases the semaphore, and disables
   both DMA directions before reset-while-pinned. The physical trace drained
   NIC-power event 3, received patch GET result 2, and received release result
   3 before a healthy kernel/iwd/network restore. Full firmware loading now
   reaches N9, switches normal responses to WM2 ring 4, and parses the read-only
   NIC capability response before reset-while-pinned and healthy restoration.
