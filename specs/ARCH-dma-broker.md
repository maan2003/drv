# ARCH-dma-broker: Current DMA path and experimental broker support

## Status

Redwood's QMI-only Run A successfully used the default coherent VFIO/iommufd
path through device bind and IOAS setup. A custom cache-maintenance broker is
not an established prerequisite for that path. Admission and QMI progress do
not establish correct packet DMA under load.

The userspace backend retains an explicit experimental DMA-broker mode and its
API bindings. Compatible kernel behavior has not been demonstrated on Redwood.
This is not the production backend choice or a requirement to implement a
custom kernel UAPI. The earlier broad broker and narrowed doorbell prescriptions
are withdrawn; existing experimental code is not removed by this record.

## Current boundary

The default backend owns an iommufd IOAS and maps dedicated backend-owned RAM
through it. Coherency admission, CPU/device ordering, resource bounds, and safe
mapping lifetime remain required by
[ARCH-hardware-isolation](ARCH-hardware-isolation.md). A CPU fence is not cache
maintenance for genuinely non-coherent hardware; such a device needs an
appropriate backend rather than silently reusing coherent assumptions.

The GIC interrupt doorbell is a separate issue from RAM coherency and from the
QMI-discovered register aperture. `IOMMU_IOAS_MAP` does not expose arbitrary
physical MMIO mapping. Redwood currently polls without a device-visible GIC
doorbell mapping; the existing mechanism and unresolved interrupt gap are
explained in [NOTES-iommufd-sw-msi](NOTES-iommufd-sw-msi.md).

No custom broker, new DMA capability, or kernel-domain switch is prescribed
before that interrupt path is needed. Select the smallest device-scoped solution
from actual hardware evidence then. Do not mix the existing iommufd domain with
an experimental default-domain broker as though they were the same owner.
[REQ-isolation](REQ-isolation.md) continues to require exclusive device/domain
ownership and bounded access; ath11k protocol policy stays in userspace.
