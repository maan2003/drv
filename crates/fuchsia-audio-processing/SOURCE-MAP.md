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
| `upstream-cargo/src/media/audio/lib/processing/flags.h` | pristine, compiled | `42e30aa3bb890189570241531d5fa72077cb81ef1ae5aef824df5e18a14a3b4c` |
| `upstream-cargo/src/media/audio/lib/processing/position_manager.h` | pristine, compiled | `4dddbe8aac4ab6c20f02927e2411d8af6797c341b4ba00b034914818fd9c61ee` |
| `upstream-cargo/src/media/audio/lib/processing/position_manager.cc` | pristine, compiled | `03c0c1f9c8c02a967c79ebcd5b3b38891c18ef1023e3f6d8b6c3f8daba578b37` |

No host patch is applied. `host-include/` supplies narrow `std::span`, DCHECK,
logging, tracing, fixed-point, format, timeline, and Zircon value facades needed
to compile the selected unchanged sources without unavailable Fuchsia
transports. `src/bridge.cc` is the isolated representation adapter.

ChannelStrip owns the planar working buffer and channel access, and sampler
`MixSample<kUnity, false/true>` owns overwrite/accumulation for two-stream
mixing. PositionManager owns exact fractional source-position and residual-rate
advancement for 44.1 kHz to 48 kHz point resampling; `MixSample<kUnity, false>`
transfers each selected point sample. Gain classification, decibel-to-scale
conversion, and per-sample gain multiplication remain direct calls into the
unchanged pinned gain header.
