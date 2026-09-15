# Source and provenance map

This crate owns the sandboxed native network service described by
`ARCH-network-service` and is `GPL-2.0-only` as a combined work.

| Responsibility | Source ownership | Disposition |
| --- | --- | --- |
| Netstack3 Ethernet, DHCP, DNS, TCP and socket provider | Pinned Fuchsia `netstack3-port-integration` and project `netstack3-port-spike` | Calls the pinned portable cores; native capability bindings are project-owned |
| NSS resolver endpoint | Project-local safe `resolver.rs`, shared `drv-dns-wire`, and separate Rust `nss-drv` cdylib | Bounded local requests use existing Hickory/Netstack3 runtime; unsafe glibc pointer adapter is isolated in `nss-drv/src/ffi.rs` |
| SOCKS5 application handoff | Project-local service formerly in `wlan-softmac-host/src/ethernet.rs` | Moved without protocol behavior changes |
| Self-sandboxing READY/GO to NETWORK_READY/SERVE state machine | Project-local service formerly in `wlan-softmac-host/src/netstack_child.rs` | Readiness now proves initialized core, loopback, registration namespace and sandbox while external links may remain offline |
| Provider and link generation launcher | Project-local `NetworkServiceSupervisor` and `link_control.rs` | Starts a persistent kernel provider, then transfers narrowly validated frame descriptors over a private generation-tagged seqpacket channel; legacy SOCKS still uses per-link children |

Pinned Fuchsia closure provenance and its BSD license remain in
`../netstack3-port-spike/upstream-cargo/{PROVENANCE.md,LICENSE.fuchsia}`.
