# drv: Isolated Userspace Wi-Fi Drivers

## Goal

Run the complete Wi-Fi device stack outside the Linux kernel without a VM.
Hardware, controller, policy, and network components run in sandboxed Wasm;
a native broker provides VFIO and limits DMA to dedicated untrusted memory.

The first target is BCM4387C2 FullMAC PCIe Wi-Fi in the 13-inch M2 MacBook Air
(`t8112-j413`) running Asahi Linux. Firmware implements most 802.11 MAC behavior;
userspace handles initialization, DMA rings, `msgbuf`, commands, and frames.

## Architecture

```text
applications
    |
typed network capability API
    |
sandboxed IP/transport stack
    | versioned component interface
sandboxed Wi-Fi control and policy
    | typed controller/driver interface
Wasm hardware driver (no WASI)
    | narrow capability protocol
native VFIO broker
    | VFIO + iommufd
IOMMU -> Broadcom PCIe Wi-Fi device
```

Components own device and network policy. The broker owns resource safety: it
bounds BAR access, manages interrupts and DMA, and never maps arbitrary process
memory. Each component gets a separate instance and only adjacent capabilities.

## Initial Interfaces

The worker needs a small host ABI resembling:

```text
bar_read/bar_write(bar, offset, width)
dma_alloc(size, alignment) -> opaque handle and IOVA
dma_read/dma_write(handle, offset, bytes)
interrupt_wait()
device_reset()
firmware_read(artifact, offset, length)
monotonic_time()
```

Handles and offsets cross IPC boundaries; native pointers do not. Packet copying
is preferred until correctness and isolation are established. Zero-copy shared
rings can be evaluated later without changing the security model.

Interfaces between driver, controller, protocol stack, and applications are
project-owned, versioned component interfaces. Linux boundaries such as
`cfg80211`, HCI sockets, ALSA, or TAP are optional adapters, not architecture.
Existing projects such as iwd, BlueZ, and PipeWire may be forked behind these
interfaces when their Linux assumptions obstruct isolation.

## Scope

Initially included:

- PCIe FullMAC Wi-Fi only;
- one M2 Air Wi-Fi function; its companion Bluetooth function may be claimed
  but will not be driven when IOMMU grouping requires ownership of both;
- firmware loading, RX/TX, scanning, association, and key management;
- a minimal userspace network stack and application-facing capability API;
- TAP only as an optional bring-up and compatibility adapter;
- process sandboxing, Wasm limits, watchdogs, and deterministic restart.

Initially excluded:

- SDIO and USB transports;
- SoftMAC devices;
- transparent POSIX socket compatibility;
- Bluetooth, audio, display, and GPU service domains;
- generic Linux kernel-module compatibility;
- transparent support for every `cfg80211`/`nl80211` feature;
- protection of the assigned Wi-Fi hardware from its driver.

## Development Plan

1. Inventory the target device, IOMMU group, reset support, firmware, and NVRAM.
2. Build a small Rust VFIO/iommufd broker and verify BAR access and interrupts.
3. Establish a fixed DMA arena and validate isolation experimentally.
4. Boot firmware and port the PCIe ring and `msgbuf` paths from `brcmfmac`.
5. Exchange copied Ethernet frames through a temporary TAP diagnostic adapter.
6. Add scan, association, authentication, and key-management control.
7. Move driver and control logic into separate no-WASI Wasm components.
8. Add a userspace network stack and typed application capability API.
9. Split runtimes and VFIO broker into separately sandboxed processes.
10. Add trace replay, fuzzing, fault injection, recovery, and reset tests.

Linux `brcmfmac` is a behavioral reference. We port required responsibilities,
not Linux-internal abstractions such as workqueues, `net_device`, or `cfg80211`.
