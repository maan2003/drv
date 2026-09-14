# Upstream provenance

Started by copying `crates/server/src/store/forwarder.rs` from Hickory DNS
v0.26.1, commit `f09321075b1f97902b7bc4ca4ffda7816fcf2971`, verbatim.
Review showed its record-oriented zone handler is not a complete-message proxy.
The owned `forward.rs` replaces that abstraction rather than retaining unused
zone/server machinery. UDP/TCP receive-loop structure in `ingress.rs` derives
from the same revision's `crates/server/src/server/mod.rs`. `transport.rs`
adapts `crates/net/src/h2.rs`'s connect/send path. These portions use upstream's
MIT license option (`LICENSE-MIT`); copyright notices are retained.

Intentional differences: admission before spawning; finite frame/body/header
limits; original whole-request deadlines; static endpoints and explicit CA
roots; no ambient resolver; lazy connection establishment; multiplexed H2
connections with generation-scoped invalidation and owned driver lifetime;
HTTP Age handling; full DNS responses and library-assisted truncation.
HTTP/2 and TLS remain h2/tokio-rustls/rustls dependencies, not copied protocols.

Review future Hickory releases for codec and transport fixes. Vendored/copied
logic does not receive upstream fixes merely by updating Cargo.lock.
