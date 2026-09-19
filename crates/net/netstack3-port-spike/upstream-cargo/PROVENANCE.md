# Shared pinned Fuchsia Cargo overlay provenance

The path-preserving manifests and build configuration in this directory package unmodified
Fuchsia source fetched at commit `1e1219e3fac944c9a906aea9646939746b6062b3` by
`../../../../scripts/fetch-fuchsia-reference`. Dependency versions and features
were transcribed from the same commit's GN `BUILD.gn` files and
`third_party/rust_crates/Cargo.toml`.

`src/connectivity/network/netstack3/cargo/net-declare-portable` is a local,
FIDL-free facade over Fuchsia's `net-declare-macros`; it exports only the
`net_types` literal macros used by core. The port-integration test is local
MIT/Apache-2.0 code. Fuchsia source and derived build packaging remain under the
attached BSD 2-Clause `LICENSE.fuchsia`. No source from third-party host-binding
experiments is included.

The overlay models the pinned production and testutils variants. Loom and
benchmark GN variants are deliberately not mapped because Cargo feature
unification cannot represent GN's mutually exclusive dependency targets.

## DHCP and DNS production reuse

`SOURCE_MAP.md` records the file/function-level disposition for the pinned
DHCP core/protocol and the retained Fuchsia Trust-DNS forks. Their original
source and license notices are fetched unchanged. The native DNS runtime now
uses crates.io Hickory resolver/protocol/network 0.26.3, pinned by the integration
manifest and consumer lockfiles (MIT OR Apache-2.0), rather than the old 0.22.0
forks. The Netstack3/Fuchsia revision is unchanged. Host changes are isolated as reviewable
patch files; no algorithm, constant, state transition, cache, or retry policy is
forked into the native adapter.

## Passive TCP storage admission

`tcp-passive-storage-admission-host.patch` adds a fallible, context-bearing
buffer admission hook before a validated SYN publishes connection state or
emits SYN-ACK. An admitted buffer triple stays on the connection until the
final ACK transfers it into established state. Existing bindings default to
their existing constructor, now called at SYN admission; the native binding's
payload storage is lazy and carries one shared-budget lease.

Idle listeners no longer reserve storage for their entire backlog, and accept
does not charge the same connection again. Buffer-size, socket-count, backlog
and total storage limits remain unchanged. Passive handshake timeout removes
the dead accept-queue entry and destroys the connection, matching reset/close
cleanup, rather than retaining capacity until listener closure. The native
tests cover IPv4/IPv6 shared pressure, retry, duplicate SYNs, timeout, reset,
listener closure and retained buffers after handle closure.

## Overlay extension contract

This directory is the shared Cargo overlay for portable code at the project
Fuchsia pin; its location under the Netstack3 spike is historical, not a second
pin boundary. New WLAN common, SoftMAC MLME, SME, RSN, and EAPOL packages:

1. reuse `scripts/fetch-fuchsia-reference` (the default closure already fetches
   all of `src/connectivity/wlan`; pass extra repository paths as arguments);
2. preserve Fuchsia-relative paths below `upstream-cargo/src`;
3. add members and shared dependency versions to the existing workspace root at
   `src/connectivity/network/netstack3/Cargo.toml` and its single lockfile;
4. record production/test source disposition in `SOURCE_MAP.md`; and
5. isolate host-only build changes as named patches without forking protocol or
   state-machine behavior.

The fetch closure includes the `fuchsia.wlan.*` FIDL source namespaces consumed
by these packages. Host bindings must be generated or represented by narrow
path-shaped compatibility crates from those pinned schemas; they must not copy
policy or invent a second protocol definition.

The Fuchsia pin records BoringSSL as gitlink
`156c7b75ae9b8c3b3f847acf264f17594c3859fb`. The fetch script materializes that
exact revision at Fuchsia's expected `third_party/boringssl/src` path; the host
build metadata compiles it with Fuchsia's generated bindings and compatibility
wrapper. BoringSSL remains under its upstream Apache-2.0 license.

`wlan-common-host.patch` gates only the Zircon-status conversion at the existing
platform boundary, retains monotonic scan timestamps on host, makes its portable
fixed-size test buffer available without compiling the Fuchsia-only test helpers,
and reports scan-result VMO persistence unavailable without Fuchsia transport.
The frame, IE, BSS, capability, channel, rate-vector, and security algorithms
remain byte-for-byte pinned Fuchsia source.

