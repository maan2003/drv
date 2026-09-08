# Provenance

`upstream-cargo/src/media/audio/lib/processing/{gain.h,channel_strip.h,sampler.h,flags.h,position_manager.h,position_manager.cc,BUILD.gn}`
are byte-for-byte copies from Fuchsia commit
`1e1219e3fac944c9a906aea9646939746b6062b3`. The pinned processing path is
already recorded in `nix/fuchsia-reference.json`; there is no second source pin.
The files are governed by `LICENSE.fuchsia` (BSD 2-Clause).

The Cargo manifest, include facades, build script, and C ABI bridge are host
packaging for drv. No upstream line is patched. The bridge adapts interleaved
signed 16-bit host PCM to unchanged Fuchsia `media_audio::ChannelStrip`,
`media_audio::PositionManager`, `media_audio::MixSample`,
`media_audio::DbToScale`, and `media_audio::ApplyGain<GainType::kNonUnity>`
calls.

For 44.1 kHz to 48 kHz point resampling, the unchanged `PositionManager` owns
the source offset, exact 13-bit fixed-point stride, residual modulo, and frame
advancement. The bridge supplies the exact ratio
`7526 + 19200/48000` fractional source frames per destination frame and uses
the unchanged `MixSample<kUnity, false>` primitive to transfer the selected
stereo frame. It owns only S16/float representation conversion, output
allocation, and clamping. Fuchsia's `PointSampler` class is intentionally not
packaged because at this revision its factory rejects differing source and
destination frame rates; claiming it as the rate converter would be incorrect.
