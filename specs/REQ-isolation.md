# REQ-isolation: Contain hostile device stacks

## Status

The deterministic broker proves interface-level bounds only. Process sandboxing,
VFIO/IOMMU enforcement, and physical fault tests remain unimplemented.

Source: project owner. Strength: mandatory.

A compromised driver, protocol component, device firmware, or assigned device
must not compromise the host kernel, applications, unrelated CPU memory, other
devices, IOMMU configuration, or persistent storage. Every component treats
adjacent output as hostile.

No-IOMMU operation is forbidden. A raw VFIO path exclusively assigns its
complete IOMMU group. Only dedicated backend-owned arenas are mapped, and native
boundaries validate handles, arithmetic, ranges, alignment, state, and quotas.
Wasm receives no ambient WASI, native pointer, host descriptor, arbitrary
mapping operation, or raw device capability through an application interface.

A native Rust driver may use language safety as its privilege boundary. In that
model, the driver and its driver-facing dependencies forbid unsafe Rust and
ambient host I/O, while a separate audited implementation crate privately owns
VFIO/iommufd handles, mappings, pointers, and other unsafe mechanics. Every safe
public operation must preserve the same device, generation, region, DMA, IOMMU,
and quota invariants as the broker boundary. Merely wrapping an unrestricted
ioctl, mapping, address, or MMIO operation in a safe function does not satisfy
this requirement.

The assigned hardware and network availability need not be protected from their
driver. Side channels, physical attacks, broken isolation hardware, platform
firmware, and denial of service within configured limits are outside scope.
