# REQ-hardware-independent-testing: Fast tests without physical devices

## Status

Deterministic native tests cover hardware bounds and lifecycle behavior, with
randomized driver rigs and pinned-source protocol oracles. A QEMU `edu` suite
covers native VFIO/IOMMU mechanics. Physical MT7921 connectivity is demonstrated;
model coverage and laptop recovery evidence remain incremental.

Source: project owner. Strength: mandatory development requirement.

Most driver and service development must run deterministically without root,
physical hardware, or network access. The same production Rust driver implementation must run
against deterministic and native hardware backends through the shared typed
interface. Tests must not substitute a second driver implementation or mock
inside the driver.

Automated tests must cover DMA bounds, interrupt delivery, timeout, reset,
generation revocation, malformed device responses, worker failure, and clean
restart. Native VM and lab tests separately cover VFIO/IOMMU mechanics. No model
is proof of physical device behavior; physical tests remain required before claiming
hardware support.
