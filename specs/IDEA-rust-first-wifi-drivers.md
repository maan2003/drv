# IDEA-rust-first-wifi-drivers: Native Rust Wi-Fi drivers

## Status

This is an unfinalized direction for discussion. It does not supersede the
Wasm-driver architecture in [ARCH-drv](ARCH-drv.md),
[ARCH-hardware-isolation](ARCH-hardware-isolation.md), or
[REQ-hardware-independent-testing](REQ-hardware-independent-testing.md).

## Idea

Production Wi-Fi drivers would be rewritten as native, predominantly safe Rust
rather than preserving their Linux C implementations inside Wasm. They would run
within a process- and capability-sandboxed network service and access hardware
only through the bounded broker required by
[REQ-isolation](REQ-isolation.md).

The main obstacle to reusing a Linux driver is its coupling to Linux, not its C
syntax. PCI lifecycle, DMA and scatter/gather APIs, interrupts, `sk_buff`,
workqueues, timers, locking, firmware loading, power management, `cfg80211`, and
`mac80211` must be replaced regardless of the implementation language. Retaining
C in Wasm additionally requires kernel-API emulation and adaptation of native
pointers, DMA memory, callbacks, threads, and interrupt processing to Wasm.

The Rust driver would preserve hardware knowledge rather than Linux structure:

```text
network policy and Fuchsia-derived SME/MLME/security
        -> safe Rust firmware protocol and device state machines
        -> typed command/event and TX/RX rings
        -> bounded native hardware broker
        -> IOMMU and device
```

The broker remains responsible for VFIO or another host device API, IOMMU
mappings, BAR mappings, interrupt descriptors, reset, cache maintenance, and
resource quotas. Driver code receives typed regions, DMA handles and offsets,
interrupt notifications, firmware artifacts, and lifecycle operations rather
than native pointers or ambient host authority. Unavoidable unsafe Rust should
be small, locally owned, and audited; packet and descriptor parsing should use
checked representations.

### Adopt the OSTD DMA model

The driver-facing DMA abstraction should start from Asterinas OSTD's design,
described in the [Asterinas fit spike](../crates/asterinas-fit-spike/README.md),
rather than inventing an unrelated API. Preserve its important distinctions:

- coherent buffers versus streaming buffers that require explicit range sync;
- sealed `ToDevice`, `FromDevice`, and `FromAndToDevice` direction types;
- device addresses distinct from CPU offsets and addresses;
- zero-initialized allocation by default, bounds-checked access, owned mapping
  lifetimes, and automatic unmapping on drop; and
- typed views over descriptor and MMIO memory instead of raw pointers.

This is an adoption of the abstraction, not a direct dependency on OSTD's
kernel implementation. The safe driver API should wrap broker capabilities,
with the deterministic model, Linux VFIO/iommufd, and a possible future
Asterinas host implementing the same resource operations. Each backend retains
ownership of page allocation, IOMMU mappings, cache maintenance, revocation,
and unsafe host mechanics. The process boundary also requires project-specific
generation checks, quotas, and copied access; those constraints must not be
weakened merely to achieve source compatibility with OSTD.

## Hardware targets

For BCM4387C2 FullMAC, the Rust port would use Asahi/Linux `brcmfmac` and
Fuchsia `brcmfmac` as behavioral references for PCIe/MSGBUF transport, firmware
boot, DMA rings, commands, events, NVRAM, calibration, and Ethernet frames. A
Fuchsia-derived SME and security layer would sit above the FullMAC adapter. This
refines the potential implementation of
[ARCH-asahi-wifi-target](ARCH-asahi-wifi-target.md) without changing that
record's physical target.

For MT7921/MT7922 SoftMAC, the Rust port would preserve the hardware-facing
`mt76` and MediaTek knowledge: WFDMA, MCU commands, firmware and patch loading,
EEPROM/calibration, descriptors, radio configuration, and supported offloads.
Fuchsia-derived SoftMAC MLME, SME, and security logic would replace the relevant
Linux `mac80211`, `cfg80211`, and userspace-supplicant responsibilities.

Both paths converge on the same Ethernet-frame, MAC, MTU, carrier, and queue
boundary consumed by a host kernel during transition or by the proposed native
network service later.

## Limited role for C and Wasm

Wasm remains a containment tool, not the default driver architecture. It is
appropriate when a specific inherited C subsystem is demonstrably cheaper to
adapt than rewrite, changes frequently enough that upstream synchronization is
more valuable than a native port, or must temporarily remain executable during
bring-up.

An adapted C driver may also serve as a non-production oracle: execute it
against deterministic hardware, record commands, descriptors, events and state
transitions, and compare the Rust implementation. It should be removed from the
production path once the Rust behavior is proven.

Device firmware blobs are unaffected by this choice. They execute on the device
and remain untrusted; either host language must load the same firmware and
contain it with the IOMMU, bounded DMA mappings, broker validation, reset, and
timeouts. Wasm only contains host-side C and does not sandbox device firmware.

## Adoption consequences

Adopting this idea would require revising the current assumption that every
production hardware driver is a Wasm artifact. Deterministic and native brokers
would instead exercise the same Rust driver through a shared typed broker
interface, while the no-WASI component contract would remain available for C or
otherwise untrusted imported drivers. That architectural change should be made
only after a Rust hardware-protocol slice demonstrates that native porting is
less complex than maintaining a C compatibility layer.
