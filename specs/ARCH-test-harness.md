# ARCH-test-harness: Layered deterministic verification

## Status

Native deterministic driver tests, randomized lifecycle rigs, and pinned C
protocol oracles coexist with the legacy Wasm probe. The latter is not the
production artifact. A QEMU `edu` suite covers VFIO/iommufd mechanisms; physical
MT7921 tests have demonstrated association and Internet traffic. These do not
substitute for production containment, recovery, suspend/resume, or power
validation. Redwood remains a physical bring-up target.
A deterministic Bluetooth transport oracle covers bounded HCI event framing,
command-credit handling, duplicate discovery reports, malformed discovery
input, and cleanup planning against pinned Sapphire fixture shapes. The
`no-plastic` physical oracle covers time-bounded LE scanning and BR/EDR inquiry
through an exclusive Linux HCI user channel, root-only structured reporting,
exact initial controller-flag restoration, and post-run exclusive
reacquisition while Wi-Fi remains active.

The same production Rust driver implementation runs against deterministic and
native backends through the typed hardware interface. Lab entrypoints and
oracles do not substitute a second production implementation. The deterministic model owns virtual time and scripted BAR, DMA,
interrupt, reset, and fault behavior. It rejects unexpected operations and
supports malformed data, missing or repeated interrupts, timeouts, worker failures,
generation changes, and out-of-range access.

QEMU's `edu` PCI device exercises real VFIO/iommufd mechanics; a later
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
