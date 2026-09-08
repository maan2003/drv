# OSTD IOMMU and DMA comparison

This audit compares `drv-hardware` and its two backends with the Asterinas
OSTD source pinned by [the fit spike](README.md) at
`2e2b3468f07815be2c372fd5cd103bb37664ad5c`. Paths and line numbers below
refer to that immutable archive. The local side refers to stable Rust items
because its line numbers move as the API converges.

## Provenance

The pinned files are MPL-2.0 and carry SPDX headers. No OSTD source was copied
or modified here, and this project's MIT/Apache-2.0 source does not reproduce
MPL implementation text.

* **Direct reuse:** none.
* **Source adaptation:** none. The local code uses different storage, ownership,
  error, and backend models.
* **Design inspiration:** the coherent/streaming split, sealed direction types,
  range synchronization, device-address distinction, owned mapping lifetime,
  and typed-view goal. These concepts were reimplemented from their contracts,
  not translated from OSTD source.

## Exact source map

| Concern | Pinned OSTD | Current drv API/backend | Result |
| --- | --- | --- | --- |
| Domain and device ownership | `ostd/src/arch/x86/iommu/dma_remapping/mod.rs:74-122,124-160` maps through one global table; initialization shallow-copies one page table into every enumerated PCI context. | `Device<B>` exclusively owns one backend instance. `LinuxVfio::open` binds one VFIO cdev, creates one iommufd IOAS, and attaches the device; `DeviceAddress` is rejected across different backend instances. | drv has the necessary ownership shape. OSTD has no domain handle, attach/detach API, or private per-device address space. The edu backend does not yet enforce complete IOMMU-group ownership. |
| Coherent versus streaming | `ostd/src/mm/dma/dma_coherent.rs:19-92` provides always-CPU-visible coherent memory; `dma_stream.rs:70-227` requires explicit synchronization. | `CoherentDma` immediately copies reads/writes through the backend; `StreamingDma` stages CPU bytes and exposes `sync_for_cpu/device`. | Intentionally aligned. drv asks the backend to implement coherence; OSTD may instead map uncached memory or a bounce KVA. |
| Direction types | `dma_stream.rs:20-68,117-123,201-227,392-437` seals three directions and gates readers, writers, and sync methods. Coherent DMA is not direction-typed. | `Direction`, `CpuRead`, `CpuWrite`, `DeviceRead`, and `DeviceWrite` gate methods for both coherent and streaming DMA; `DmaDirection` crosses the private backend boundary. | drv is stricter for coherent buffers. `Bidirectional` versus OSTD's `FromAndToDevice` is only naming and should not be changed. |
| CPU and device addresses | `ostd/src/mm/mem_obj.rs:11-26` separates `HasPaddr`, `HasDaddr`, and size; `dma_stream.rs:364-389` returns a mapped Daddr or falls back to Paddr. | CPU storage is never addressable by the driver. A borrowed, opaque `DeviceAddress` contains a DMA token plus checked offset and can only be consumed by `MmioRegion::write_device_address`. | drv is safer for a hostile userspace driver and prevents raw IOVA arithmetic or cross-device use. OSTD exposes integer Paddr/Daddr values, which suits an in-kernel trusted-safe driver but not the broker threat model. |
| Allocation and initialization | `dma_coherent.rs:41-92` and `dma_stream.rs:95-199` allocate page-counted zeroed or uninitialized contiguous segments and can map an existing `USegment`. | drv allocates byte-counted, always-zeroed backend-owned arenas. Existing memory cannot be mapped. | Byte sizing and mandatory zeroing are justified userspace containment differences. Mapping arbitrary caller memory would violate the project isolation requirement. |
| Alignment, address width, segments | `dma_stream.rs:305-354` and `dma_coherent.rs:95-136` only split at page boundaries. `util.rs:38-65,267-313` allocates page-aligned IOVAs from the IOMMU virtual range. The x86 table is fixed to 39-bit, 4 KiB pages (`arch/x86/iommu/dma_remapping/second_stage.rs:11-61`). There is no per-allocation DMA mask, maximum segment size/count, boundary, or scatter/gather constraint. | Legacy allocators accept size/alignment. `DmaConstraints` and the additive `alloc_*_with_constraints` methods now express alignment, inclusive maximum device address, maximum segment size, and maximum segment count. Current mappings are one contiguous device-visible segment; unsupported constraints return `Limit`. | drv now covers the low-risk single-segment milestone and a 32-bit mask without breaking existing callers or backend implementers. Neither API models an SG list, segment boundary mask, or separate physical-contiguity requirement. |
| Mapping lifetime and drop | `dma_stream.rs:357-360`, `dma_coherent.rs:139-143`, and `util.rs:148-183` tie unmap to drop. | DMA tokens are released on drop; VFIO issues `IOMMU_IOAS_UNMAP`; regions and interrupts are also released on drop. | Same RAII intent. OSTD `util.rs:299-313` explicitly lacks IOTLB flush and never reuses freed IOVAs, so drop does **not** prove immediate device revocation. VFIO's unmap ioctl supplies the stronger userspace completion boundary, subject to the kernel/IOMMU contract. |
| Reset and generations | No reset generation exists in the DMA/IOMMU APIs. PCI construction enables bus mastering (`kernel/comps/pci/src/common_device.rs:97-138`); reset coordination is driver-specific. | Every handle captures `Backend::generation`; `Device::reset` advances it and all later safe operations on old region/DMA/IRQ handles return `StaleHandle`. Deterministic reset removes mappings and IRQ state. | Required drv extension. The edu VFIO backend advances generation even when its best-effort reset ioctl fails and does not unmap all live DMA at reset, so physical revocation/reset ordering remains a prototype blocker. |
| Cache maintenance | `dma_stream.rs:201-302` validates ranges, no-ops for coherent/uncached mappings, performs architecture cache maintenance, or copies through a bounce mapping. | Streaming sync validates ranges, copies between private CPU staging and backend storage, then invokes backend maintenance. Linux currently uses a SeqCst fence only. | drv has the right driver contract, but a fence is not cache maintenance on a noncoherent platform. The production backend must obtain platform/kernel-supported DMA synchronization rather than claim a CPU fence flushes caches. |
| Publish/acquire ordering | OSTD sync documents cache/data visibility but does not itself define descriptor publication ordering. Virtio adds SeqCst barriers around ring publication/acquisition (`kernel/comps/virtio/src/queue.rs:341-371`). | Sync methods imply transfer/maintenance but do not specify a Rust release before device notification or acquire after completion. | Both leave ordering partly to drivers. drv should add explicit publish/acquire operations or documented ordering to the typed descriptor layer, not silently strengthen byte-copy methods. |
| Typed DMA and MMIO views | OSTD consumers use rights-typed `SafePtr<T, M, R>`: construction and typed access are in `kernel/libs/aster-util/src/safe_ptr.rs:145-220,247-300`; virtio applies it to descriptors/rings in `kernel/comps/virtio/src/queue.rs:34-36,89,139-178,341-358`. `IoMem` owns bounded MMIO and checks ranges (`ostd/src/io/io_mem/mod.rs:50-169,213-230`). | drv exposes only aligned `u32` MMIO and byte slices. Device addresses are typed by direction and ownership, but descriptor layout, endian, volatile/nontearing width, and field rights are not represented. | This is the largest useful OSTD semantic missing from drv. A view must be descriptor-specific and Pod/layout-audited; a generic userspace `SafePtr` clone would be an unnecessarily broad wrapper. |
| Interrupt and reset interaction | DMA creation requires IRQs enabled, while drop is valid in IRQ context (`ostd/src/mm/dma/mod.rs:9-18`). `IrqLine` owns callbacks and unregisters/releases them on drop (`ostd/src/irq/top_half.rs:23-39,75-119,144-184`). MSI-X code binds BAR entries and IRQ lines (`kernel/comps/pci/src/capability/msix.rs:40-58,88-129,137-176`). No reset-generation rule connects them. | Interrupt waits are bounded capabilities and generation-checked. Reset stales IRQ and MMIO handles together with DMA. | drv's process-friendly notification model and cross-resource reset generation are necessary additions. It still lacks mask/drain/quiesce/reset ordering and reset scopes. |
| Quotas, bounds, containment | OSTD allocators impose global resource exhaustion, page-table range, and MMIO ownership bounds, but expose no per-driver quotas. All PCI functions share DMA mappings. Safe Rust protects kernel memory invariants, not mutually hostile services. | Driver operations check overflow, ranges, alignment, same-device identity, generation, direction, and backend limits. The deterministic backend caps one arena at 4 KiB. | drv has better capability bounds but no general per-device byte/allocation/IRQ quota object yet. Production group ownership, aggregate quotas, and fault accounting remain required. |
| Unsafe boundary and fake backend | OSTD contains unsafe mapping/cache primitives below safe drivers; the pinned DMA files identify their safety preconditions. It has kernel tests but no backend trait or userspace deterministic model. | `drv-hardware` forbids unsafe and has no host I/O. `Backend` is hidden from docs and implemented by a forbid-unsafe deterministic model and a separate Linux VFIO binary containing all FFI, mmap, and volatile unsafe code. | drv's boundary is the correct userspace adaptation. The native backend should move from the example binary into an audited implementation module before production use. Deterministic tests cover DMA/MMIO/IRQ/reset/bounds and now constraints. |

