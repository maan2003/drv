# iommufd software-MSI path for the WCN6750 doorbell

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

## Smallest credible iommufd design

A generic solution should add a device-authorized software-MSI message query,
not expose arbitrary resource mapping:

1. vfio-platform validates that the requested message belongs to one of the
   device's own wired IRQ resources and that its physical target is the single
   page containing device register resource 0. For WCN6750 that target is
   `0x17a10040`; the page must be confirmed safe as a whole because the IOMMU
   maps at page granularity.
2. Through an iommufd helper, the bound device requests installation of that
   physical page in its IOAS's `IOMMU_RESV_SW_MSI` window. The helper should
   reuse iommufd's file-global mapping and per-HWPT installation/lifetime
   tracking rather than create an `IOMMU_IOAS_MAP` entry.
3. A narrow VFIO device feature returns only a composed `{address, data}`
   message. Address is the kernel-selected reserved-window IOVA plus the
   `0x40` page offset; data is the validated absolute SPI. Userspace cannot
   supply a physical address, IOVA, page, or unrelated SPI.
4. Attach/replace must install the mapping in every required paging HWPT, and
   device detach/file close must release it through iommufd's existing
   software-MSI map lifetime. Reset revokes the userspace `MsiDoorbell`
   capability even if the file-global mapping remains cached for another
   attached device.

Trying to allocate synthetic platform MSIs is the wrong shortcut: it may
allocate different SPIs, does not describe the already wired vfio-platform
IRQ indices, and depends on an MBI domain the Redwood DT does not publish. A
new raw-physical-address IOAS ioctl is also out of scope because it would
bypass the device and IRQ-domain authorization which makes software-MSI
mapping safe.

## Contrast with the narrowed broker

The narrowed design in [ARCH-dma-broker](ARCH-dma-broker.md) calls
`dma_map_resource()` while vfio-platform retains the device's default domain,
returns the same address/data-only capability, and rejects both
`SET_CONTAINER` and `BIND_IOMMUFD`. It is the lower-schedule out-of-tree option:
the existing ath11k operation is reproduced directly, and no generic
MSI-descriptor gap has to be solved. Its costs are a private vfio-platform
mode and inability to use a userspace IOAS for the device.

The iommufd design keeps normal cdev/IOAS ownership and builds on upstream
reserved-region, multi-HWPT, and lifetime machinery. Its kernel surface can be
small, but it crosses iommufd, VFIO platform, and IRQ authorization, and the
current tree lacks the request/query seam. Recommendation remains: use polling
for the first hardware run; keep the narrowed broker as the bounded deployment
fallback; prototype the device-authorized iommufd message query afterward and
prefer it for steady state only if page safety and attach/reset lifetime are
proved on Redwood.
