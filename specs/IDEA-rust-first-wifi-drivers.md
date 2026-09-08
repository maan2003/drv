# IDEA-rust-first-wifi-drivers: Native Rust Wi-Fi drivers (adopted)

## Status

The project owner adopted native Rust drivers with strong process sandboxing,
not Wasm. The governing architecture is now [ARCH-drv](ARCH-drv.md),
[ARCH-hardware-isolation](ARCH-hardware-isolation.md), and
[ARCH-wlan-stack-topology](ARCH-wlan-stack-topology.md). This is not an
unfinalized alternative to those records.

Preserve Linux's hardware knowledge, not its kernel integration scaffolding:
firmware protocols, descriptors, rings, and lifecycle ordering are ported to
Rust; Fuchsia supplies WLAN state machines and Netstack3. Pinned C can remain
an offline behavioral oracle, not the production driver. Device firmware blobs
remain untrusted device code and are unaffected by the host language choice.

The Asterinas/OSTD inspiration concerns typed, direction-aware DMA ownership,
not a dependency on a kernel implementation. Its relevant resource distinctions
are preserved in [ARCH-hardware-isolation](ARCH-hardware-isolation.md).