## Semantics OSTD cannot supply to VFIO/iommufd userspace

OSTD is not merely missing Linux bindings; its ownership model is different.
A safe userspace broker additionally needs:

1. explicit ownership of the iommufd, IOAS/domain, VFIO device, and whole IOMMU
   group, with transactional attach/detach;
2. pinned userspace memory lifetime and `IOMMU_IOAS_MAP/UNMAP` completion/error
   semantics;
3. per-device DMA masks, IOVA apertures, reserved ranges, quotas, and mapping
   permissions derived from direction;
4. reset generations spanning DMA, BAR mappings, interrupts/eventfds, and
   in-flight operations;
5. interrupt mask/drain and device-quiesce ordering before unmap/reset;
6. broker-safe copied or otherwise isolated CPU access, never a raw Paddr,
   pointer, file descriptor, or arbitrary mapping capability; and
7. malicious-driver accounting and recovery when ioctls, devices, or workers
   fail.

The global OSTD page table is specifically unsuitable: all functions receive
shallow copies of one table, mappings use a synthetic zero BDF, map permissions
are always read/write (`context_table.rs:286-318`), and unmap lacks IOTLB
completion.

## Deliberate and unnecessary divergence

Keep the byte-sized, zeroed, backend-owned buffers, opaque borrowed device
addresses, copied streaming access, generation checks, and interrupt
notifications. They are deliberate consequences of the userspace isolation
boundary.

