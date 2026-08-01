# drv: Isolated Userspace Wi-Fi Drivers

## Goal

Run Wi-Fi hardware drivers outside the Linux kernel without requiring a VM.
A ported driver runs as WebAssembly, while a small native broker provides access
to one PCIe Wi-Fi device through VFIO. The IOMMU limits device DMA to dedicated,
untrusted packet memory.

The first target is a Broadcom FullMAC PCIe device supported by Linux
`brcmfmac`. FullMAC is attractive because firmware implements most 802.11 MAC
behavior; the userspace driver primarily handles device initialization, DMA
rings, the `msgbuf` protocol, firmware commands/events, and Ethernet frames.

## Architecture

```text
applications
    |
Linux networking through TAP (initially)
    |
Wi-Fi control and packet service
    | IPC / copied packets
Wasm driver worker (no WASI)
    | narrow capability protocol
native VFIO broker
    | VFIO + iommufd
IOMMU -> Broadcom PCIe Wi-Fi device
```

The Wasm module owns device policy and may issue any valid device command. The
broker owns resource safety, not device policy: it bounds BAR accesses, manages
interrupts, allocates DMA buffers, and never permits the driver to map arbitrary
process memory.

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

## Scope

Initially included:

- PCIe FullMAC Wi-Fi only;
- one device and one driver instance;
- firmware loading, RX/TX, scanning, association, and key management;
- TAP integration so existing Linux applications continue using normal sockets;
- process sandboxing, Wasm limits, watchdogs, and deterministic restart.

Initially excluded:

- SDIO and USB transports;
- SoftMAC devices;
- replacing the Linux TCP/IP stack;
- generic Linux kernel-module compatibility;
- transparent support for every `cfg80211`/`nl80211` feature;
- protection of the assigned Wi-Fi hardware from its driver.

## Development Plan

1. Inventory the target device, IOMMU group, reset support, firmware, and NVRAM.
2. Build a small Rust VFIO/iommufd broker and verify BAR access and interrupts.
3. Establish a fixed DMA arena and validate isolation experimentally.
4. Boot firmware and port the PCIe ring and `msgbuf` paths from `brcmfmac`.
5. Exchange Ethernet frames through TAP, beginning with copied packets.
6. Add scan, association, authentication, and key-management control.
7. Move portable driver logic into a no-WASI Wasm component.
8. Split the Wasm runtime and VFIO broker into separately sandboxed processes.
9. Add trace replay, fuzzing, fault injection, watchdog recovery, and reset tests.

Linux `brcmfmac` is a behavioral reference. We port required responsibilities,
not Linux-internal abstractions such as workqueues, `net_device`, or `cfg80211`.
