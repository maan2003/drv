# M2 Air Target Survey

## Exact Target

The first machine is the 13-inch M2 MacBook Air (2022), Apple target `J413`,
SoC `t8112`. Its device tree identifies:

- Wi-Fi PCI function `01:00.0`: Broadcom BCM4387C2, PCI ID `14e4:4433`;
- Bluetooth PCI function `01:00.1`: PCI ID `14e4:5f71`;
- firmware board type `apple,hokkaido`;
- a loader-populated MAC address and antenna SKU;
- a port power-enable GPIO and shared upstream PCIe reset.

These are ordinary PCI functions behind Apple's `pcie-apple` host controller,
not platform devices. The Wi-Fi function uses the `brcmfmac` PCIe transport and
msgbuf protocol.

## IOMMU Topology

PCIe port 0 is behind `pcie0_dart`, an Apple DART IOMMU. The device tree maps
bus 1 through one DART stream using an `iommu-map-mask` of `0xff00`. Wi-Fi and
Bluetooth therefore share the DART stream and probably the IOMMU group; sysfs
on the target machine must confirm this.

If they share a group, both functions must be detached from their kernel
drivers and claimed by VFIO. We will drive only Wi-Fi. Bluetooth can remain
unused under VFIO, but its availability and reset state are part of the
assigned hardware failure domain.

The Asahi DART driver implements paging domains, map/unmap operations, blocked
and identity domains, and explicit PCI grouping. Those are the mechanisms VFIO
and iommufd need, so the architecture appears viable. This is not proof until a
VFIO bind and DMA mapping succeed on the machine.

## Why Asahi's Driver Is Primary

Asahi's branch contains substantial BCM4387 support beyond the same-version
stable kernel. Relevant additions include:

- PCIe shared protocol v6 host capabilities and newer doorbells;
- MSI handling during firmware boot;
- Apple firmware signatures and memory-map/rTLV footers;
- required random-seed and NVRAM placement;
- newer reset behavior and core register access;
- extended scan, join, interface, ratespec, and event handling.

Port from the pinned Asahi tree first. Mainline remains useful for identifying
which behavior has since been cleaned up or upstreamed.

## Required Target Inventory

Run these locally before any driver is detached:

```sh
uname -a
readlink /sys/bus/pci/devices/0000:01:00.0/iommu_group
find /sys/bus/pci/devices/0000:01:00.0/iommu_group/devices -maxdepth 1 -type l
readlink /sys/bus/pci/devices/0000:01:00.0/driver
readlink /sys/bus/pci/devices/0000:01:00.1/driver
ls -l /dev/iommu /dev/vfio /dev/vfio/devices 2>&1
zgrep -E 'CONFIG_(VFIO|VFIO_PCI|IOMMUFD|APPLE_DART|PCIE_APPLE)=' /proc/config.gz
```

Also preserve `dmesg` from a successful normal `brcmfmac` boot and inventory
the selected firmware, signature, NVRAM, CLM, and txcap files under
`/boot/vendorfw` or `/lib/firmware`.

## First Gate

Before porting protocol code, prove that the stock Asahi kernel can bind both
functions to `vfio-pci`, expose the Wi-Fi VFIO device, attach a DART-backed
IOAS, map a private DMA arena, and deliver an interrupt. If iommufd/cdev is not
enabled in the distribution kernel, enable it in the NixOS kernel configuration
rather than falling back permanently to the legacy VFIO API.
