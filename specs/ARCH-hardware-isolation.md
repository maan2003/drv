# ARCH-hardware-isolation: Driver resource boundary

## Status

The proposed boundary is not implemented yet. The first slice will define its
WIT contract and deterministic broker before native VFIO integration.

An untrusted no-WASI component receives one injected device capability and
cannot enumerate host devices. Subordinate region, DMA, interrupt, and artifact
resources are unforgeable, bounded, and revoked when the device generation
changes.

The broker allocates dedicated zeroed DMA arenas at broker-selected IOVAs. It
never maps Wasm linear memory, application memory, broker state, or arbitrary
worker memory into the device domain. BAR access is copied and allowlisted;
interrupts are notifications rather than event descriptors. Firmware artifacts,
monotonic time, and boot randomness are explicit capabilities.

Only the native broker owns device discovery and binding, VFIO/iommufd handles,
IOMMU mappings, BAR mappings, eventfds, reset selection, companion IOMMU-group
functions, cache maintenance, and quotas. This boundary implements
[REQ-isolation](REQ-isolation.md) and is exercised by
[ARCH-test-harness](ARCH-test-harness.md).
