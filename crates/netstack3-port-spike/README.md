# Netstack3 host-portability spike

This spike tests whether Fuchsia's Netstack3 can supply the portable network
service described by [ARCH-network-service](../../specs/ARCH-network-service.md).
It is not an adoption decision and it contains no MT7921 or other hardware code.

## Reproducible source and license audit

The repository's `scripts/fetch-fuchsia-reference` fetches Gitiles archives at
Fuchsia commit `1e1219e3fac944c9a906aea9646939746b6062b3`, records each archive's
SHA-256, and expands the ignored tree under `reference/`. The audit below used
that mechanism. At this pin:

- the Netstack3 archive is 1.57 MiB compressed and 12 MiB expanded;
- all fetched references are 59 MiB expanded;
- Netstack3 core contains 163,854 lines of Rust after excluding the separate
  integration-test, fuzz, and `teststd` trees;
- the aggregate core crate's own GN-listed production sources are 10,121 lines;
- Fuchsia's GN-listed production bindings sources are 44,474 lines; and
- none of the relevant first-party archives contains a `Cargo.toml`.

Fuchsia source is under the BSD 2-Clause license in the repository-root
`LICENSE`; individual source headers refer to that file. The Cargo overlay
therefore carries the exact pinned root license as `upstream-cargo/LICENSE.fuchsia`
and records its origin in `upstream-cargo/PROVENANCE.md`. No Fuchsia source is
copied into this crate; `prepare-upstream` applies packaging metadata to the
ignored, pinned reference tree.

## Smallest portable closure found

Netstack3 deliberately separates a functional protocol core from platform
bindings. `docs/CORE_BINDINGS.md` says the core is platform-agnostic and that a
binary supplies the outside world as trait implementations. Production core
code has no unconditional Zircon, FIDL IPC, filesystem, or device access.
Fuchsia tracing has a non-Fuchsia implementation; the `fuchsia_async` and
Inspect uses found in core are target-gated test code.

The useful adoption unit is nevertheless the **aggregate core**, not just its
Ethernet and ICMP directories. Its production GN target unconditionally depends
on these protocol crates:

`base`, `datagram`, `device`, `filter`, `hashmap`, `icmp_echo`, `ip`,
`lock-order`, `macros`, `sync`, `tcp`, `trace`, and `udp`.

Together with the aggregate crate that is 14 first-party crates. Their portable
Fuchsia-library closure includes `net-types` (and proc macro),
`packet-formats`, `internet-checksum`, `packet`, `diagnostics-traits`,
`explicit`, and `replace-with`, plus ordinary crates.io dependencies. The
checked-in Cargo overlay packages exactly this closure. Its FIDL-free
`net-declare` facade exposes the literal macros and portable network types used
by core without importing generated Fuchsia network FIDL types. Versions are
locked and `core/build.rs` reproduces the aggregate GN target's
`cfg(no_lock_order)` setting.

The required host binding is a concrete context implementing the aggregate
marker traits over smaller responsibilities: monotonic time and timers,
randomness, owned packet buffers and device TX, device events, socket buffers
and readiness, reference-lifetime notifications, filtering metadata, and
diagnostics. Fuchsia's 44,474-line binding additionally owns FIDL socket,
route, interface and netdevice services plus Zircon async behavior. That shell
is not part of the portable closure.

## Protocol ownership at the pin

| Capability | Location |
| --- | --- |
| Ethernet and loopback/pure-IP devices | core (`device` and aggregate device API) |
| IPv4 and IPv6, fragmentation and forwarding | core (`ip`) |
| ARP | core (`device`) |
| IPv6 NDP/NUD, DAD, SLAAC and router discovery | core (`ip` device logic) |
| route tables, rules and multicast routing | core (`ip` and aggregate API) |
| ICMPv4/ICMPv6 and ICMP echo sockets | core (`ip`, `datagram`, `icmp_echo`) |
| UDP | core (`udp` plus shared `datagram`) |
| TCP state machine and socket state | core (`tcp`); POSIX/FIDL descriptors are bindings |
| DHCPv4 client and address policy | separate Fuchsia service; `main.rs` calls it out as out-of-stack |
| DNS configuration and name resolution | separate services (`netcfg`/name lookup), not core; Netstack3's DNS watcher deliberately does not serve results |

DHCP and DNS therefore remain explicit service dependencies even after a core
port. This crate packages sans-I/O adapters for them: `Dhcpv4Client` uses
`edge-dhcp` for DISCOVER/OFFER/REQUEST/ACK, and `DnsCodec` uses `hickory-proto`
for A/AAAA exchanges. Both bound all datagrams and open no sockets. A deployment
binding must send DHCP on UDP 68 -> 67, apply the accepted
address/route and DNS servers through Netstack3's APIs, and send DNS queries to
an explicitly configured server on UDP 53. It must also supply entropy, elapsed
time, retry policy and TCP fallback when required; importing core alone still
does not configure an interface or provide a resolver service.

