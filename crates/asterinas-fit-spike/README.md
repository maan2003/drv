# Asterinas fit spike

This spike evaluates whether Asterinas can reduce the work needed for isolated
Rust Wi-Fi drivers, a Netstack3 network service, or Linux application
compatibility. It is reference material, not an architecture decision.

## Pinned source

`scripts/fetch-asterinas-reference` downloads a source archive without Git
history at commit `2e2b3468f07815be2c372fd5cd103bb37664ad5c` from
[`asterinas/asterinas`](https://github.com/asterinas/asterinas). The source is
MPL-2.0; copied or modified files would retain that file-level copyleft.

## Architecture

Asterinas is a Linux-ABI-compatible Rust kernel built on its OSTD framework. Its
framekernel keeps the OS framework as the only unsafe Rust boundary and requires
kernel services, including drivers and networking, to use safe Rust. The kernel
and services still share one privileged address space; the isolation boundary
is language soundness rather than a userspace process boundary.

Its current network implementation wraps a fork of `smoltcp` in
`aster-bigtcp`. Linux socket syscalls and file-descriptor behavior live above
that wrapper in the kernel. A safe `aster-network` component adapts network
drivers to `smoltcp` device tokens, and the current physical backend is
virtio-net.

## Useful reference areas

### Linux socket compatibility

Asterinas has already separated much of the difficult Linux socket surface from
its protocol engine. Its syscall and `FileLike` layers implement socket creation,
bind, connect, listen, accept, send/receive variants, socket options, blocking,
polling, epoll integration, shutdown, and descriptor behavior above
`aster-bigtcp`.

The code is coupled to Asterinas kernel types and the `aster-bigtcp` API, so it
is not a drop-in Netstack3 binding. It is nevertheless a valuable reference for
the host adapter described by
[`ARCH-network-service`](../../specs/ARCH-network-service.md), especially for
mapping Linux FD readiness and lifecycle onto a remote userspace socket. Its C
regression suite and imported gVisor conformance tests are also useful sources of
socket behavior tests independent of smoltcp.

### Safe hardware interfaces

The exact pinned-source comparison and staged convergence plan are in
[`DMA-COMPARISON.md`](DMA-COMPARISON.md).

OSTD and the safe virtio drivers demonstrate several patterns relevant to
[`IDEA-rust-first-wifi-drivers`](../../specs/IDEA-rust-first-wifi-drivers.md):

- `DmaCoherent` versus direction-typed `DmaStream` buffers;
- explicit cache synchronization ranges;
- typed device addresses distinct from CPU addresses;
- rights-typed `SafePtr` access to DMA and MMIO structures;
- safe PCI BAR, MSI-X, interrupt, and callback APIs; and
- `#![deny(unsafe_code)]` across drivers and the network component.

These designs should be the baseline for the native broker and Rust Wi-Fi
driver APIs. In particular, this project should preserve the coherent/streaming
split, direction types, explicit range synchronization, typed device addresses,
owned mapping lifetimes, and typed memory views. The pinned x86 OSTD code does
not yet flush IOTLBs on unmap, so its drop path is lifetime ownership rather
than proof of immediate revocation. VFIO/iommufd can implement the stronger
contract today; the deterministic broker and a future Asterinas host can be
alternative backends. Pulling OSTD in as a Linux-userspace dependency would not
help: its implementation assumes an OSTD-based kernel, global machine
ownership, interrupt contexts, and its own memory manager.

### Linux application testing

Asterinas NixOS demonstrates a NixOS userland running on a non-Linux kernel and
the repository has broad syscall, socket, epoll, network, and application
testing. This is evidence that Linux application compatibility can be evaluated
independently of the protocol stack. Its test corpus may help validate a future
host socket adapter, subject to MPL-2.0 provenance.

## Mismatches with this project

### It cannot host the M2 target

Asterinas can be developed on ARM64 hosts but has no ARM64 deployment target.
The current kernel targets x86-64, RISC-V 64, and experimental LoongArch 64; its
roadmap remains centered on x86-64 VM and virtio use. It has none of the Apple
Silicon platform, DART, PCIe, power, or BCM4387 support required by
[`ARCH-asahi-wifi-target`](../../specs/ARCH-asahi-wifi-target.md).

### It is not a VFIO userspace-driver framework

The source has no VFIO or iommufd implementation. Drivers execute in the kernel
address space against OSTD's direct PCI, DMA, MMIO, and interrupt APIs. Adopting
Asterinas would therefore replace the Linux host kernel and move the Rust Wi-Fi
driver back into a kernel service, rather than support the current Linux-hosted
userspace milestone.

### Its isolation goal is weaker in the dimensions required here

OSTD aims to prevent undefined behavior even if safe kernel services or devices
behave unexpectedly. This is valuable, but it is not capability isolation
between mutually hostile services. The current x86 IOMMU implementation gives
all enumerated PCI functions shallow copies of one second-stage page table, so a
DMA mapping is not a private per-device domain. That does not meet
[`REQ-isolation`](../../specs/REQ-isolation.md), which requires assigned devices
to reach only dedicated broker-owned arenas and not unrelated devices or memory.

### Netstack3 is not a direct substitution

Asterinas's approximately 20,000 lines of network, socket, and smoltcp-wrapper
code use `aster-bigtcp` types directly; there is no protocol-stack backend trait
that can simply select Netstack3. Replacing `aster-bigtcp` while preserving its
surface is conceivable, but it would be a substantial adapter. Netstack3 core
also contains some unsafe Rust for synchronization and unchecked invariant
construction, conflicting with Asterinas kernel crates' blanket
`#![deny(unsafe_code)]` unless that code were removed, audited into OSTD, or
isolated behind a separately accepted boundary.

## Assessment

Asterinas should remain a reference rather than a dependency or host kernel for
the current milestone, but it is a strategic architectural upstream rather than
a disposable code sample. Its highest-value contributions are:

1. patterns for safe DMA, MMIO, PCI, and interrupt APIs for Rust drivers;
2. a concrete Linux socket syscall and FD compatibility implementation; and
3. socket, epoll, syscall, and real-application compatibility tests.

Long term, an Asterinas backend—or Asterinas itself as the host kernel—could
unify these safe hardware abstractions with Linux ABI compatibility. That path
becomes credible if Asterinas gains ARM64/Apple platform support, private
per-device IOMMU domains, and a service isolation model that satisfies this
project's threat model. A separate experiment could replace its smoltcp backend
with Netstack3 on x86-64, proving Linux ABI applications over Netstack3 without
a Linux kernel. Neither experiment advances the immediate M2 Wi-Fi milestone,
so they should not displace the smaller Linux-hosted `BindingsCtx` and VFIO
work.
