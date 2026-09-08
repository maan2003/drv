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

The target is to exercise the same production Rust driver implementation through
deterministic and native hardware interfaces, as required by
[REQ-hardware-independent-testing](REQ-hardware-independent-testing.md).
Existing tests have different scopes: legacy Wasm resource tests, native typed
backend/lifecycle tests, chip protocol oracles, and normalized SoftMAC
conformance. Test-only subsystem models do not by themselves establish complete
production-driver behavior. Attribute virtual time, scripted responses, fault
injection, and hardware coverage to the test that actually implements them.

QEMU's `edu` PCI device exercises real VFIO/iommufd mechanics; a later
`vfio-user` model may combine those mechanics with BCM-specific behavior.
Cuttlefish's virtio `mac80211_hwsim`, wmediumd, and OpenWRT are optional peers for
scan and authentication behavior. They model SoftMAC radio behavior and cannot
validate BCM FullMAC firmware, PCIe rings, `msgbuf`, DMA, or reset.

Physical tests calibrate rather than merely confirm models. `no-plastic` is the
shared generic VFIO and MT7921 test host; Redwood uses it for builds and USB
control. Coordinate hardware windows rather than inferring availability from
a document. Asahi/M2 feasibility remains separate unproved work described by
[ARCH-asahi-wifi-target](ARCH-asahi-wifi-target.md), not a scheduled test suite.

Runs persist reports locally and recover independently of the experimental
Wi-Fi path. A physical result establishes only the exercised configuration and
failure modes, not all model behavior or production readiness. Recovery and
scope for Redwood follow
[ARCH-redwood-wifi-target](ARCH-redwood-wifi-target.md).
