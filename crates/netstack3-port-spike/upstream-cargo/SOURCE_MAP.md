# Native Netstack3 production source map

Pinned Fuchsia revision: `1e1219e3fac944c9a906aea9646939746b6062b3`.

The fetch script downloads every path below directly from the pinned revision.
The Cargo overlay supplies build metadata only. Unless a patch is named, source
files are byte-for-byte upstream.

| Native package / responsibility | Pinned Fuchsia production source | Host modification | License |
|---|---|---|---|
| `dhcp-protocol`: DHCP message, option, serialization and size-constrained types | `src/connectivity/network/dhcpv4/protocol/src/{lib,size_constrained,size_of_contents}.rs` | Cargo manifest only | Fuchsia BSD-2-Clause |
| `dhcp_client_core`: complete client state machine, transitions, retransmission/jitter, lease phases, parsing and abstract dependencies | `src/connectivity/network/dhcpv4/client/core/src/{client,deps,inspect,lib,parse}.rs` | `dhcp-client-core-host.patch` gates only the `fuchsia_async::MonotonicInstant` convenience import/impl; traits and algorithms unchanged | Fuchsia BSD-2-Clause |
| `trust-dns-proto`: DNS protocol and transport state | `third_party/rust_crates/forks/trust-dns-proto-0.22.0/**` | `trust-dns-workspace.patch` adds only the enclosing Cargo workspace pointer | MIT OR Apache-2.0; original notices fetched with source |
| `trust-dns-resolver`: resolver cache, retry, name-server ordering and lookup lifecycle | `third_party/rust_crates/forks/trust-dns-resolver-0.22.0/**` | `trust-dns-workspace.patch` adds only the enclosing Cargo workspace pointer | MIT OR Apache-2.0; original notices fetched with source |
| `wlan-statemachine` and `wlan-statemachine-macro`: portable state transition DSL used by client SME/RSN | `src/connectivity/wlan/lib/statemachine/{src,macro/src}/**` | Cargo manifests only; upstream unit tests retained; Cargo doctests disabled because upstream illustrative fragments are intentionally non-standalone and GN does not compile them | Fuchsia BSD-2-Clause |
| Native Ethernet, device, route, UDP and TCP bindings | `src/connectivity/network/netstack3/src/bindings/{devices,routes,socket,timers,time,waker}.rs` as semantic reference; production core APIs under `netstack3/core/**` are linked directly | Native capability adapter remains custom because FIDL, Zircon handles/signals, fuchsia-async, netdevice FIDL, Inspect and component lifecycle have no authority-free host implementation | Local MIT OR Apache-2.0 adapter; linked Fuchsia core BSD-2-Clause |
| DHCP configuration policy | `src/connectivity/policy/netcfg/src/dhcpv4.rs` | Adapted in cargo/port-integration/src/service.rs at the effect boundary only: address, route and DNS ownership are applied through native Netstack3 APIs instead of interfaces-admin/routes-admin/LookupAdmin FIDL | Fuchsia BSD-2-Clause |
| DNS source policy | `src/connectivity/policy/netcfg/src/dns.rs` | Adapted by the DHCP effect and NativeDnsBridge configuration boundary; FIDL watcher plumbing is excluded | Fuchsia BSD-2-Clause |

## Replacement rule

The upstream DHCP state machine/protocol and Trust-DNS resolver are the sole
production implementations. Native code may implement their socket, clock,
RNG, spawning and configuration-effect traits, but must not retain a parallel
codec, retry/cache, lease state machine, or service lifecycle.

The former custom control-plane module, its Edge-DHCP/Hickory dependencies, and the frozen service prototype were removed after the pinned upstream adapters landed.

### Native DNS bridge mapping

| Adapter symbol | Upstream contract reused unchanged |
|---|---|
| NativeDnsTime | trust-dns-proto Time |
| NativeUdp | trust-dns-proto UdpSocket |
| NativeTcp | trust-dns-proto DnsTcpStream and Connect |
| NativeSpawn / NativeDnsRuntime | trust-dns-resolver Spawn and RuntimeProvider |
| NativeDnsBridge configure / lookup_ip | trust-dns-resolver AsyncResolver and NameServerConfigGroup |

The bridge queue is bounded to 64 commands and each receive queue to 64
datagrams. It owns no resolver algorithm: caching, retry, server ordering,
truncation detection, and TCP fallback execute in the pinned resolver.

### Remote application socket provider mapping

| Native boundary | Pinned Fuchsia semantic source |
|---|---|
| nonblocking accept / WouldBlock | netstack3/src/bindings/socket/stream.rs AcceptError to_errno |
| connect, bind, listen and connection errors | netstack3/src/bindings/socket/stream.rs error mappings |
| readable/writable edge model | netstack3/src/bindings/socket/event_pair.rs |
| bounded datagram receive and error delivery | netstack3/src/bindings/socket/queue.rs and datagram.rs |
| TCP application buffers and readiness | bindings/socket/stream.rs and production ReceiveBuffer / SendBuffer |
| worker lifetime / handle revocation reference | netstack3/src/bindings/socket/worker.rs |

RemoteSocketProvider deliberately stops before FIDL, Zircon eventpairs, fd
tables, Linux errno conversion, and socket-option policy. NativeSocketProvider
adds only non-reused capability IDs, per-client quotas, revocation, and bounded
readiness staging around the production Runtime handles. Configuration and
filter administration are separate traits and cannot be reached through an
application socket capability.

### Filter and configuration administration

NativeFilterRules exposes the pinned netstack3_filter Routines types without
translation. Runtime implements PacketFilterAdmin by calling the pinned
FilterApi set_filter_state in netstack3/core/filter/src/api.rs, preserving its
cycle validation, hook ordering, NAT activation, matcher and action semantics.
Runtime configuration continues to call production device and RoutesApi
operations; the separate admin traits are not implemented by
NativeSocketProvider.

### Kernel-provider framing

provider_transport.rs preserves the experimental incompatible v1 framing oracle.
provider_transport_v2.rs is the kernel-provider contract: it retains the 40-byte
header, immutable namespace/client identity, 64 KiB payload bound and strict
version rejection while adding unambiguous POSIX operation results, sequenced
level readiness, source addresses, EOF/errors and directional shutdown. Golden
bytes and cross-version rejection are compatibility oracles. Loopback selection is tested
separately: IPv4 127/8 and IPv6 ::1 stay private; all other destinations select
Netstack3.
