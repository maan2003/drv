# ARCH-dma-broker: Non-coherent userspace DMA broker

## Status

The portable `drv-hardware` contract and coherent-device iommufd path exist,
but the broker UAPI is not implemented. Stage 2 will measure
`IOMMU_CAP_CACHE_COHERENCY` and the exact iommufd bind result for WCN6750.
Until that evidence exists, WCN6750 is treated as non-coherent.

Stock VFIO/iommufd cannot support non-coherent userspace DMA.
`drivers/iommu/iommufd/device.c:iommufd_device_bind` rejects devices lacking
`IOMMU_CAP_CACHE_COHERENCY` because iommufd always requests `IOMMU_CACHE`
and has no UAPI by which userspace can restore cache coherency.
`drivers/vfio/vfio_main.c:__vfio_register_dev` enforces the same condition for
IOMMU-backed VFIO devices. `IOMMU_IOAS_MAP`, unmap, and dirty tracking manage
page-table or migration state; none performs the per-ownership-transition cache
maintenance of `dma_sync_single_for_cpu` and
`dma_sync_single_for_device`.

The native backend uses stock VFIO/iommufd only for devices proven
cache-coherent. A non-coherent device uses a device-bound kernel broker for DMA
allocation, mapping, and synchronization. The broker is integrated with that
device's VFIO platform binding so there is exactly one DMA-domain owner; it
must not attach the same device to a separate userspace IOAS. VFIO retains
exclusive IOMMU-group ownership, bounded MMIO, eventfd interrupts, and reset.
The broker is device-neutral and contains no ath11k policy.

This boundary refines [ARCH-hardware-isolation](ARCH-hardware-isolation.md) and
is constrained by [REQ-isolation](REQ-isolation.md) and
[REQ-host-portability](REQ-host-portability.md).

## UAPI

The broker exposes one file descriptor bound to one VFIO-owned device. The
native backend obtains the corresponding `/dev/drv-dma/<vfio-device-id>`
node; opening fails unless that device and its complete viable IOMMU group are
exclusively owned by the same caller's VFIO context. Closing the fd revokes all
handles after unmapping them.

All fields are fixed-width little-endian native UAPI integers. Every request
must set `argsz` to the structure size and all `flags` and reserved fields
to zero. Unknown flags, short structures, arithmetic overflow, zero sizes,
invalid alignment, stale handles, disallowed directions, and out-of-range
subranges fail without changing state.

```c
#define DRV_DMA_IOC_MAGIC 0xDA
#define DRV_DMA_TO_DEVICE     1
#define DRV_DMA_FROM_DEVICE   2
#define DRV_DMA_BIDIRECTIONAL 3

struct drv_dma_alloc_coherent {
        __u32 argsz;
        __u32 flags;
        __u64 size;                 /* in */
        __u64 alignment;            /* in, nonzero power of two */
        __u64 max_device_address;   /* in, inclusive */
        __u32 handle;               /* out */
        __u32 reserved;
        __u64 mmap_offset;          /* out, broker-fd offset */
        __u64 iova;                 /* out */
};

struct drv_dma_map_streaming {
        __u32 argsz;
        __u32 flags;
        __u64 user_address;         /* in, page-owned backend arena */
        __u64 size;                 /* in */
        __u64 alignment;            /* in, nonzero power of two */
        __u64 max_device_address;   /* in, inclusive */
        __u32 direction;            /* in, DRV_DMA_* */
        __u32 handle;               /* out */
        __u64 iova;                 /* out, contiguous for size bytes */
};

struct drv_dma_sync {
        __u32 argsz;
        __u32 flags;
        __u32 handle;               /* in */
        __u32 reserved;
        __u64 offset;               /* in */
        __u64 length;               /* in */
};

struct drv_dma_release {
        __u32 argsz;
        __u32 flags;
        __u32 handle;               /* in */
        __u32 reserved;
};

#define DRV_DMA_ALLOC_COHERENT _IOWR(DRV_DMA_IOC_MAGIC, 0x00, \
                                     struct drv_dma_alloc_coherent)
#define DRV_DMA_MAP_STREAMING  _IOWR(DRV_DMA_IOC_MAGIC, 0x01, \
                                     struct drv_dma_map_streaming)
#define DRV_DMA_SYNC_CPU       _IOW (DRV_DMA_IOC_MAGIC, 0x02, \
                                     struct drv_dma_sync)
#define DRV_DMA_SYNC_DEVICE    _IOW (DRV_DMA_IOC_MAGIC, 0x03, \
                                     struct drv_dma_sync)
#define DRV_DMA_FREE           _IOW (DRV_DMA_IOC_MAGIC, 0x04, \
                                     struct drv_dma_release)
#define DRV_DMA_UNMAP          _IOW (DRV_DMA_IOC_MAGIC, 0x05, \
                                     struct drv_dma_release)
```

`ALLOC_COHERENT` uses the device's coherent DMA allocator, returns one
allocation-derived IOVA, and permits exactly one shared, non-executable mmap of
the returned size and offset. `FREE` applies only to coherent handles and
fails while a userspace mapping remains.

`MAP_STREAMING` pins only pages in a backend-owned anonymous arena, maps them
through the broker-owned DMA domain in the declared direction, and succeeds
only if the entire range has one contiguous device-address interval satisfying
the requested alignment and maximum address. `UNMAP` applies only to
streaming handles. It performs the final direction-appropriate CPU ownership
transition before unpinning.

`SYNC_DEVICE` accepts only to-device or bidirectional handles and invokes the
DMA API's device-ownership transition for exactly the checked subrange.
`SYNC_CPU` accepts only from-device or bidirectional handles and invokes the
CPU-ownership transition before returning. Synchronization is serialized with
unmap, free, and reset. It is never implemented as a compiler fence, CPU fence,
dirty-tracking operation, or no-op on a non-coherent device.

The broker records the allocation kind, direction, mapped length, DMA mapping
metadata, and owning file for every unpredictable handle. It validates
`offset + length <= mapped_length` with checked arithmetic and never accepts
an IOVA, physical address, kernel pointer, arbitrary fd, or arbitrary page
range from the caller.

## Portable API mapping

| `drv-hardware` operation | Coherent device | Non-coherent device |
|---|---|---|
| `alloc_coherent` | backend arena plus `IOMMU_IOAS_MAP` | `ALLOC_COHERENT` and broker mmap |
| `alloc_streaming` | backend arena plus `IOMMU_IOAS_MAP` | backend arena plus `MAP_STREAMING` |
| `DeviceAddress` | IOAS-derived IOVA plus checked offset | broker-derived IOVA plus checked offset |
| `sync_for_cpu` | validated no-op after acquire ordering | `SYNC_CPU` |
| `sync_for_device` | validated no-op before release ordering | `SYNC_DEVICE` |
| DMA drop | `IOMMU_IOAS_UNMAP`, then arena release | `FREE` or `UNMAP`, then arena release |
| reset | revoke IOAS mappings before VFIO reset | revoke broker handles before VFIO reset |

The backend's MMIO reads remain acquire operations and MMIO writes remain
release operations as specified by `Backend`. Cache synchronization and
MMIO ordering are separate obligations.

## Alternatives

Userspace cache instructions are rejected because their availability depends
on architecture and privileged control state. Treating sync as a fence or
no-op is incorrect for non-coherent memory. IOAS unmap/remap per ownership
transition is neither a documented cache-maintenance API nor a practical ring
data path. Allocating every packet buffer coherent remains possible inside the
broker, but is not the default because it changes streaming-memory performance
and still requires the same kernel allocation/mmap boundary.
