# Pinned Fuchsia audio timeline source map

Upstream revision: `1e1219e3fac944c9a906aea9646939746b6062b3`.
Archive: `src/media/audio/lib/timeline`, Nix unpacked hash
`sha256-RMk5GJbuLmbDvbLB5bBVJTxnBOe5JSPEK2RjdPYm9sg=`.

| Checked-in path | Status | SHA-256 |
|---|---|---|
| `upstream-cargo/src/media/audio/lib/timeline/BUILD.gn` | pristine reference | `0b1b5cccbf23defd91daddce63972492021b80cd74d8aa07de3c7ca3a7e19077` |
| `upstream-cargo/src/media/audio/lib/timeline/timeline_function.cc` | pristine, compiled | `3401185005628047caa95fba98896de0bc96f73271b48ef746aed5128a9de2e4` |
| `upstream-cargo/src/media/audio/lib/timeline/timeline_function.h` | pristine, compiled | `0504ae4538e479e25c343cae83bd1c22a8120b091f69ff223d64d036eb410d5a` |
| `upstream-cargo/src/media/audio/lib/timeline/timeline_rate.cc` | pristine, compiled | `505b59f52ab8b66b377d918d4ca4e5ce13f1602076199b2892a35e863dabbb97` |
| `upstream-cargo/src/media/audio/lib/timeline/timeline_rate.h` | pristine, compiled | `28e85966fbd5576013ceeb7b53f5a2601b4cded393b4bb1a2e6ac076490955e6` |

No upstream line is patched. `host-include/` supplies only the Zircon assertion
macros used by this component and the otherwise-unused syslog include.
`build.rs` uses C++17 and suppresses GCC 15's false `maybe-uninitialized`
diagnostic for the exhaustive `RoundingMode` switch; it does not define audio
behavior.
`src/bridge.cc` is a local scalar C ABI over the unchanged
`media::TimelineFunction::Apply`; `src/lib.rs` owns and audits the only unsafe
call. The PipeWire endpoint uses that function to map its consumed byte
timeline to its reported frame position.