The fetch closure also retains Fuchsia's production trace crate at the pin.
The Cargo `fuchsia-trace` package selects the separately licensed
`host/src/lib.rs` only on this non-Fuchsia port: it preserves the WLAN-facing
event API while disabling emission because the Fuchsia trace engine is absent.
Pinned `wlan-trace` event names and calls remain the production implementation.

The closure retains the pinned FIDL runtime and Zircon Rust value/runtime
sources as references for MLME boundaries. Exact `zx-types` and `zx-status`
sources are built unchanged. Separately licensed host facades expose only
status/raw values, monotonic time value semantics, and the FIDL error required by generic responder helpers;
syscalls, handles, encoding, and transport are not emulated.

The pinned `wlan-sme` AP/client policy and state machines compile directly on
the host. `wlan-sme-host.patch` excludes only the generated endpoint-serving
module and two binding-shape compatibility sites. Host responder tokens return
an explicit transport-unavailable error rather than emulating Fuchsia channels.
The Linux `crates/wifi/wlan-softmac-host` serving implementation also derives
from this pin, preserving BSD headers and upstream source paths:
`src/serve.rs` from `drivers/wlansoftmac/rust_driver/src/lib.rs`,
`src/mlme.rs` from `lib/mlme/rust/src/lib.rs`, and `src/sme/{mod,client}.rs`
from `lib/sme/src/serve/{mod,client}.rs` (all below `src/connectivity/wlan`).
Native typed channels replace FIDL/FFI transport; initialization ordering,
MLME/SME event loops, connect transaction forwarding, and supervisor tests
retain the upstream control flow. Hardware ownership and cancellation authority
are Linux binding responsibilities, not copied protocol policy.
`wlan-serving-sinks-host.patch` adds emission-time native sink callbacks,
SME construction with supplied sinks, and a shared Minstrel constructor so
these serving loops can use the pinned state machines without polling adapters.

`wlan-sme-passive-observation-timeout-host.patch` adds one optional initial
RSNA response timeout to `ClientConfig`. Its default preserves the pinned
4000 ms policy; only passive validation requests 6000 ms so its 5000 ms
read-only M1 observer reaches its own boundary first.

The pinned SME uses Fuchsia Inspect only for diagnostics. Separately licensed
path-shaped host facades retain scalar/string/byte property state and the API
surface needed by SME, while disabling Fuchsia-specific hierarchy/VMO encoding
and convenience log emission. SME policy and state transitions remain in the
pinned WLAN source rather than being copied into the diagnostics boundary.

`wlan-timer-host.patch` retains WLAN common's pinned timer queue and replaces
only fuchsia-async's deadline wake-up with the Fuchsia-pinned Tokio timer.
Event identity, concurrent deadline ordering, cancellation, and filtering stay
in upstream source and are exercised by host integration tests.

`nix/fuchsia-reference.json` is the machine-readable owner of the canonical
revision, closure paths, special gitlinks, and normalized provider-package
archive hashes. Both `scripts/fetch-fuchsia-reference` and the Nix daemon package
consume it directly. Run `scripts/fetch-fuchsia-reference --print-commit` for a
human-readable revision; do not introduce a second revision constant, archive
convention, workspace, or lockfile for another Fuchsia subsystem.

The `wlan-mlme` manifest at
`src/connectivity/wlan/lib/mlme/rust/Cargo.toml` transcribes the pinned GN
client dependency closure without modifying the materialized source.
`HOST-PACKAGING.md` records every post-fetch host-selection hunk and its
rationale. Host allocation and monotonic-time facades are separately licensed;
protocol and client state remain pinned Fuchsia source.

`wlan-mlme-sae-disposition-host.patch` adds opt-in host diagnostics at the
pinned client receive gates. `DRV_SAE_DISPOSITION_TELEMETRY` enables only
coarse state/disposition, numeric channel, fixed Authentication header, and
public SAE group fields. It changes no control flow and never emits frame
bodies, addresses, IEs, credentials, key material, packet numbers, scalars, or
elements.