Do not rename `Bidirectional`, sync methods, or error variants merely to match
OSTD. Do not expose Paddr or import `SafePtr` wholesale. The only needless
semantic divergence identified is that allocation constraints previously
stopped at alignment; the additive constraint methods close the enforceable
single-segment part without changing legacy calls.

## Staged convergence

1. **Completed here:** retain legacy allocation methods; add
   `DmaConstraints`, constrained coherent/streaming allocation, backend
   fail-closed defaults, exact deterministic enforcement, and tests.
2. **Typed descriptors:** add views only alongside the first real descriptor
   type. Require checked layout/alignment/endian conversion and make publish
   (release) and acquire explicit at the ownership transition. Keep raw generic
   pointer construction private.
3. **Production IOAS lifecycle:** make the native backend own a whole validated
   IOMMU group and a private IOAS; allocate non-overlapping IOVAs from its
   reported aperture; track aggregate quotas and every live mapping; make
   reset mask/drain/quiesce, reset, revoke/unmap, and generation advance a
   single state transition.
4. **Scatter/gather only when demanded:** return an owned list of opaque
   device-address segments subject to maximum count, length, boundary, and
   address mask. Do not expose arbitrary map operations.
5. **Noncoherent and physical verification:** implement cache maintenance only
   through a kernel/platform contract with documented ordering, then test
   VFIO unmap/reset/fault behavior in the existing VM/lab tiers. No physical
   claim should rely on the deterministic model.

Architectural blockers are the absence of a production group-owning backend,
kernel-supported noncoherent synchronization in the current Linux path,
specified reset/quiesce scopes, IOVA aperture allocation, and a first real
descriptor whose layout can anchor a narrow typed-view API.

