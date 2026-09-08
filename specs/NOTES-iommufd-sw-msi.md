# NOTES-iommufd-sw-msi: Existing machinery and deferred WCN6750 interrupt gap

## Hardware message

WCN6750 does not emit a PCI-style MSI. Its rings write the GICv3 message-based
SPI doorbell at physical address `0x17a10040`, with data equal to the absolute
SPI number. In the pinned ath11k source,
`ath11k_ahb_setup_msi_resources()` (`ahb.c:817-864`) obtains that address from
register resource 0, maps one page with `dma_map_resource()` (`ahb.c:833-843`),
and sets the base data to Device Tree `interrupts[1] + 32`
(`ahb.c:848-852`). The addition converts the DT GIC SPI number to the absolute
SPI used by GICD `SETSPI_NSR`. The platform IRQ resources for those same SPIs
remain the CPU-facing delivery path exposed as vfio-platform eventfd indices.

With an iommufd IOAS, userspace cannot reproduce `dma_map_resource()` using
`IOMMU_IOAS_MAP`: the GIC MMIO page is not pinnable userspace memory. Eventfd
setup can therefore succeed while the device still has no IOMMU-visible
address at which to assert an interrupt.

## Bind-time wired-IRQ gate

Kernel #3 also rejects `VFIO_DEVICE_BIND_IOMMUFD` with `EPERM` before IOAS
attachment. On arm64, `iommufd_device_bind` requires
`iommu_group_has_isolated_msi()`; that predicate is false for a platform
device with wired IRQs, independently of whether userspace later needs the
software-MSI doorbell described below.

The polling-only first run therefore uses the runtime parameter
`iommufd.allow_unsafe_interrupts=1`. This is bounded for that run because no
GIC doorbell page is mapped into the IOAS, so WCN6750 has no device-visible
address at which to raise an interrupt. Installing any software-MSI mapping or
otherwise making the GIC doorbell reachable invalidates that justification and
must revisit the parameter before enabling the interrupt path.

## Machinery already in Linux 7.2

The read-only Redwood target tree at `509ce3d952d5` already contains the generic
pieces, and kernel configuration #3 enables `CONFIG_IOMMUFD=y` and
`CONFIG_VFIO_DEVICE_CDEV=y`. `ARM_GIC_V3` selects `IRQ_MSI_IOMMU` in
`drivers/irqchip/Kconfig`, so the relevant code is built on this platform.

* arm-smmu-v3 publishes a 1 MiB `IOMMU_RESV_SW_MSI` window beginning at
  `MSI_IOVA_BASE` from `arm_smmu_get_resv_regions()`
  (`drivers/iommu/arm/arm-smmu-v3/arm-smmu-v3.c:4270-4283`).
* On device attach, iommufd reserves that window in the IOAS and records its
  base (`drivers/iommu/iommufd/io_pagetable.c:1502-1548` and
  `device.c:369-432`). Userspace cannot collide an ordinary IOAS mapping with
  it.
* When an IRQ domain calls `iommu_dma_prepare_msi(desc, physical_address)`, an
  iommufd-owned domain dispatches to `iommufd_sw_msi()`
  (`drivers/iommu/iommu.c:4227-4265`). iommufd assigns a file-global page in
  the reserved window, maps the physical page into every required paging
  domain with `IOMMU_MMIO`, and records the IOVA in the MSI descriptor
  (`drivers/iommu/iommufd/driver.c:180-300`).
* `msi_msg_set_addr()` then substitutes that IOVA while preserving the offset
  within the physical page (`include/linux/msi.h:308-331`). For the first
  mapping of `0x17a10040`, this would normally produce an address equivalent to
  `MSI_IOVA_BASE + 0x40`; the exact base must be treated as kernel-derived, not
  hard-coded by userspace. The message data remains the absolute SPI, matching
  ath11k's `interrupts[1] + 32`.

This mapping does not make WCN6750 work unchanged. The normal trigger is an MSI
allocation through an IRQ domain such as GICv3 MBI, whose allocation callback
calls `iommu_dma_prepare_msi()` and whose compose callback calls
`msi_msg_set_addr()` (`drivers/irqchip/irq-gic-v3-mbi.c:78-145`). WCN6750's 32
IRQs are pre-existing wired platform resources, not MSI descriptors allocated
for the wifi device. The Kodiak DT GIC node also does not advertise
`mbi-ranges`; ath11k deliberately constructs the message from resource 0 and
its wired SPI property instead. No current VFIO or iommufd UAPI asks the kernel
to install this software-MSI mapping and return the resulting address/data
pair.

## Deferred interrupt support

The source machinery above explains the missing doorbell path; it is not an
approved implementation plan. Continue polling until interrupts are needed.
No custom VFIO feature, synthetic MSI allocation, default-domain broker, or
new portable `MsiDoorbell` type is currently prescribed. See
[ARCH-dma-broker](ARCH-dma-broker.md) for the admitted DMA path and the status of
experimental broker support.

Any eventual interrupt mechanism must authorize the device's own message,
respect page-granularity access and mapping lifetime, and preserve exclusive
IOMMU ownership. Page safety and a working live doorbell mapping remain
unverified. The polling-only unsafe-interrupts justification above must not be
carried into a mapped interrupt path without reassessment.
