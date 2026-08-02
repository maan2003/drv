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
| Native Ethernet, device, route, UDP and TCP bindings | `src/connectivity/network/netstack3/src/bindings/{devices,routes,socket,timers,time,waker}.rs` as semantic reference; production core APIs under `netstack3/core/**` are linked directly | Native capability adapter remains custom because FIDL, Zircon handles/signals, fuchsia-async, netdevice FIDL, Inspect and component lifecycle have no authority-free host implementation | Local MIT OR Apache-2.0 adapter; linked Fuchsia core BSD-2-Clause |
| DHCP configuration policy | `src/connectivity/policy/netcfg/src/dhcpv4.rs` | To be adapted at the effect boundary only: address, route and DNS ownership are applied through native Netstack3 APIs instead of interfaces-admin/routes-admin/LookupAdmin FIDL | Fuchsia BSD-2-Clause |
| DNS source policy | `src/connectivity/policy/netcfg/src/dns.rs` | To be adapted at the source ownership boundary only; FIDL watcher plumbing is excluded | Fuchsia BSD-2-Clause |

## Replacement rule

The upstream DHCP state machine/protocol and Trust-DNS resolver are the sole
production implementations. Native code may implement their socket, clock,
RNG, spawning and configuration-effect traits, but must not retain a parallel
codec, retry/cache, lease state machine, or service lifecycle.

The former custom `control_plane.rs` and the frozen `service.rs` prototype
are migration inputs only and are removed as their upstream-backed adapters
land.
