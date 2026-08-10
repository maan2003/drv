# Pinned Fuchsia audio processing source map

Upstream revision: `1e1219e3fac944c9a906aea9646939746b6062b3`.
Pinned closure path: `src/media/audio/lib/processing` in
`nix/fuchsia-reference.json`.

| Checked-in path | Status | SHA-256 |
|---|---|---|
| `upstream-cargo/src/media/audio/lib/processing/BUILD.gn` | pristine reference | `51cba9a94aa818255d0734d2b51d73f9dabdc71d15db979601f12c92f128c218` |
| `upstream-cargo/src/media/audio/lib/processing/channel_strip.h` | pristine, compiled | `40c06462ba6fc6987f6fcadc8a05cdd957e50416cfbd6b8d1f59670474075e45` |
| `upstream-cargo/src/media/audio/lib/processing/gain.h` | pristine, compiled | `4ff7727d915073951f626449361b99a1fc4e5656569f6a9306fe47750ed8519f` |
| `upstream-cargo/src/media/audio/lib/processing/sampler.h` | pristine, compiled | `9068f65d946ca9f3bdd618bd308d1956c4bb3d605c85f9b91bba3a77c20de213` |

No host patch is applied. `host-include/` supplies only `std::span` and DCHECK
facades required by ChannelStrip. `src/bridge.cc` is the isolated
representation adapter: ChannelStrip owns the planar working buffer and channel
access, and sampler `MixSample<kUnity, false/true>` owns overwrite/accumulation,
while the bridge converts two fixed S16 streams. Minimal format, timeline, and
Zircon value declarations allow the unchanged sampler header to compile without
bringing unavailable Fuchsia transports onto the host. Gain
classification, decibel-to-scale conversion, and per-sample gain multiplication
remain direct calls into the unchanged pinned header.
