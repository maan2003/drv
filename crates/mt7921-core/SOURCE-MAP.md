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
| `mt792x_load_firmware`, connac2 patch/RAM loaders | `mt792x_core.c`, `mt76_connac_mcu.c` | `load_mt7921_firmware`, `FirmwareLoaderTransport`, `VfioFirmwareLoader`; device-neutral completion state in `driver-runtime` | MT7961 installed-artifact subset, tested | golden transaction trace, bounded polls/completions, per-operation failure injection, reset-while-pinned VFIO adapter |
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
| IRQ lifecycle/eventfd | mt76 PCI/IRQ paths | `driver_runtime::IrqLifecycle`, `VfioIrq` | adapted, tested | generic state and device-specific UAPI tests; physical source-masked MSI RX0 trace |
| Connac2 PCI management/data TXWI/TXP and DMA publish shape | `net/mac80211/mlme.c::{ieee80211_nullfunc_get,ieee80211_send_nullfunc}`, `net/mac80211/tx.c::{ieee80211_tx_control_port,ieee80211_tx_h_check_control_port_protocol,ieee80211_tx_h_sequence}`, `mt7921/pci_mac.c::mt7921e_tx_prepare_skb`, `mt792x_core.c::mt792x_tx`, `mt7921/main.c`, `mt76_connac_mac.c::{mt76_connac2_mac_write_txwi,mt76_connac2_mac_write_txwi_80211,mt76_connac_write_hw_txp}`, `dma.c` | `encode_mt7921_5ghz_auth_tx`, `linux_qos_null_probe_reference{,_for_tid}`, `linux_qos_eapol_control_port_reference`, `run_mt7921_privacy_safe_mgmt_matrix` | exact format subsets, tested | Independent Linux v7.1 awake QoS-null TID0/BE/qidx1 and TID7/VO/qidx3 plus QoS control-port transcripts cover associated WCID 7 and every TXD/TXP/MPDU byte. Ordinary nullfunc traffic uses normal hardware rate control; only connection-poll nullfunc sets USE_MINRATE, while control-port EAPOL retains fixed OFDM6. A numeric all-DW audit proves WLAN_IDX is entirely DW1[9:0] through WCID 1023, while PID is DW5 and frame type/subtype are DW7. Linux reverse LMAC mapping proves BE is qidx1 (not qidx0, which is BK). Full post-ASSOC call-graph audit proves there is no separate RA command: mac80211 rate-init only initializes NSS because mt7921 has no link_sta_rc_update callback; peer STA_REC already contains PHY/RA. It also proves BSS_CHANGED_ASSOC emits a second mandatory STA_REC for reserved interface WCID19 after EDCA; data readiness is now blocked until that exact GENERIC/RX/HDR_TRANS reset-and-set is ACKed. Beacon-filter BCNFT/RXFILTER are later RX-power optimizations, and awake cfg.ps=false emits no peer TX-PS mutation. Physical discriminators retain unique token/PID, raw completion evidence, serialized correlation, mandatory wipe/reclaim and bounds. |
| JOIN remain-on-channel lifecycle | `net/mac80211/mlme.c::{ieee80211_auth,ieee80211_send_assoc,ieee80211_rx_mgmt_auth,ieee80211_rx_mgmt_assoc}`, `mt7921/main.c::{mt7921_mgd_prepare_tx,mt7921_mgd_complete_tx,mt7921_set_roc,mt7921_abort_roc}`, `mt7921/mcu.c::{mt7921_mcu_set_roc,mt7921_mcu_abort_roc,mt7921_mcu_uni_roc_event}` | `encode_client_join_roc_{acquire,abort}`, `parse_client_join_roc_grant` | exact JOIN subset, tested | Linux 6.18.40 and 7.1.5 have byte-identical ownership and payload code. UNI CID 0x27 acquisition uses BSS0, incrementing nonzero token, selected channel/band, 20 MHz command bandwidth, JOIN request type and Linux durations (SAE 2000 ms; association fallback 1000 ms). TX waits up to one second for the unsolicited tag-validated grant, matching Linux which does not require echoed request fields. SAE retains one transaction through commit/confirm and aborts on peer confirm. A successful association acquires a second transaction and reaches `mgd_complete_tx` only after synchronous BSS/STA_REC/WCID19/beacon-tail programming; a non-success association response, including status-30 comeback, reaches the same completion callback immediately and the retry acquires a fresh transaction. `ClientChannelLease` remains host evidence only and is not a firmware ROC equivalent. Firmware source does not establish ROC as a post-association unicast-data gate. |
| Management TX completion | `mt7921/mac.c::{mt7921_mac_tx_free,mt7921_mac_add_txs}`, `mt76_connac2_mac.h` | `parse_mt7921_{tx_free,tx_status}` | exact one-MSDU subset, tested; physical blocked by `NO_IR` | TXRX_NOTIFY packet type 6, RXD byte-count bounds, token/PID/WCID/ACK fixtures; paired/batched/out-of-WTBL completions fail closed |
| Associated STA_REC/WTBL context and queue mapping | `mt7921/mcu.c::mt7921_mcu_sta_update`, `mt76_connac_mcu.c::{mt76_connac_mcu_sta_basic_tlv,mt76_connac_mcu_sta_tlv,mt76_connac_mcu_sta_amsdu_tlv,mt76_connac_mcu_wtbl_generic_tlv,mt76_connac_mcu_wtbl_ht_tlv,mt76_connac_mcu_sta_cmd}`, `mt76_connac.h::mt76_connac_lmac_mapping` | `encode_legacy_wme_add_wcid_command`, `linux_legacy_rate_context_reference` | exact negotiated HT/VHT subset, tested | BSS/OMAC/WCID/AID/STATE_ASSOC, negotiated HT/VHT/AMSDU/PHY/RA fields and nested WTBL GENERIC/RX/HDR_TRANS/HT/VHT/SMPS are source-decoded. Linux LMAC address mapping independently places WCID7 at 0x820d8700 and its admission counters at DW20/0x820d8750; completion correlation uses ten-bit WCID plus unique token/PID. UAPSD is correctly absent for station mode; BA is added only by later aggregation setup; HE remains explicitly absent because the pinned association API does not expose negotiated HE. Linux reverse AC mapping confirms mac80211 VO 0 maps to LMAC qidx 3; `no_rx_trans=true` is RX decapsulation policy, not TX disable. |
| Initial and associated BSS BASIC/QBSS context | `mt76_connac_mcu.h::mt76_connac_bss_{basic,qos}_tlv`, `mt76_connac_mcu.c::{mt76_connac_mcu_uni_add_dev,mt76_connac_mcu_uni_add_bss,mt76_connac_mcu_uni_set_chctx,mt76_connac_get_phy_mode,mt76_connac_get_phy_mode_v2}` | `encode_client_bss_basic_payload`, `encode_client_interface_commands`, `encode_client_bss_command`, `encode_client_post_assoc_rlm_command` | exact MT7921 infrastructure-client subset, tested | Both paths share Linux's packed 4-byte request header plus exactly 32-byte BASIC TLV, ending in explicit `phymode_ext`/`link_idx` bytes with no host-layout padding. Associated QBSS begins at payload offset 36, is exactly 8 bytes, and yields the native 44-byte payload/92-byte MCU command. Full initial and associated native command goldens reject the former stale BASIC length 36/total payload 48. The initial record keeps BSSID/BMC/STA/beacon/DTIM/PHY zero; the associated record uses BMC/STA WCID 19 and the validated selected-beacon DTIM retained by `JoinedClientBss`, also shared by the later BCNFT command. Active BSS index 0/OMAC 0/band 0/WMM 0, infrastructure-station connection type/state, BSSID, beacon context, local A+HT+VHT+HE PHY mode, local OFDM+HT+VHT+HE non-HT-basic PHY bitmap and QBSS state are emitted together before STA association. E2E80 corrects the prior `0x0001` placeholder to Linux's local 5-GHz `0x0078`. The corrected native timeline proves the next 20-byte CID2 RLM is synchronously ACKed after associated BASIC/QBSS and before M1/peer STA_REC; userspace now preserves that exact placement before its bounded RX pump. |
| Connac2 rate/SAR power initialization | `mt7921/main.c::{__mt7921_start,mt7921_set_tx_sar_pwr}`, `mt76_connac_mcu.c::{mt76_connac_mcu_set_rate_txpower,mt76_connac_mcu_rate_txpower_band,mt76_connac_mcu_build_sku}`, `mt76_connac_mcu.h` | `RegulatoryRatePowerSnapshot`, `encode_regulatory_rate_tx_power_commands`, `RateTxPowerAuthorizer`, `regulatory_rate_power_snapshot_from_fuchsia` | generic no-OF-override subset, tested; physical publication blocked on unavailable Fuchsia max-power input | One immutable generation contains every emitted channel's band/number/frequency/presence/disabled/max-power state, ordered end-exclusive SAR ranges, and the optional configured external cap. Empty SAR means no additional limit. The encoder validates completeness and generation before transport, pre-encodes all pages, retains 127 for missing/disabled channels, and applies the first matching SAR range plus cap only to semantic fields of enabled channels; 5-GHz CCK and all eight VHT holes remain 127. Native no-override body, raw-page, and envelope SHA-256 goldens cover all 62 records. Fuchsia currently supplies channel identity/authorization but no authoritative `max_reg_power_dbm`; production now preserves that absence and rejects publication rather than substituting captured 20 dBm/40 half-dBm values. No per-rate override source exists, so none is represented or parsed. |
| Connac2 authentication RX metadata strip | `mt7921/mac.c::mt7921_mac_fill_rx`, `mt76_connac2_mac.h` | `parse_mt7921_auth_rx` | adapted, tested; SAE interpretation excluded | Raw SAE fields delivered unchanged after RX error/header-format validation |

This table is not yet exhaustive. The next inventory pass must list every
function, struct/union/enum, macro/constant, and global in every scoped file,
including an explicit `unmapped` row. Until then the source-complete dependency
gate is **closed**.

The exact derivation-patched full-feature offline baseline is:

```sh
source=$(nix build .#mt7921-fuchsia-source --no-link --print-out-paths)
CARGO_TARGET_DIR=/tmp/mt7921-full-baseline cargo test --locked \
  --manifest-path "$source/crates/mt7921-passive-scan/Cargo.toml" \
  --no-default-features --features fuchsia-passive,full-firmware-production
```

Testing `mt7921-port-spike` directly or the working-tree passive-scan crate is
not equivalent: that bypasses the pinned host patches in the source derivation.

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
