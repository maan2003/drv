<!-- SPDX-License-Identifier: GPL-2.0-only -->

# MT7921 pinned-Linux source map

This manifest governs the source-corresponding Rust port of Linux mt76/MT7921.
The pinned source is Linux commit `e8efe09d4f378992c890d181d65e2ed8d8cb1194`.
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
| Linux policy | `mac80211.c`, cfg80211/mac80211 calls reached above | reference only | stubbed target | typed unsupported policy traits |

The production MLME/SME/RSN/EAPOL/frame path remains the pinned Fuchsia WLAN
port. Linux mac80211/cfg80211 is retained only to preserve hardware-driver call
graphs and must not become production policy.

## Current translated item map

| Linux item | Linux file | Rust item | Status | Verification |
|---|---|---|---|---|
| `mt792x_dma_prefetch` constants/order | `mt792x_dma.c` | constants intentionally not active | unmapped | physical trace proved zero is a valid pre-init state |
| `mt792x_dma_enable` DTX ordering subset | `mt792x_dma.c` | `prepare_global_tx_rings` | adapted | 43 unit tests; physical inventory only |
| `mt76_queue` TX descriptor fields | `mt76.h`, `dma.c` | `TxRingState`, `DmaDescriptor` | adapted, tested | unit fixtures/readback traces |
| `mt76_connac2_mcu_fill_message` download subset | `mt76_connac_mcu.c` | `encode_download_command` | adapted, tested | pinned-format fixtures |
| connac2 patch header/sections | `mt76_connac_mcu.h` | `Patch` parser | adapted, tested | malformed/bounded fixtures |
| `mt792x_wfsys_reset` | `mt7921/pci.c` | `wfsys_reset` | adapted, tested | ordering and timeout tests |
| driver ownership transitions | `mt7921/pci_mac.c`, connac registers | ownership state machines | adapted, tested | transition/error tests |
| PCI interrupt disable (`pci_intx(pdev, 0)`) | Linux PCI core call site | `disable_pci_intx` | adapted, tested | command-bit readback; physical run |
| kernel DMA allocation/mapping | mt76 DMA/core | `DmaArena` | adapted | iommufd pin/unmap tests; incomplete call graph |
| IRQ lifecycle/eventfd | mt76 PCI/IRQ paths | `IrqLifecycle`, `VfioIrq` | adapted, tested | state/UAPI tests; incomplete call graph |

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
4. mac80211/cfg80211 calls need typed unsupported stubs, while hardware-returned
   data must cross into the Fuchsia production policy boundary.
5. The last physical trace reached all 18 idle TX rings, then userspace received
   SIGSEGV on the first ownership-write path. Physical writes remain disabled
   until the complete dependency path and MMIO fault semantics are modeled.
