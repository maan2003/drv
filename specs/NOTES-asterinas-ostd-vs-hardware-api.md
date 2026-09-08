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

## DMA: what they do that we do not

| OSTD | hardware-api today | Verdict |
|---|---|---|
| `DmaCoherent` / `DmaStream<D>` split | `CoherentDma<B,D>` / `StreamingDma<B,D>` | Same. Keep. |
| `DmaDirection` sealed trait with `const CAN_READ_FROM_DEVICE / CAN_WRITE_TO_DEVICE` and `const { assert!(..) }` in `alloc`/`sync_*` | sealed marker traits `CpuRead/CpuWrite/DeviceRead/DeviceWrite` | Equivalent; ours is finer-grained. Keep ours. |
| `is_cache_coherent: bool` **per allocation**; non-coherent => uncacheable KVA mapping, or cache-maintenance when the arch can (`can_sync_dma()`), else bounce copy (`Inner::Both`) | coherency is a backend-wide property (broker vs coherent flavour) | **Borrow the idea**: expose `Device::is_cache_coherent()` (measured on redwood) and let `StreamingDma::sync_*` be a no-op on coherent backends, so the same driver code is correct on np (coherent PCI) and redwood (non-coherent). We already do this implicitly; make it explicit and test both paths in the deterministic backend. |
| `Split` (page-aligned split of a DMA object into two owned objects) | none; we allocate per ring | **Borrow**: `CoherentDma::split_at(offset)` lets the DP aggregate carve TCL/WBM/REO rings from one allocation the way ath11k C does (`dp_srng` from one `dma_alloc_coherent`), keeping addresses contiguous for descriptors that assume it. Cheap to add, no unsafe outside the backend. |
| Access only via `VmReader/VmWriter` + `read_val::<T: Pod>` / `write_val`; **no `&T` into DMA memory ever** | `read(offset,&mut [u8])`/`write(offset,&[u8])` bytes only; typed descriptors are built in HAL via byte codecs | Same discipline, ours via bytes. Their `Pod` + `read_val`/`write_val` is the ergonomic version. **Borrow**: `read_pod::<T: FromBytes+AsBytes>` / `write_pod` on both DMA types, with `PodOnce` (single non-tearing access) for descriptor words the device may update concurrently (WBM/REO head-pointer words, CE ring indices). |
| `VmIoOnce::read_once/write_once` = single non-tearing load/store, alignment-checked | `read_u32` only on MMIO; DMA reads are memcpy | **Borrow** for DMA: HAL SRNG head/tail-pointer reads in host memory (`hp_addr` shadow) must be single accesses, not memcpy, or the C ordering contract is not reproduced. |
| Explicit `fence(SeqCst)` in the virtqueue between descriptor write and avail-index publish | our ordering contract lives in MMIO (`write_u32` = release, `read_u32` = acquire) but DMA writes have no documented ordering vs a later doorbell | **Borrow the doc, not the code**: state in `hardware-api` that `CoherentDma::write` is ordered before a subsequent `MmioRegion::write_u32` on the same thread (backend must guarantee: release store or `dma_wmb` equivalent). Add a deterministic-backend check that a doorbell write observed before the descriptor write is a recorded violation. |
| `DmaPool<D>` (safe crate): fixed-size sub-page streaming segments, refcounted pages, returned on drop, `deny(unsafe_code)` | DP allocates one `StreamingDma` per packet | **Borrow as a crate** (`crates/dma-pool`, safe, generic over `Backend`): RX refill for 1024-entry RXDMA ring with per-packet `alloc_streaming` is the wrong cost model. Same shape as their `DmaPool`/`DmaSegment`, mapping to our `StreamingDma<B,FromDevice>` + offset slices. |
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
| `IoMem` acquired from a global allocator that has *removed* system-owned ranges; range-checked; `slice(range)` sub-windows sharing the mapping (`Arc<KVirtArea>`) | `MmioRegion<B>` from `Device::open_region(index)`, range-checked | **Borrow `slice`**: `MmioRegion::slice(offset, len) -> MmioRegion` so CE/HAL/DP each get a window bounded to their register block (CE_n base, TCL/REO/WBM blocks) instead of the whole BAR. Cheap, no unsafe outside backend, and it turns "wrong register block" into a range error in the deterministic backend. |
| `IoMem<Sensitive>` marker: security-sensitive MMIO (IOMMU, interrupt controller) only reachable with unsafe inside OSTD | n/a; VFIO already withholds those | n/a. |
| `read_once/write_once` typed `PodOnce`, alignment checked | `read_u32/write_u32` | Add `read_u64/write_u64` when a device needs it; otherwise same. |
| Cache policy per mapping (`Uncacheable` default) | backend-decided | n/a. |

## Concrete borrow list (ordered by payoff, all safe-API additions to `hardware-api`)

1. `MmioRegion::slice(offset, len)` sub-windows. Small; improves DP/CE/HAL isolation.
2. `CoherentDma::split_at(offset)` / `StreamingDma::split_at` (page/alignment-checked) for
   the DP aggregate's one-allocation ring carving.
3. `read_pod/write_pod<T: FromBytes+AsBytes>` and `read_once/write_once<T: PodOnce>` on
   DMA objects; HAL SRNG shadow pointers use the `_once` forms.
4. Documented ordering: DMA write → later MMIO write on the same thread is release-ordered;
   deterministic backend records violations.
5. `crates/dma-pool` (safe, generic over `Backend`) modelled on Asterinas `dma-pool`, for
   RX refill and TX packet buffers in ath11k-dp and mt7921.
6. Explicit `Device::is_cache_coherent()` with both paths exercised in `DeterministicBackend`.

Not borrowed: callback-based `IrqLine`, `SafePtr` rights system, top/bottom-half
machinery, CVM handling, RfL `Devres`/`Revocable` (our generation handles already cover it).

## Longer-term relevance

If this program ever moves the drivers into a kernel, OSTD is the substrate whose safe
surface is closest to `hardware-api` (both are "capability handles, bytes/Pod in DMA,
no references into device memory"). Keeping our `Backend` trait shaped so that an OSTD
backend (`DmaCoherent`/`DmaStream`/`IoMem`/`IrqLine`) could implement it is cheap and
worth preserving as a constraint when the trait changes.
