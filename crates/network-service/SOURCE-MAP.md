# Source and provenance map

This crate owns the sandboxed native network service described by
`ARCH-network-service` and is `GPL-2.0-only` as a combined work.

| Responsibility | Source ownership | Disposition |
| --- | --- | --- |
| Netstack3 Ethernet, DHCP, DNS, TCP and socket provider | Pinned Fuchsia `netstack3-port-integration` and project `netstack3-port-spike` | Calls the pinned portable cores; native capability bindings are project-owned |
| SOCKS5 application handoff | Project-local service formerly in `wlan-softmac-host/src/ethernet.rs` | Moved without protocol behavior changes |
| Self-sandboxing READY/GO to NETWORK_READY/SERVE state machine | Project-local service formerly in `wlan-softmac-host/src/netstack_child.rs` | Moved without startup or sandbox behavior changes |
| Process generation launcher | Project-local `NetworkServiceSupervisor` | Transfers only frame/listener/bootstrap capabilities and replaces the sandboxed child when the WLAN owner supplies a new Ethernet generation |

Pinned Fuchsia closure provenance and its BSD license remain in
`../netstack3-port-spike/upstream-cargo/{PROVENANCE.md,LICENSE.fuchsia}`.
