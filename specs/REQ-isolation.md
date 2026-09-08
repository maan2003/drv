# REQ-isolation: Contain hostile device stacks

## Status

Native VFIO/IOMMU paths and a sandboxed Netstack3 child exist alongside
deterministic bounds and lifecycle tests. Full Wi-Fi service sandboxing and
production recovery evidence remain incomplete; Internet connectivity alone
does not establish this requirement.

Source: project owner. Strength: mandatory.

A compromised driver, protocol component, device firmware, or assigned device
must not compromise the host kernel, applications, unrelated CPU memory, other
devices, IOMMU configuration, or persistent storage. Every component treats
adjacent output as hostile.

No-IOMMU operation is forbidden. A raw VFIO path exclusively assigns its
complete IOMMU group. Only dedicated backend-owned arenas are mapped, and native
boundaries validate handles, arithmetic, ranges, alignment, state, and quotas.
Portable driver code receives no ambient host I/O, native pointer, unrestricted
host descriptor, or arbitrary mapping operation. Application interfaces expose
no raw device capability.

Native Rust drivers run in strongly sandboxed processes. Within that boundary,
the driver and its driver-facing dependencies forbid unsafe Rust and
ambient host I/O, while a separate audited implementation crate privately owns
VFIO/iommufd handles, mappings, pointers, and other unsafe mechanics. Every safe
public operation must preserve the same device, generation, region, DMA, IOMMU,
and quota invariants as the broker boundary. Merely wrapping an unrestricted
ioctl, mapping, address, or MMIO operation in a safe function does not satisfy
this requirement. Language safety does not replace process confinement or the
IOMMU. Wasm is not the production containment mechanism.

The assigned hardware and network availability need not be protected from their
driver. Side channels, physical attacks, broken isolation hardware, platform
firmware, and denial of service within configured limits are outside scope.
