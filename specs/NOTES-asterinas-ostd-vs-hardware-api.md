# Asterinas OSTD (framekernel) vs `hardware-api`: what to borrow

Reference: asterinas 819d662 (`ostd/src/mm/dma`, `ostd/src/irq`, `ostd/src/io/io_mem`,
`kernel/libs/aster-util/src/safe_ptr.rs`, `kernel/libs/dma-pool`,
`kernel/core/comps/virtio/src/queue.rs`). Also checked Rust-for-Linux 7.2
(`rust/kernel/{dma,irq,devres,io,revocable}.rs`) for contrast.

## Same architecture, different substrate

Asterinas' framekernel rule is exactly our rule: one small crate (OSTD, ~43k lines,
~1.2k `unsafe` sites) owns every raw resource and exposes only sound safe APIs; everything
above it (`#![deny(unsafe_code)]`: virtio, dma-pool, network) is safe Rust. Our
`hardware-api` + `hardware-backends` play OSTD's role; ath11k/mt7921 crates play the
services role. OSTD is a real kernel (it owns page tables, IOMMU, TLB), we sit on VFIO,
so their *implementation* is not borrowable, but their *safe-API shapes* are, and they
have been validated by a working virtio/net stack in safe Rust.

## DMA comparison

| OSTD | hardware-api today | Verdict |
|---|---|---|
| `DmaCoherent` / `DmaStream<D>` split | `CoherentDma<B,D>` / `StreamingDma<B,D>` | Same. Keep. |
| `DmaDirection` sealed trait with `const CAN_READ_FROM_DEVICE / CAN_WRITE_TO_DEVICE` and `const { assert!(..) }` in `alloc`/`sync_*` | sealed marker traits `CpuRead/CpuWrite/DeviceRead/DeviceWrite` | Equivalent; ours is finer-grained. Keep ours. |
| `is_cache_coherent: bool` **per allocation**; non-coherent => uncacheable KVA mapping, or cache-maintenance when the arch can (`can_sync_dma()`), else bounce copy (`Inner::Both`) | `Device::is_cache_coherent()` reports the backend property; streaming ownership transitions are no-ops on coherent backends and call backend synchronization on non-coherent backends. Deterministic tests exercise both paths. | Same portable driver shape, with coherency selected per backend rather than per allocation. |
| `Split` (page-aligned split of a DMA object into two owned objects) | `CoherentDma::split_at` and `StreamingDma::split_at` create alignment-checked owned suballocations that share the underlying mapping. | Same ownership shape. |
| Access only via `VmReader/VmWriter` + `read_val::<T: Pod>` / `write_val`; **no `&T` into DMA memory ever** | Both DMA types provide byte access and alignment-checked `read_pod`/`write_pod`; they do not expose references into device memory. | Same discipline. |
| `VmIoOnce::read_once/write_once` = single non-tearing load/store, alignment-checked | Both DMA types provide alignment-checked `read_once`/`write_once` for sealed 32-bit `PodOnce` values. | Same single-access shape for device-shared words. |
| Explicit `fence(SeqCst)` in the virtqueue between descriptor write and avail-index publish | The backend contract makes MMIO writes release-ordered after coherent DMA writes and completed streaming handoffs, and MMIO reads acquire-order later coherent DMA reads. The deterministic backend can reject a declared descriptor-before-doorbell violation. | Equivalent ordering is explicit at the publication boundary. |
| `DmaPool<D>` (safe crate): fixed-size sub-page streaming segments, refcounted pages, returned on drop, `deny(unsafe_code)` | `crates/dma-pool` is a safe crate generic over `Backend`; it splits streaming pages into direction-typed segments, bounds retained free segments, and is used by ath11k DP RX/REO paths. | Borrowed. MT7921 adoption remains a driver-local decision. |
| `SafePtr<T, M: VmIo, Rights>` typed pointer into a `VmIo` object with static rights (`Dup`, `Write`), `field_ptr!` macro | typed descriptor codecs in ath11k-hal | Do **not** borrow the rights machinery (heavy). Borrow `field_ptr!`-style offset projection only if HAL codecs get painful. |
| Constructors require IRQs enabled (documented), drop allowed in IRQ context | n/a (userspace) | n/a. |
| CVM/TDX shared-page handling | n/a | n/a. |

