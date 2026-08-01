# ARCH-drv: Isolated userspace device stacks

## Status

One no-WASI probe component runs against the deterministic broker through the
project WIT contract. Native VFIO and Wi-Fi behavior are not implemented.

The system moves complete device stacks out of the host kernel without placing
them in a VM. Its dependency direction is:

```text
applications -> compatibility services -> network/control components
             -> Wasm hardware driver -> native resource broker -> IOMMU/device
```

Application-facing protocols remain compatible where required, while internal
interfaces are project-owned and versioned. Sandboxed components own device and
protocol policy. The native broker owns resource safety and host integration but
does not interpret device commands.

Components receive only adjacent capabilities. Portable components do not see
host file descriptors, ioctls, pointers, VFIO, or Linux network abstractions.
The architecture is constrained by [REQ-isolation](REQ-isolation.md),
[REQ-application-compatibility](REQ-application-compatibility.md), and
[REQ-host-portability](REQ-host-portability.md).
