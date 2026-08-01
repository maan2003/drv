# ARCH-test-harness: Layered deterministic verification

## Status

The in-memory broker runs the production Wasm probe through BAR programming,
device DMA, interrupt, bounds failure, reset, and stale-handle checks. Virtual
Wi-Fi, native broker, and physical suites remain unimplemented. A QEMU
VM suite covers `edu` enumeration, IOMMU grouping, exclusive `vfio-pci`
binding, the iommufd device interface, region and IRQ discovery, DMA mapping,
teardown, and clean shutdown.

The production Wasm binary runs unchanged against deterministic and native
brokers. The deterministic model owns virtual time and scripted BAR, DMA,
interrupt, reset, and fault behavior. It rejects unexpected operations and
supports malformed data, missing or repeated interrupts, timeouts, traps,
generation changes, and out-of-range access.

QEMU's `edu` PCI device will exercise real VFIO/iommufd mechanics; a later
`vfio-user` model may combine those mechanics with BCM-specific behavior.
Cuttlefish's virtio `mac80211_hwsim`, wmediumd, and OpenWRT are optional peers for
scan and authentication behavior. They model SoftMAC radio behavior and cannot
validate BCM FullMAC firmware, PCIe rings, `msgbuf`, DMA, or reset.

Physical tests calibrate rather than merely confirm models. `new-plastic` covers
generic physical VFIO and recovery; scheduled `m2sh` tests cover BCM firmware,
scan, association, traffic, reset, and normal-driver restoration. This
architecture satisfies [REQ-hardware-independent-testing](REQ-hardware-independent-testing.md).
