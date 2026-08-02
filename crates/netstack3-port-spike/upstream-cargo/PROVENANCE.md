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

`wlan-common-host.patch` gates only the Zircon-status conversion at the existing
platform boundary and marks the already ignored host scan timestamp as consumed.
The frame, IE, BSS, capability, channel, rate-vector, and security algorithms
remain byte-for-byte pinned Fuchsia source.

Run `scripts/fetch-fuchsia-reference --print-commit` when another build tool
needs the canonical revision. Do not introduce a second revision constant,
archive convention, workspace, or lockfile for another Fuchsia subsystem.
