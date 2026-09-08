# ARCH-hardware-isolation: Driver resource boundary

## Status

The native safe hardware API, deterministic backend, and Linux VFIO/iommufd
backend exist. MT7921 still mixes typed resources with older active-path
ownership while migrating to the shared boundary. The netstack process is
sandboxed; equivalent Wi-Fi service confinement remains incomplete. Legacy
WIT/probe artifacts do not define the native production boundary.

A sandboxed native Rust driver receives only its assigned device capabilities.
The safe driver-facing crate forbids unsafe Rust and ambient host I/O; its
private backend owns native handles, pointers, mappings, and unsafe mechanics.
Safe public operations preserve device identity, generation, bounds, direction,
state, and quotas. Typed APIs are not a substitute for process sandboxing.

DMA uses dedicated zero-initialized backend-owned arenas, never application
memory, unrelated worker memory, or backend control state. Coherent and streaming
buffers are distinct; streaming ownership transitions perform the required
range synchronization. Device addresses are distinct from CPU addresses;
CPU/device access permissions follow DMA direction. Descriptor and MMIO views
are typed and bounded. Reset revokes generations and cannot release live DMA
owners before containment is established.

Only the native resource implementation owns VFIO/iommufd handles, IOMMU and BAR
mappings, IRQ descriptors, cache maintenance, and resource quotas. Assignment
and privileged recovery belong to a narrow supervisor/host binding, not Wi-Fi
network-selection policy. The backend may live inside the driver process; a
separate hardware-broker process is not required by this architecture.

The kernel retains narrow shared mechanisms such as IOMMU, interrupt routing,
and platform firmware/reset support. Device-specific protocol policy remains
in userspace. Complete IOMMU groups are assigned exclusively; no-IOMMU operation
is forbidden. Current DMA backend selection and the distinction between RAM
coherency and interrupt-doorbell mapping are described in
[ARCH-dma-broker](ARCH-dma-broker.md).

This boundary implements [REQ-isolation](REQ-isolation.md), preserves
[REQ-host-portability](REQ-host-portability.md), and is exercised by
[ARCH-test-harness](ARCH-test-harness.md).
