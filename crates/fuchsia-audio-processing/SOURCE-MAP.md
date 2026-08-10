# Pinned Fuchsia audio processing source map

Upstream revision: `1e1219e3fac944c9a906aea9646939746b6062b3`.
Pinned closure path: `src/media/audio/lib/processing` in
`nix/fuchsia-reference.json`.

| Checked-in path | Status | SHA-256 |
|---|---|---|
| `upstream-cargo/src/media/audio/lib/processing/BUILD.gn` | pristine reference | `51cba9a94aa818255d0734d2b51d73f9dabdc71d15db979601f12c92f128c218` |
| `upstream-cargo/src/media/audio/lib/processing/gain.h` | pristine, compiled | `4ff7727d915073951f626449361b99a1fc4e5656569f6a9306fe47750ed8519f` |

No host patch is applied. `src/bridge.cc` is the isolated representation
adapter; gain classification, decibel-to-scale conversion, and per-sample gain
multiplication remain direct calls into the unchanged pinned header.