Rust-for-Linux 7.2 for contrast: `Coherent<T>` gives `unsafe fn as_ref/as_mut` and
`dma_read!/dma_write!` field projections (unsafe, "device must be halted before drop");
`CoherentBox<T>` is the CPU-only pre-share phase; `Devres<T>`/`Revocable<T>` revoke MMIO
on unbind. Our generation-tied `Device<B>` handles are the equivalent of `Devres`:
every handle carries the device generation, and a reset bumps it. Nothing to borrow
there beyond confirming the design.

## IRQ: what they do

| OSTD | hardware-api today | Verdict |
|---|---|---|
| `IrqLine` = owned allocation of a vector; `on_active(Fn(&TrapFrame) + Send + Sync)` registers a callback; callbacks unregister on handle drop; `Clone` shares the line, each clone owns its own callbacks | `Interrupt<B>` = owned eventfd-backed vector; `wait_until(deadline)` / `wait_any(&[..])`; drop = release/mask | Callback vs wait model. **Keep ours**. In userspace there is no top half; a blocking `wait_any` loop *is* the bottom half, and it keeps the driver single-threaded and transcript-deterministic. Callbacks would reintroduce the `Sync` + interior-mutability burden they pay in OSTD (`RwLock<Vec<Box<dyn Fn>>>`). |
| Physical disable = drop the handle | same (eng-xvq1 chose exactly this for the DP interrupt aggregate) | Same. |
| Top/bottom-half levels, nested interrupt levels, `disable_local()` guards | n/a | n/a in userspace. |
| MSI-X: virtio transport allocates one `IrqLine` per vector and hands `&mut IrqLine` to each queue | eng-nwai's vfio-pci flavour: per-vector `Interrupt`, `wait_any` across vectors | Same shape. |

## MMIO: what they do

| OSTD | hardware-api today | Verdict |
|---|---|---|
| `IoMem` acquired from a global allocator that has *removed* system-owned ranges; range-checked; `slice(range)` sub-windows sharing the mapping (`Arc<KVirtArea>`) | `MmioRegion<B>` from `Device::open_region(index)` is range-checked; `slice(offset, len)` creates independently owned bounded subwindows sharing the mapping and generation. | Same bounded-view shape; VFIO withholds system-owned ranges. |
| `IoMem<Sensitive>` marker: security-sensitive MMIO (IOMMU, interrupt controller) only reachable with unsafe inside OSTD | n/a; VFIO already withholds those | n/a. |
| `read_once/write_once` typed `PodOnce`, alignment checked | `read_u32/write_u32` | Add `read_u64/write_u64` when a device needs it; otherwise same. |
| Cache policy per mapping (`Uncacheable` default) | backend-decided | n/a. |

## Borrowed surface now present

The safe API now includes bounded `MmioRegion` slices, owned coherent and
streaming DMA splits, typed POD and single-access operations, explicit
DMA/MMIO publication ordering, backend coherency reporting with deterministic
coherent/non-coherent coverage, and the safe generic `crates/dma-pool`. The
pool is integrated into ath11k DP; this note does not require every driver to
adopt it.

Not borrowed: callback-based `IrqLine`, `SafePtr` rights system, top/bottom-half
machinery, CVM handling, RfL `Devres`/`Revocable` (our generation handles already cover it).

## Longer-term relevance

OSTD remains useful prior art because its safe surface is close to
`hardware-api`: capability handles, byte/POD DMA access, and no references into
device memory. It does not create a requirement to preserve an OSTD backend or
an in-kernel-driver path. [REQ-host-portability](REQ-host-portability.md) still
requires host-portable resource mechanisms, independent of any particular
kernel substrate.
