# NOTES-mt7921-vfio-pci-substrate: MT7921 cut 2 VFIO PCI substrate

## Scope

Cut 2 should replace the MT7961 physical path's handwritten VFIO ownership in
`mt7921-port-spike/src/bin/vfio_read.rs` with
`drv_hardware::Device<drv_hardware_backends::LinuxVfio>`, constructed by
`LinuxVfio::open_pci_coherent`. It should not create another HAL or move
MT7921 register, ring, interrupt-source, or reset policy out of
`mt7921-core`. This inventory describes the current master implementation and
the physical capabilities already recorded in the port-spike README.

The target reported RESET and PCI device flags, nine VFIO regions, and five
IRQ indices. Its usable register aperture is PCI BAR0. IRQ discovery reported
INTx count 1 with EVENTFD, MASKABLE, and AUTOMASKED; MSI count 32 with EVENTFD
and NORESIZE; and MSI-X count 0. The active path therefore uses MSI.

## Substrate comparison

| Area | Current MT7921 VFIO path | LinuxVfio PCI flavour | Gap for cut 2 |
| --- | --- | --- | --- |
| Device and IOMMU ownership | Opens the VFIO cdev and `/dev/iommu`, then directly binds, allocates an IOAS, and attaches it. Acquisition and containment ledgers track partial progress. | `open_pci_coherent` performs the same bind/allocate/attach sequence and retains the device, iommufd, and IOAS owners. It checks PCI/reset flags only after attach. | Replace the duplicated owners and ioctls. Preserve both the pre-construction D0/MSE-on/BME-off gate and its post-attach revalidation before any BAR or DMA access. |
| BARs | Discovers all nine regions but maps only BAR0, in 4 KiB pages. Always-used pages include the L1 selector at `0xfe000`, dynamic window at `0x40000`, SWDEF at `0x9f000`, and DMASHDL at `0xd6000`; operation-specific pages include patch table `0x21000`, PCIe MAC `0x10000`, WFDMA `0xd4000`, CONN `0xe0000`, and the 13 passive-MAC pages. Dynamic addressing writes the selector and accesses the window page. No other BAR is used. | PCI flavour accepts only BAR indices 0 through 5, validates the VFIO region, maps a selected BAR once, and provides checked 32-bit MMIO. `MmioRegion::slice` creates generation-checked bounded subwindows. | Open BAR0 once and express every current page allowlist as a slice or MT7921 address translator. Do not expose the full BAR to higher layers or move selector save/select/verify/restore policy into the generic backend. |
| IRQ capability and vector selection | `InstallDisableVfioIrq` now uses typed logical MSI vector 0 after verifying WFDMA and its host source are disabled, and `MaskAckDisabledFwdl` uses the typed BAR0 slice for exact mask/status/W1C handling. The active MCU operation still owns a raw eventfd-backed MSI vector, drains its 64-bit counter, masks/acknowledges MT7921 host and MAC sources, and uses acquire ordering before consuming descriptors. Pinned mt76 allocates one vector for MT7921. | After the IRQ fix-forward already present on master, construction prefers nonempty eventfd MSI-X, then MSI; the target therefore selects its 32-vector MSI index. Logical vector N installs VFIO start N. `wait_interrupt` and non-serial `wait_any` return counters with monotonic timestamps. Automasked level IRQs are rearmed before another wait and before deassignment. Release/reset retries failed disable and invalidates generation-bound handles. | Scope cut 2 to logical MSI vector 0. The target advertises NORESIZE, which LinuxVfio does not retain; installing a one-vector extent prevents adding a second vector later. Multiple vectors require atomic/full-extent activation and duplicate-vector rejection first. No pending IRQ branch needs importing: `e4368f66`, `2974bf01`, `f7c23d28`, and `bd6d00f0` are already covered on master. |
| DMA allocation and addressing | `StageDisabledFirmwareDescriptor` now uses typed constrained coherent allocations: a bidirectional descriptor ring and device-readable payload, both below 4 GiB. The remaining operations create page-aligned anonymous arenas at fixed low IOVAs beginning at `0x0100_0000`; those `DmaArena` mappings are still device-readable and device-writable regardless of logical direction. Separate arenas cover TX/RX guards, firmware and MCU rings, command and firmware payloads, WM/WM2 buffers, data RX, and management TX. | Typed coherent/streaming allocations enforce direction, alignment, size, segment, and maximum-device-address constraints. PCI flavour derives IOMMU permissions from `DmaDirection`, allocates from the same low base, returns device addresses rather than exposing IOVA choice, zeros fresh memory, and revokes mappings on release/reset. | Audit each arena's real direction, stop depending on exact fixed IOVAs, program returned `DeviceAddress` values, and require `max_device_address = u32::MAX`. Keep distinct ownership for rings and buffers, using splits only where one allocation's lifetime and direction are genuinely shared. |
| DMA visibility and synchronization | Assumes coherent x86 PCI DMA. CPU reads/writes the shared arenas directly, with release fences before descriptor publication and acquire fences after completion. It does not call a cache-maintenance API. Cleanup explicitly zeroes sensitive payloads and unmaps arenas. | PCI flavour reports cache coherent. Coherent handles provide checked access but no ownership-transition methods. MMIO writes are release ordered after coherent DMA writes, and MMIO reads are acquire ordered before subsequent coherent DMA reads. Streaming transitions remain available when semantically required. | Keep publication behind a releasing MMIO doorbell. Completion must perform the acquiring device status/index MMIO read before consuming coherent descriptors or payloads; where no such read exists, retain or add an explicit acquire operation rather than assuming the eventfd wait orders DMA. Preserve secure payload wiping and RX reuse invariants. |
| Reset and teardown | Masks MAC/host sources, disables and polls WFDMA, clears BME, disables the VFIO IRQ, and releases resources. Some current operations, including `PrepareOwnedGlobalTxRings`, reset while DMA remains pinned and then read the old BAR mappings; the current active cleanup instead releases mappings before reset. | `reset` first invalidates the generation, retries IRQ revocation, revokes PCI DMA mappings, issues VFIO reset, and makes all issued BAR/DMA/IRQ handles stale. Failed DMA release is quarantined rather than forgotten. | Cut 2 selects unmap-before-reset, matching pinned mt76's C teardown order—unregister/disable NAPI, take driver ownership, clean DMA, then reset WFSYS—and the normal LinuxVfio contract. Every retained operation must migrate to that order. Post-reset verification must reopen BAR0/slices in the new generation. If BME-off readback fails, retain the whole `Device` for watchdog recovery; do not release mappings or call reset. |
| PCI configuration | Reads endpoint and parent configuration for D0, MSE/BME, PM, PCIe/ASPM, and writes Command for INTx disable and BME transitions with readback. | Deliberately exposes BAR, DMA, IRQ, and reset—not PCI configuration space or sysfs. | Add or retain one narrow typed PCI-control owner for Command/PM/PCIe capability access. It must preserve saved bits, read back every write, and be coordinated with LinuxVfio reset; do not add arbitrary config-space access to `Backend`. |
| Raw UAPI | Locally declares or invokes GET_INFO, GET_REGION_INFO, GET_IRQ_INFO, SET_IRQS, RESET, BIND/ATTACH/DETACH_IOMMUFD_PT, IOAS_ALLOC/MAP/UNMAP/DESTROY, mmap, eventfd, and polling operations. Some map and IRQ work already delegates to `userspace-vfio`. | `LinuxVfio` and `userspace-vfio` own all of these operations, including correct DEVICE_FEATURE SET bit, exact per-vector SET_IRQS payloads, all-ready ppoll, retryable teardown, and RAII. | The completed cut must contain no raw VFIO/iommufd ioctl, mmap, eventfd, or poll code in the MT7921 path. PCI sysfs/config access is the separate typed gap above, not an exception for raw VFIO. |

