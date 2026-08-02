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
`LICENSE`; individual source headers refer to that file. Gitiles path archives
do not contain the repository-root license. No Fuchsia code has been copied into
this crate, so this crate remains `MIT OR Apache-2.0`. Any later source import
must carry the pinned root BSD license, copyright notices, source pin, and local
modifications. An archive plus source headers alone is insufficient licensing
provenance.

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
`explicit`, and `replace-with`, plus ordinary crates.io dependencies. The `ip`
crate also declares `net-declare`: production uses only its `net_*` literal
macros, but `net-declare` unconditionally re-exports generated Fuchsia network
FIDL types. A host package should depend directly on a separated
`net-declare-macros`/network-types-only target rather than importing those FIDL
types.

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
port. Importing core alone does not produce a configured interface or resolver.

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
source. Configuration, monotonic timers, entropy and socket readiness must be
separate injected capabilities when the real context is added. This makes the
frame edge usable in-process without granting the protocol engine ambient
hardware or filesystem authority and keeps it compatible with
[REQ-host-portability](../../specs/REQ-host-portability.md).

A Linux TAP adapter may later be placed **outside** this contract for temporary
first-connectivity testing. It is not the target binding and must not leak a
file descriptor or Linux type into portable code. The target path is direct
owned-frame exchange with the Wi-Fi driver's Ethernet boundary.

## Deterministic proof and exact blocker

The current executable proof is intentionally a **contract scaffold, not
Netstack3 execution**. `FakeEthernetDevice` deterministically demonstrates:

- ingress and transmit transfer owned, lossless Ethernet frames in FIFO order;
- an ARP-EtherType fixture and an IPv4-EtherType fixture remain opaque to the
  device boundary;
- short and oversized frames are rejected before crossing it; and
- bounded queues return ownership rather than allocate without limit or block.

Run it with:

```sh
cargo test -p netstack3-port-spike
```

An upstream ARP/ICMP proof is blocked before binding implementation: the pinned
source checks in GN metadata but **zero Cargo manifests** for the 14-crate
aggregate core and its first-party library closure. Fuchsia normally generates
Cargo metadata from a configured GN build (`fx gen-cargo`); the path archives do
not include the generated output. Manually inventing one manifest for only
`device`/`ip` does not solve this because the supported aggregate target pulls
all protocol crates, and its fake execution context is exposed through
`testutils` variants across that same closure. The `net-declare` production edge
also needs a FIDL-free package split.

The next evidence gate is reproducible checked-in or generated Cargo metadata
for the pinned **production** aggregate closure, preserving its feature variants
and BSD attribution. Only then should this fake device be adapted to Netstack3's
buffer/TX context and used to prove an actual sequence such as Ethernet ARP
request -> core processing -> ARP reply, followed by IPv4 ICMP echo. Until that
test calls upstream core APIs, this spike makes no protocol-support claim.
