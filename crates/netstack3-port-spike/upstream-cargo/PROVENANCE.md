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
DHCP core/protocol and Fuchsia Trust-DNS forks. Their original source and
license notices are fetched unchanged. Host changes are isolated as reviewable
patch files; no algorithm, constant, state transition, cache, or retry policy is
forked into the native adapter.

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

The pinned SME uses Fuchsia Inspect only for diagnostics. Separately licensed
path-shaped host facades retain scalar/string/byte property state and the API
surface needed by SME, while disabling Fuchsia-specific hierarchy/VMO encoding
and convenience log emission. SME policy and state transitions remain in the
pinned WLAN source rather than being copied into the diagnostics boundary.

`wlan-timer-host.patch` retains WLAN common's pinned timer queue and replaces
only fuchsia-async's deadline wake-up with the Fuchsia-pinned Tokio timer.
Event identity, concurrent deadline ordering, cancellation, and filtering stay
in upstream source and are exercised by host integration tests.

Run `scripts/fetch-fuchsia-reference --print-commit` when another build tool
needs the canonical revision. Do not introduce a second revision constant,
archive convention, workspace, or lockfile for another Fuchsia subsystem.

The `wlan-mlme` manifest at
`src/connectivity/wlan/lib/mlme/rust/Cargo.toml` transcribes the pinned GN
client dependency closure without modifying the materialized source.
`HOST-PACKAGING.md` records every post-fetch host-selection hunk and its
rationale. Host allocation and monotonic-time facades are separately licensed;
protocol and client state remain pinned Fuchsia source.