## Integration boundary

The safe native-Rust data-plane contract is the `EthernetDevice` trait in this
crate:

```text
Wi-Fi Ethernet boundary -> owned EthernetFrame -> Netstack3 host binding
Wi-Fi Ethernet boundary <- owned EthernetFrame <- Netstack3 host binding
```

`EthernetFrame` owns exactly 14 through 1514 bytes (Ethernet II without FCS,
initially no VLAN, 1500-byte MTU). Both fake-device queues have an explicit item
bound and return frame ownership on backpressure. The trait exposes no file,
path, descriptor, ioctl, TAP handle, hardware handle, clock, executor, or random
source. Configuration, monotonic timers, entropy and socket readiness are
separate injected capabilities. `EthernetEventSource` adds link, receive-ready,
and returned-transmit-credit events without exposing an OS handle, while
`EthernetRunner` retains at most one frame in each direction during
backpressure. This makes the
frame edge usable in-process without granting the protocol engine ambient
hardware or filesystem authority and keeps it compatible with
[REQ-host-portability](../../specs/REQ-host-portability.md).

A Linux TAP adapter may later be placed **outside** this contract for temporary
first-connectivity testing. It is not the target binding and must not leak a
file descriptor or Linux type into portable code. The target path is direct
owned-frame exchange with the Wi-Fi driver's Ethernet boundary.

## Reproduce the executable proof

`FakeEthernetDevice` deterministically demonstrates:

- ingress and transmit transfer owned, lossless Ethernet frames in FIFO order;
- an ARP-EtherType fixture and an IPv4-EtherType fixture remain opaque to the
  device boundary;
- short and oversized frames are rejected before crossing it; and
- bounded queues return ownership rather than allocate without limit or block.

The package tests also round-trip DHCPv4 acquisition messages and DNS queries
without ambient I/O. Run these boundary tests with:

```sh
cargo test -p netstack3-port-spike
```

The pinned upstream aggregate core, its `testutils` variant, and the actual
upstream protocol tests are reproduced with:

```sh
./crates/netstack3-port-spike/prepare-upstream
```

That command fetches the exact source pin if needed, overlays the checked-in
Cargo metadata, performs locked production and `testutils` checks, then runs
five tests through upstream core APIs behind this crate's owned-frame boundary:

- Ethernet ARP resolution, IPv4 route selection, and queued UDP transmission;
- Ethernet IPv4 ICMP echo request and reply;
- IPv6 NDP neighbor solicitation/advertisement and UDP transmission;
- a real upstream TCP loopback handshake followed by payload receive; and
- DHCPv4 acquisition over bounded Ethernet, application of the accepted
  address and on-link route to Netstack3, DNS request/response over Netstack3
  UDP, and a TCP handshake plus payload between two Ethernet-attached stacks.

The overlay also builds `NativeBindingsCtx` against production core with its
`testutils` feature disabled. This standalone context supplies injected time and
entropy, budgeted timers, non-panicking reference notifiers, bounded frame,
event and UDP queues, TCP buffers/readiness, and device dispatch. Its focused
tests run in addition to the five protocol tests above (the older protocol
fixtures still use upstream's fake context as an oracle).

`Runtime` is the narrow production-facing owner. It creates and enables an
explicit Ethernet interface, applies/revokes IPv4 addresses, atomically replaces
on-link/default route sets with `RoutesApi::set_routes`, and exposes bounded opaque
UDP and TCP handles. Pre-lease DHCP uses a private Netstack3 device socket, so
0.0.0.0/broadcast traffic crosses core's FIFO and normal TX backpressure;
accepted leases install the address, routes and DNS server set. A deterministic
two-runtime test then resolves DNS over native UDP. Separate native tests prove
ARP, bidirectional UDP, TCP connect/listen/accept/read/write/shutdown, socket
quotas, stale handles, and FIN exchange over owned Ethernet frames.

This establishes a usable synchronous native userspace stack without Fuchsia
platform bindings. The embedding remains responsible for driving time/frame
polls and DHCP renewal/rebind/expiry and DNS retry/cache policy; the supplied
lease deadlines and truncation signal make those policies explicit. IPv6 is
compiled and protocol-tested but the narrow `Runtime` socket facade is
currently IPv4-only. The final deployment-specific step is implementing the
existing frame/readiness contract at the Wi-Fi Ethernet boundary. No TAP device
or MT7921 code is involved.
