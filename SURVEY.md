# Source Survey

## Pinned References

Reference trees live under ignored `reference/`; they are research inputs, not
project dependencies or vendored code.

| Reference | Version | Archive SHA-256 |
|---|---:|---|
| Linux mainline | 7.2-rc5 | `8be5bf245c5bc89927f15a9f575c04869c25a8d511d8b810ecd8a67e5b0dd51e` |
| Linux stable baseline | 7.1.5 | `22a0196b3cbcdf34dc27b77561f4d040585fd3447edc9ab3531a1ac79e3041e7` |
| Asahi Linux `asahi` | `e8efe09d4f37` | `f06b95f68982ac9cc5948276b779c31de9d20a391b8178e8504e0c1c0cd24eea` |
| rust-vmm/vfio | vfio-ioctls 0.8.0 | `217a00c6bef441eff38621d387111e135eaad3f4ec23ff6b9d7417ab7e5fee5b` |

Sources:

- <https://git.kernel.org/torvalds/t/linux-7.2-rc5.tar.gz>
- <https://cdn.kernel.org/pub/linux/kernel/v7.x/linux-7.1.5.tar.xz>
- <https://github.com/AsahiLinux/linux/archive/e8efe09d4f378992c890d181d65e2ed8d8cb1194.tar.gz>
- <https://github.com/rust-vmm/vfio/archive/refs/tags/vfio-ioctls-v0.8.0.tar.gz>

## Linux Wi-Fi Reference

The reference driver is in
`drivers/net/wireless/broadcom/brcm80211/brcmfmac/`. PCIe FullMAC selects the
common driver, PCIe transport, and msgbuf protocol.

Start with these files:

| Responsibility | Linux reference |
|---|---|
| PCI IDs, BAR access, firmware boot, IRQs | `pcie.c` |
| Chip discovery and core reset | `chip.c`, `chip.h` |
| Firmware and NVRAM selection | `firmware.c`, `firmware.h` |
| Shared producer/consumer rings | `commonring.c`, `commonring.h` |
| PCIe firmware wire protocol | `msgbuf.c`, `msgbuf.h` |
| Dynamic TX rings | `flowring.c`, `flowring.h` |
| Firmware commands and events | `fwil.c`, `fweh.c`, their headers |
| Firmware ABI structures/constants | `fwil_types.h`, `cyw/fwil_types.h` |
| High-level behavior reference | `core.c`, `cfg80211.c` |

The useful porting boundary is not a Linux bus or network interface. It is:

```text
PCIe bootstrap -> firmware boot -> shared ring discovery -> msgbuf
               -> firmware commands/events -> Ethernet RX/TX
```

Do not initially port `cfg80211`, `net_device`, workqueues, skbuffs, debugfs,
power management, P2P, WoWLAN, or vendor extensions. Recreate only behavior
required to boot, scan, associate, and exchange Ethernet frames.

The driver files use the permissive ISC license. Any copied or translated code
must retain its applicable notices; keep provenance explicit per module.

## VFIO Reference

The authoritative API references in the Linux tree are:

- `Documentation/driver-api/vfio.rst`;
- `Documentation/userspace-api/iommufd.rst`;
- `include/uapi/linux/vfio.h`;
- `include/uapi/linux/iommufd.h`.

Use the modern device-cdev path rather than the legacy group/container API:

1. Open `/dev/iommu` and `/dev/vfio/devices/vfioX`.
2. Bind the VFIO device with `VFIO_DEVICE_BIND_IOMMUFD`.
3. Allocate one IOAS with `IOMMU_IOAS_ALLOC`.
4. Attach the device using `VFIO_DEVICE_ATTACH_IOMMUFD_PT`.
5. Discover PCI regions and IRQs with VFIO device ioctls.
6. Map only the dedicated DMA arena with `IOMMU_IOAS_MAP`.
7. Map BAR regions and connect MSI/MSI-X to eventfds.

`rust-vmm/vfio` is the practical Rust reference. Its `VfioIommufd` implementation
shows cdev binding, IOAS attachment, region access, IRQ setup, and DMA map/unmap.
We should initially depend on its maintained crates rather than reproduce ioctl
numbers and variable-sized VFIO structures. The broker must still wrap them in a
smaller API that cannot map arbitrary worker memory.

## First Implementation Slice

Build a native Rust broker probe before porting Wi-Fi code. Given a PCI BDF, it
should:

1. locate its VFIO cdev through sysfs;
2. open and bind it to a fresh iommufd IOAS;
3. print device, region, and IRQ capabilities;
4. allocate and map a small private DMA arena at a broker-selected IOVA;
5. unmap everything cleanly without touching device-specific registers.

This validates the security-critical foundation independently of Broadcom. It
also yields a narrow `Device`, `Region`, `Interrupt`, and `DmaArena` interface
that the later Wasm broker protocol can expose.
