# REQ-hardware-independent-testing: Fast tests without physical devices

## Status

No test harness exists yet. The first slice must establish the deterministic
component path; broader fault, network, VM, and physical suites follow.

Source: project owner. Strength: mandatory development requirement.

Most driver and service development must run deterministically without root,
physical hardware, or network access. The exact production Wasm artifact must run
against both the model and native broker so tests do not mock inside the driver.

Automated tests must cover DMA bounds, interrupt delivery, timeout, reset,
generation revocation, malformed device responses, worker failure, and clean
restart. Native VM and lab tests separately cover VFIO/IOMMU mechanics. No model
is proof of BCM4387 behavior; physical tests remain required before claiming
hardware support.
