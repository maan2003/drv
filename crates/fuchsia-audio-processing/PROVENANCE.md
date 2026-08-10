# Provenance

`upstream-cargo/src/media/audio/lib/processing/{gain.h,BUILD.gn}` are
byte-for-byte copies from Fuchsia commit
`1e1219e3fac944c9a906aea9646939746b6062b3`. The pinned processing path is
already recorded in `nix/fuchsia-reference.json`; there is no second source pin.
The files are governed by `LICENSE.fuchsia` (BSD 2-Clause).

The Cargo manifest, build script, and C ABI bridge are host packaging for drv.
No upstream line is patched. The bridge adapts interleaved signed 16-bit host
PCM to unchanged Fuchsia `media_audio::DbToScale` and
`media_audio::ApplyGain<GainType::kNonUnity>` calls, then clamps the result back
to the negotiated sample representation.
