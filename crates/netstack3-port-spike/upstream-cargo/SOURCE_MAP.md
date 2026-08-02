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
| `wlan-bitfield` and `wlan-bitfield-wrapper`: 802.11/EAPOL bitfield generator and its upstream conformance tests | `src/connectivity/wlan/lib/bitfield/{src,wlan-bitfield-tests/src}/**` | Cargo manifests only; all 14 upstream tests retained; Cargo doctests disabled because GN does not compile the illustrative fragments | Fuchsia BSD-2-Clause |
| `ieee80211`: SSID, MAC address, BSSID, parsing and formatting | `src/connectivity/wlan/lib/ieee80211/src/**` | Cargo manifest only; all 38 upstream unit tests unchanged; Cargo doctest disabled because GN does not compile the non-standalone illustrative fragment | Fuchsia BSD-2-Clause |
| `fidl_fuchsia_wlan_common` host subset | `sdk/fidl/fuchsia.wlan.common/{driver_features,wlan_common}.fidl` | Narrow schema binding replacement exports WLAN common's pinned bounds and TX-vector sentinel, flexible MAC/data-plane/implementation enums, and feature-support tables consumed by WLAN common and SoftMAC MLME; exact discriminants, unknown-value round trips, and table defaults are tested; no transport or policy | Fuchsia BSD-2-Clause |
| `fidl_fuchsia_wlan_driver` host values | `sdk/fidl/fuchsia.wlan.driver/types.fidl` | Path-shaped schema binding exports the complete pinned join, key, WMM, and SoftMAC capability value model consumed by SoftMAC MLME; IEEE value dependencies, table optionality, strict discriminants, bit values, and field widths are preserved and tested; no protocol transport | Fuchsia BSD-2-Clause |
| `fidl_fuchsia_wlan_ieee80211` host subset | `sdk/fidl/fuchsia.wlan.ieee80211/{channel,constants,fields,reason_code,rsn,status_code}.fidl` | Narrow schema binding replacement exports the pinned bounds, C SSID, MAC address/BSS description, channel/band/access-category/BSS/PHY/reason/status/key/cipher enums, channel number, and fixed-size HT/VHT records required by WLAN common, FCG crypto, and SoftMAC/driver values; exact known-value validation, explicit unknown-value construction, and record lengths are tested; no policy | Fuchsia BSD-2-Clause |
| `fidl_fuchsia_wlan_internal` host subset | `sdk/fidl/fuchsia.wlan.internal/security.fidl` | Narrow schema binding replacement exports only the protocol, authentication, and WEP/WPA credential value types required by WLAN common security conversion; exact protocol discriminants, credential field shapes, boxed optional union, and opaque unknown union payload retention are tested; no service transport or authentication policy | Fuchsia BSD-2-Clause |
| `fidl_fuchsia_wlan_mlme` host subset | `sdk/fidl/fuchsia.wlan.mlme/{wlan_mlme,wlan_mlme_ext}.fidl` | Narrow schema binding replacement exports only the per-band capability/device-information records plus EAPOL result and SAE frame value types consumed by WLAN common and RSN; boxed optional HT/VHT records, exact result discriminants, and IEEE/common schema dependencies are preserved and tested; no MLME protocol transport or policy | Fuchsia BSD-2-Clause |
| `fidl_fuchsia_wlan_minstrel` host values | `sdk/fidl/fuchsia.wlan.minstrel/wlan_minstrel.fidl` | Path-shaped schema binding exports the complete pinned peer list and per-rate Minstrel statistics value model consumed by SoftMAC MLME; integer widths, floating-point estimates, MAC addresses, strings, and vectors are preserved and tested; no protocol transport | Fuchsia BSD-2-Clause |
| `fidl_fuchsia_wlan_sme` host subset | `sdk/fidl/fuchsia.wlan.sme/sme.fidl` | Narrow schema binding replacement exports only the protection, compatibility, radio configuration, and scan-result value types consumed by WLAN common; exact protection discriminants and common/internal/IEEE field shapes are tested; VMO handles, persistence codecs, protocols, and SME policy are excluded | Fuchsia BSD-2-Clause |
| `fidl_fuchsia_wlan_softmac` host values | `sdk/fidl/fuchsia.wlan.softmac/{features,softmac,tx}.fidl` | Path-shaped schema binding exports the pinned discovery/query, RX/TX metadata and result, key/association, scan, beacon/channel/WMM, packet, and FFI transfer value types consumed by SoftMAC MLME; flag masks, flexible result codes, table optionality, fixed attempt arrays, and cross-schema dependencies are preserved and tested; protocol proxies and driver policy remain excluded | Fuchsia BSD-2-Clause |
| `fidl_fuchsia_wlan_stats` host values | `sdk/fidl/fuchsia.wlan.stats/wlan_stats.fidl` | Path-shaped schema binding exports the complete pinned histogram, interface/connection counter, signal-report, and telemetry-support value model; strict/flexible discriminants, nullable boxes, table optionality, sparse vectors, and bounds are preserved; no protocol transport or persistence | Fuchsia BSD-2-Clause |
| `wlan-frame-writer-macro`: frame/header/IE construction macros used by SoftMAC MLME | `src/connectivity/wlan/lib/frame_writer/macro/src/**` | Cargo manifest only; source unchanged; tests arrive with the upstream frame-writer consumer after WLAN common is packaged | Fuchsia BSD-2-Clause |
| `wlan-frame-writer`: complete management/data header and information-element serialization used by SoftMAC MLME | `src/connectivity/wlan/lib/frame_writer/src/**` | Cargo manifest plus `wlan-frame-writer-host.patch`: substitutes an owned zeroed vector only for the Fuchsia driver-arena allocation/ownership boundary; macro parsing, length calculation, field/IE serialization, fixed-slice and append paths are unchanged; all upstream unit tests retained | Fuchsia BSD-2-Clause |
| `fuchsia-trace` host boundary and `wlan-trace` | `src/lib/trace/rust/src/lib.rs` and `src/connectivity/wlan/lib/trace/src/**` | The pinned Fuchsia trace runtime remains the semantic/API reference; a path-shaped local host binding preserves trace IDs, argument/event signatures, and macros but deliberately disables emission where Fuchsia's trace engine is absent. `wlan-trace-host.patch` generalizes only the host TX-status display boundary; pinned WLAN event names and event calls are unchanged | Fuchsia BSD-2-Clause reference; local host binding MIT OR Apache-2.0 |
| `zx-types`, `zx-status`, and host `zx` value facade | `sdk/rust/{zx-types,zx-status,zx}/src/**` | Exact pinned `zx-types` and `zx-status` sources are packaged unchanged. A path-shaped local `zx` facade re-exports their status/raw value API and preserves pinned monotonic instant/duration units, conversions, ordering, and saturating arithmetic using the host monotonic clock, without exposing unavailable Zircon syscalls or handles | Fuchsia BSD-2-Clause reference and value crates; local facade MIT OR Apache-2.0 |
| Generic FIDL responder support and `wlan-fidl-ext` | `src/lib/fidl/rust/fidl/src/error.rs` and `src/connectivity/wlan/lib/fidl-ext/src/**` | A local host `fidl` boundary exposes only the pinned `InvalidHeader` error contract; transport/encoding remain excluded. `wlan-fidl-ext-host.patch` gates only generated Fuchsia protocol responder impls; generic responder error handling and required-field unpacking are unchanged and retain the complete upstream test suite | Fuchsia BSD-2-Clause reference/WLAN source; local host boundary MIT OR Apache-2.0 |
| `wlan-common`: IEEE 802.11 frame/IE parsing and writing, capability derivation, channel/rate-vector logic, BSS classification, SME compatibility, security conversion, and MLME/SME timer queue | `src/connectivity/wlan/lib/common/rust/src/**` | Cargo manifest plus `wlan-common-host.patch` and `wlan-timer-host.patch`: gates only the Zircon-status conversion, consumes the non-Fuchsia scan timestamp, exposes portable test support, enables the unchanged IEEE TimeUnit conversion, and substitutes Tokio sleep only at the fuchsia-async wake-up boundary; event IDs, deadlines, unordered concurrency, cancellation handles, and filtering remain pinned source and have host tests; VMO persistence remains target-gated | Fuchsia BSD-2-Clause |
| `eapol`: EAPOL and EAPOL-Key frame parsing, validation, serialization, and key-information bitfields | `src/connectivity/wlan/lib/eapol/src/lib.rs` | Cargo manifest only; source and all upstream unit tests unchanged | Fuchsia BSD-2-Clause |
| `mundane`: BoringSSL-backed hash, HMAC, key derivation, password and public-key primitives used by WLAN RSN/SAE | `src/lib/mundane/src/**` plus BoringSSL revision `156c7b75ae9b8c3b3f847acf264f17594c3859fb` recorded by the pinned Fuchsia gitlink | Cargo manifests plus a host build script that compiles the exact gitlink revision and uses Fuchsia's generated `bssl-sys` bindings/wrapper; cryptographic source is unchanged | Fuchsia BSD-2-Clause and BoringSSL Apache-2.0 |
| `wlan-fcg-crypto`: finite cyclic group operations and complete SAE/OWE authentication handshakes | `src/connectivity/wlan/lib/fcg-crypto/src/**` | Cargo manifest only; Fuchsia's BoringSSL-backed implementation and unit tests are unchanged | Fuchsia BSD-2-Clause |
| `fuchsia-sync`: mutex, rwlock, and condition-variable primitives used by RSN | `src/lib/fuchsia-sync/src/**` | Cargo manifest only; the pinned source's upstream non-Fuchsia parking_lot backend is selected unchanged | Fuchsia BSD-2-Clause |
| `wlan-rsn`: WPA personal/enterprise RSN association, PSK derivation, EAPOL four-way/group-key handshakes, key wrapping/integrity, SAE, and OWE integration | `src/connectivity/wlan/lib/rsn/src/**` | Cargo manifest plus `wlan-rsn-host.patch`: substitutes a host wall-clock timestamp only at nonce initialization and maps two Fuchsia test attributes to ordinary Cargo tests; security algorithms and state machines are unchanged | Fuchsia BSD-2-Clause |
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

`netstack3-provider-daemon` opens `/dev/netstack3-provider`, validates each
multiplexed identity before dispatch, and routes requests through
ProviderDispatcherV2 and NativeSocketProvider. Its bounded reader channel also
drives monotonic Netstack3 time and emits only changed, sequenced readiness
snapshots. `ethernet_transport.rs` adapts an inherited connected
`SOCK_SEQPACKET` capability to versioned attach, link, and owned Ethernet-frame
messages with bounded queues and one retained transmit frame under backpressure.
The daemon does not open the provider device until attach and link-up complete;
DhcpService, rather than the frame peer or application provider, owns address,
route, and DNS configuration.
