# Security Model

## Objective

A buggy or compromised Wi-Fi driver, including one exploited by hostile network
traffic or device firmware messages, must not compromise the Linux kernel, other
processes, other devices, or unrelated CPU memory.

The assigned Wi-Fi device and its availability are not protected from the
driver. The driver is allowed to fully control, reset, misconfigure, or render
that device unusable.

## Trusted Computing Base

Trusted components are:

- the CPU, IOMMU, interrupt remapper, and relevant PCIe isolation hardware;
- Linux VFIO, iommufd, IPC, process-isolation, and memory-management paths;
- the native VFIO broker and its protocol validation;
- the Wasm runtime's sandbox implementation;
- packet/control consumers for validating data received from the driver.

The Wasm driver, Wi-Fi firmware, assigned device, DMA arena, network input, and
all driver-produced messages are untrusted.

## Required Invariants

- VFIO no-IOMMU mode is never supported.
- The Wi-Fi function and any companion functions in its IOMMU group must be
  exclusively claimed; all are considered assigned hardware.
- Only the broker holds VFIO device and iommufd descriptors.
- The broker creates all IOMMU mappings; the worker cannot request arbitrary
  virtual-address mappings.
- Only dedicated DMA-arena pages are mapped for device DMA.
- Broker heaps, stacks, Wasm memory, network-service memory, and secrets are
  never mapped into the device IO address space.
- Trusted allocation metadata remains outside the DMA arena.
- Every native boundary validates handles, integer overflow, offsets, lengths,
  alignment, BAR bounds, message types, and state transitions.
- Native pointers and file descriptors are never exposed to Wasm.
- Consumers treat packet lengths, firmware events, and ring metadata as hostile.

## Isolation Layers

The Wasm driver runs without WASI, filesystem access, sockets, or ambient host
capabilities. It receives only the device-specific host ABI, bounded memory,
and bounded execution using fuel or epoch interruption.

The Wasm runtime runs in a separate unprivileged worker process. A runtime escape
therefore reaches only a seccomp-filtered process with broker IPC, not VFIO.
Namespaces, Landlock, cgroups, closed inherited descriptors, and resource limits
provide additional containment.

The broker is a small memory-safe native process. Its sandbox permits only the
fixed VFIO/iommufd setup, BAR access, event notification, memory management, and
IPC required by the design. After initialization it exposes no general DMA-map
operation. It does not decide whether device commands are semantically safe.

The IOMMU is the final boundary against malicious DMA from either driver or
firmware. DMA buffers are untrusted and preferably copied across the network
service boundary. Shared zero-copy memory must never contain trusted state.

## Expected Failure Behavior

A compromise may corrupt Wasm memory and DMA buffers, disclose packet contents,
control the assigned device, disrupt networking, consume its resource quota, or
require a device or machine reset when hardware reset is unreliable.

It must not grant access to arbitrary syscalls, host memory, kernel memory,
other processes, other PCI devices, IOMMU configuration, or persistent host
storage. Crashes and timeouts terminate the worker; recovery starts with device
reset and fresh zeroed state.

## Out of Scope

This model does not defend against compromised CPU/IOMMU hardware, platform
firmware, physical attacks, PCIe isolation failures, side channels, denial of
service within configured limits, or vulnerabilities in a consumer that fails
to validate the explicitly untrusted driver output it accepts.
