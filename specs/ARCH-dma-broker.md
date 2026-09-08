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

## VFIO device-feature UAPI

Broker mode is a mode of the vfio-platform device, not a second character
device. The device remains attached to its kernel default IOMMU/SMMU domain
and is never attached to a userspace iommufd IOAS. The vfio-platform driver is
therefore the sole DMA-domain owner. The VFIO group still provides exclusive
device ownership, and the existing VFIO device fd remains the sole authority
for MMIO, IRQ, reset, and DMA broker operations.

The device fd advertises one out-of-tree `VFIO_DEVICE_FEATURE_DMA_BROKER`
feature. Its `data[]` starts with `struct vfio_device_dma_broker`, whose
`operation` selects `ALLOC_COHERENT`, `MAP_STREAMING`, `SYNC_CPU`,
`SYNC_DEVICE`, `FREE`, or `UNMAP`. `VFIO_DEVICE_FEATURE_PROBE` reports whether
broker mode is available. Operations use `VFIO_DEVICE_FEATURE_SET`; the kernel
copies allocation and mapping outputs back through the same argument. There is
no independent broker fd, arbitrary map ioctl, user-supplied IOVA, or raw
physical-address interface.

```c
#define VFIO_DEVICE_FEATURE_DMA_BROKER  /* out-of-tree feature index */
#define VFIO_DMA_BROKER_ALLOC_COHERENT  1
#define VFIO_DMA_BROKER_MAP_STREAMING   2
#define VFIO_DMA_BROKER_SYNC_CPU        3
#define VFIO_DMA_BROKER_SYNC_DEVICE     4
#define VFIO_DMA_BROKER_FREE            5
#define VFIO_DMA_BROKER_UNMAP           6
#define VFIO_DMA_TO_DEVICE              1
#define VFIO_DMA_FROM_DEVICE            2
#define VFIO_DMA_BIDIRECTIONAL           3

struct vfio_device_dma_broker {
        __u32 argsz;
        __u32 operation;
        __u32 flags;                    /* must be zero */
        __u32 handle;                   /* output for alloc/map; input otherwise */
        __u64 size;                     /* alloc/map input */
        __u64 alignment;                /* alloc/map input, power of two */
        __u64 max_device_address;       /* alloc/map input, inclusive */
        __u64 user_address;             /* streaming-map input only */
        __u64 offset;                   /* sync subrange input */
        __u64 length;                   /* sync subrange input */
        __u64 mmap_offset;              /* coherent-allocation output */
        __u64 iova;                     /* alloc/map output */
        __u32 direction;                /* streaming-map input */
        __u32 reserved;
};
```

The structure is carried in `struct vfio_device_feature.data`; both `argsz`
values must cover the supplied structures. Unknown flags, short structures,
nonzero reserved fields, arithmetic overflow, zero sizes, invalid alignment,
stale handles, disallowed directions, and out-of-range subranges fail without
changing state. Closing the VFIO device fd revokes every handle after the
required final ownership transition and unmap.

`ALLOC_COHERENT` calls the ordinary DMA API for this device, returns its
DMA address, and permits exactly one shared, non-executable mmap at the
returned VFIO device-fd offset. `FREE` applies only to coherent handles and
fails while that userspace mapping remains.

`MAP_STREAMING` pins only pages in a backend-owned anonymous arena and maps
them with the ordinary DMA API in the declared direction. It succeeds only if
the range has one contiguous device-address interval meeting the alignment
and maximum-address constraints. `UNMAP` applies only to streaming handles and
performs the final direction-appropriate CPU ownership transition before
unpinning.

`SYNC_DEVICE` and `SYNC_CPU` invoke `dma_sync_single_for_device` and
`dma_sync_single_for_cpu` for exactly the validated subrange. Synchronization
is serialized with unmap, free, and reset. It is never implemented as a CPU
fence, dirty-tracking operation, IOAS map/unmap cycle, or no-op on a
non-coherent device.

The vfio-platform device records allocation kind, direction, mapped length,
DMA metadata, and owning device file for every unpredictable handle. It
validates `offset + length <= mapped_length` with checked arithmetic. Reset and
file close revoke all DMA state. No operation accepts an IOVA, physical
address, kernel pointer, arbitrary fd, or arbitrary page range from the
caller.

## Portable API mapping

| `drv-hardware` operation | Coherent device | Non-coherent device |
|---|---|---|
| `alloc_coherent` | backend arena plus `IOMMU_IOAS_MAP` | `ALLOC_COHERENT` and VFIO device-fd mmap |
| `alloc_streaming` | backend arena plus `IOMMU_IOAS_MAP` | backend arena plus `MAP_STREAMING` device feature |
| `DeviceAddress` | IOAS-derived IOVA plus checked offset | broker-derived IOVA plus checked offset |
| `sync_for_cpu` | validated no-op after acquire ordering | `SYNC_CPU` |
| `sync_for_device` | validated no-op before release ordering | `SYNC_DEVICE` |
| DMA drop | `IOMMU_IOAS_UNMAP`, then arena release | `FREE` or `UNMAP`, then arena release |
| reset | revoke IOAS mappings before VFIO reset | revoke broker handles as part of VFIO reset |

The backend's MMIO reads remain acquire operations and MMIO writes remain
release operations as specified by `Backend`. Cache synchronization and
MMIO ordering are separate obligations.

## Alternatives

An iommufd-native non-coherent mode remains the preferred upstream future. It
would require explicit non-coherent IOAS mapping, cache synchronization and
uncached/coherent allocation UAPIs, together with a controlled relaxation of
the current coherency rejection. That cross-subsystem design is substantially
larger than the vfio-platform-local broker and is not assumed by this backend.

Userspace cache instructions are rejected because their availability depends
on architecture and privileged control state. Treating sync as a fence or
no-op is incorrect for non-coherent memory. IOAS unmap/remap per ownership
transition is neither a documented cache-maintenance API nor a practical ring
data path. Allocating every packet buffer coherent remains possible inside the
broker, but is not the default because it changes streaming-memory performance
and still requires the same kernel allocation/mmap boundary.
