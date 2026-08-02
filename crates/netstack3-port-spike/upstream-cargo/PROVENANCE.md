# Pinned Fuchsia Netstack3 Cargo overlay provenance

The manifests and build configuration in this directory package unmodified
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
