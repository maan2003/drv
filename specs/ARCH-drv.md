# ARCH-drv: Secure Linux through isolated Rust userspace stacks

## Status

MT7921 has demonstrated userspace Wi-Fi association and Internet connectivity
through Fuchsia WLAN components and a separately sandboxed Netstack3 service.
Its hardware ownership migration and full Wi-Fi process sandbox remain
incomplete. Redwood WCN6750 is in physical bring-up, not production hardening.
Legacy Wasm/WIT probes and Bluetooth scaffolding remain in the tree; they are
not the production direction and are not extended by this architecture.

## Direction

The project owner's goal is a secure, dependable Linux laptop, built from the
bottom up by moving device drivers and protocol stacks out of the kernel,
without putting them in a VM. Drivers are native Rust, not Wasm. Strong process
sandboxing, safe Rust capability boundaries, and IOMMU confinement complement
one another; none replaces the others.

```text
owned applications and desktop
        -> capability-scoped policy and network services
        -> sandboxed native Rust device service
        -> typed hardware API / private native backend
        -> kernel IOMMU, DMA, interrupt and lifecycle mechanisms / device
```

Fuchsia is the primary architectural and component-reuse inspiration: reuse
Netstack3 and WLAN MLME/SME/RSN cores and their behavioral contracts, replacing
host bindings rather than duplicating protocol state machines. Linux is the
initial product substrate; portable contracts retain the host independence of
[REQ-host-portability](REQ-host-portability.md).

All system and desktop userspace is replaceable, and Internet-facing
applications are owned and modifiable. Existing Linux management APIs are not
the design boundary; see
[REQ-application-compatibility](REQ-application-compatibility.md).

Native backends own resource safety and host integration, not device protocol
policy. Portable driver code sees bounded device resources rather than host
file descriptors, unrestricted mappings, or ambient I/O. Process boundaries
separate hardware authority, Internet parsing, and persistent policy, as refined
by [ARCH-hardware-isolation](ARCH-hardware-isolation.md) and
[ARCH-wlan-stack-topology](ARCH-wlan-stack-topology.md).

## Product maturity

MT7921's goal is everyday laptop production readiness, beyond its already
proved Internet path: containment, dependable recovery and reconnect,
suspend/resume, power efficiency, supported Wi-Fi behavior, sustained
performance, and application integration. Connectivity alone is not acceptance.

The product priority order is containment, recovery/everyday reliability,
essential functionality, battery/performance, then wider hardware/features.
Security wins over availability when safe containment cannot be established.
Ordinary device faults should recover automatically; reboot is exceptional
containment, not routine recovery. The production system uses our userspace
stack only, with no native kernel-driver runtime fallback. A separate
known-good boot configuration for rollback or lab recovery is distinct.

Everyday acceptance includes browsing, video calls, streaming, downloads, VPN,
captive portals, enterprise Wi-Fi, and roaming, alongside suspend/resume and
reconnect. A successful Internet demonstration alone does not establish those
workloads or production maturity.

Redwood's current goal is physical testing that discovers driver-port bugs and
reaches scan, association, DHCP, and proved Internet connectivity. Production
hardening follows that milestone. Lab recovery and hardware safety constraints
apply throughout; see [ARCH-redwood-wifi-target](ARCH-redwood-wifi-target.md).
