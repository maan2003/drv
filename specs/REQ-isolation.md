# REQ-isolation: Contain hostile device stacks

## Status

The deterministic broker proves interface-level bounds only. Process sandboxing,
VFIO/IOMMU enforcement, and physical fault tests remain unimplemented.

Source: project owner. Strength: mandatory.

A compromised driver, protocol component, device firmware, or assigned device
must not compromise the host kernel, applications, unrelated CPU memory, other
devices, IOMMU configuration, or persistent storage. Every component treats
adjacent output as hostile.

No-IOMMU operation is forbidden. The complete IOMMU group is exclusively
assigned, only dedicated broker-owned arenas are mapped, and native boundaries
validate handles, arithmetic, ranges, alignment, state, and quotas. Wasm receives
no ambient WASI, native pointer, host descriptor, arbitrary mapping operation, or
raw device capability through an application interface.

The assigned hardware and network availability need not be protected from their
driver. Side channels, physical attacks, broken isolation hardware, platform
firmware, and denial of service within configured limits are outside scope.
