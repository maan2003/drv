# Source and provenance map

This crate owns the chip-independent side of the client SoftMAC boundary
specified by `ARCH-wlan-stack-topology`.

The crate as a combined work is `GPL-2.0-only`: extracted project-local files
retain the license of their original GPL crates. The pre-existing portable
contract in `src/lib.rs` remains individually available under its SPDX header;
this extraction does not relicense any moved or copied implementation.

| Host responsibility | Source ownership | Disposition |
| --- | --- | --- |
| Synchronous `WlanSoftmac` downcalls and paired lifecycle/upcalls | Pinned Fuchsia `fuchsia.wlan.softmac` method semantics and `wlansoftmac/rust_driver` lifecycle ordering | Project-owned portable Rust contracts; FIDL/Zircon transport omitted |
| Client MLME/SME/RSN owner, timers, request/event draining and connect result | Pinned Fuchsia `wlan-mlme`, `wlan-sme`, and transitive `wlan-rsn`; extracted from the previously composed host loop | Calls pinned upstream libraries directly; chip RX/lifecycle is injected through `ClientRuntimeDriver` |
| Ethernet `SOCK_SEQPACKET` frame seam and lifecycle | Project-local host integration, formerly in `mt7921-softmac-adapter/src/ethernet.rs` | Moved verbatim apart from chip-neutral names; adapter retains compatibility re-exports |
| DHCP/DNS/TCP proof and SOCKS5 service | Pinned `netstack3-port-integration` and project `netstack3-port-spike`; formerly in `mt7921-softmac-adapter/src/ethernet.rs` | Moved with the Ethernet owner; protocol implementations remain in their source crates |
| Self-sandboxing netstack READY/GO to NETWORK_READY/SERVE state machine | Project-local service, formerly in `mt7921-passive-scan/src/netstack_child.rs` | Copied into the reusable host owner; the installed device binary is deliberately not thinned in this cut |

Pinned Fuchsia closure provenance and its BSD license remain in
`../netstack3-port-spike/upstream-cargo/{PROVENANCE.md,LICENSE.fuchsia}`.