## Closure order

1. **Apply the selected reset containment.** Convert every retained operation
   to unmap-before-reset; do not add a reset-while-pinned backend operation.
   Normal reset invalidates every handle, so post-reset checks must reopen BAR0
   and its slices in the new generation.
2. **Freeze the PCI-control seam.** Extract the exact identity, D0, MSE/BME,
   INTx-disable, PM, and ASPM operations behind a narrow fail-closed owner with
   saved-value rollback and readback. Check D0/MSE-on/BME-off both before
   `open_pci_coherent` and after attach, before BAR or DMA access.
3. **Construct and bound the device.** Use `open_pci_coherent`, open BAR0,
   create the existing page-level slices, and port selector translation without
   widening the MT7921 register allowlists.
4. **Move inactive DMA first.** Audit every arena's direction, then allocate
   guards, rings, buffers, and payloads through typed 32-bit-constrained DMA
   handles and program only returned addresses. Prove zeroing, descriptor
   layout, publication ordering, partial unwind, and no engine/source enable.
5. **Adopt the landed IRQ path.** Install only MSI logical vector 0 before
   enabling any MT7921 source, then port mask/status/W1C and completion ordering.
   Do not add another vector without full-extent NORESIZE handling.
6. **Port active sequencing.** Enable BME, WFDMA, firmware, and radio only after
   the same containment gates used today. Publish behind a releasing MMIO write
   and perform an acquiring completion/status MMIO read before coherent DMA
   consumption; retain an explicit fence where no such register read exists.
7. **Unify teardown.** Revoke device sources and WFDMA and clear BME first. A
   failed BME-off readback retains the entire `Device` for watchdog recovery.
   Only a successful readback may enter the selected release/reset order and
   post-reset safe-state verification.
8. **Delete duplication.** Remove the local VFIO/iommufd structs, constants,
   unsafe ioctl/mmap/eventfd/poll helpers, fixed-IOVA arena, and duplicate
   lifecycle ledgers only after fake-transport failure tests and one bounded
   physical equivalence run cover acquisition, interrupt delivery, timeout,
   cancellation, and teardown.

This cut remains constrained by [ARCH-hardware-isolation](ARCH-hardware-isolation.md)
and [REQ-host-portability](REQ-host-portability.md): device policy stays in
`mt7921-core`, while generic authority and resource lifetime stay in the
shared hardware/VFIO layers.
