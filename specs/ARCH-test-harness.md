# ARCH-test-harness: Layered deterministic verification

## Status

The in-memory broker runs the production Wasm probe through BAR programming,
device DMA, interrupt, bounds failure, reset, and stale-handle checks. A QEMU
VM suite covers `edu` enumeration, IOMMU grouping, exclusive `vfio-pci`
binding, the iommufd device interface, region and IRQ discovery, DMA mapping,
teardown, and clean shutdown. The `no-plastic` physical smoke path now covers
transactional `mt7921e` handoff, VFIO cdev and iommufd attachment, a private
low-IOVA DMA map/unmap, explicit IOAS teardown, deadline recovery, and automatic
kernel-driver and iwd restoration without device MMIO. Virtual Wi-Fi and
device-specific physical Wi-Fi behavior remain unimplemented.
A deterministic Bluetooth transport oracle covers bounded HCI event framing,
command-credit handling, duplicate discovery reports, malformed discovery
input, and cleanup planning against pinned Sapphire fixture shapes. The
`no-plastic` physical oracle covers time-bounded LE scanning and BR/EDR inquiry
through an exclusive Linux HCI user channel, root-only structured reporting,
exact initial controller-flag restoration, and post-run exclusive
reacquisition while Wi-Fi remains active.

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

Physical tests calibrate rather than merely confirm models. The x86_64 AMD host
`no-plastic` is the primary generic physical VFIO, deployment, and recovery
test device. Its Wi-Fi and Bluetooth hardware is also the first target for
proving transactional handoff between a normal host driver and a safe Rust
service while the other host service remains available. Scheduled `m2sh` tests
prioritize BCM4387 Wi-Fi firmware, scan, association, traffic, timeout, reset,
and automatic `brcmfmac` restoration through the development broker. Tests run
to completion locally and spool durable reports while Wi-Fi and the remote
session are unavailable. Complete group-10 VFIO tests validate the production
path later; Bluetooth transport and profile tests are secondary. This
architecture satisfies
[REQ-hardware-independent-testing](REQ-hardware-independent-testing.md).
