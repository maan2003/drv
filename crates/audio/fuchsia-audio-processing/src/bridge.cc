// SPDX-License-Identifier: BSD-2-Clause

#include <algorithm>
#include <cmath>
#include <cstddef>
#include <cstdint>
#include <limits>
#include <vector>

#include "src/media/audio/lib/processing/gain.h"
#include "src/media/audio/lib/processing/sampler.h"
#include "src/media/audio/lib/processing/position_manager.h"

namespace {

// Unlike lrint, this has no dependency on the process floating-point rounding
// mode or an out-of-line libm entry point. The RT entry points use it so their
// first invocation cannot trigger lazy symbol resolution in libm.
__attribute__((always_inline)) inline int16_t RoundAndClampS16(float sample) {
  if (sample != sample) {
    return 0;
  }
  if (sample >= static_cast<float>(std::numeric_limits<int16_t>::max())) {
    return std::numeric_limits<int16_t>::max();
  }
  if (sample <= static_cast<float>(std::numeric_limits<int16_t>::min())) {
    return std::numeric_limits<int16_t>::min();
  }
  // Match lrint's default round-to-nearest, ties-to-even behavior without an
  // out-of-line libm call on the real-time path.
  const int32_t truncated = static_cast<int32_t>(sample);
  const float fraction = sample - static_cast<float>(truncated);
  if (fraction > 0.5f || (fraction == 0.5f && (truncated & 1) != 0)) {
    return static_cast<int16_t>(truncated + 1);
  }
  if (fraction < -0.5f || (fraction == -0.5f && (truncated & 1) != 0)) {
    return static_cast<int16_t>(truncated - 1);
  }
  return static_cast<int16_t>(truncated);
}

}  // namespace

extern "C" void drv_fuchsia_apply_gain_s16(int16_t* samples, size_t sample_count, float gain_db) {
  const float scale = media_audio::DbToScale(gain_db);
  for (size_t k = 0; k < sample_count; ++k) {
    const float gained = media_audio::ApplyGain<media_audio::GainType::kNonUnity>(
        static_cast<float>(samples[k]), scale);
    samples[k] = static_cast<int16_t>(std::clamp(
        std::lrint(gained), static_cast<long>(std::numeric_limits<int16_t>::min()),
        static_cast<long>(std::numeric_limits<int16_t>::max())));
  }
}

extern "C" float drv_fuchsia_db_to_scale(float gain_db) {
  return media_audio::DbToScale(gain_db);
}

extern "C" void drv_fuchsia_apply_gain_scale_s16(int16_t* samples, size_t sample_count,
                                                    float scale) {
  for (size_t k = 0; k < sample_count; ++k) {
    const float gained = media_audio::ApplyGain<media_audio::GainType::kNonUnity>(
        static_cast<float>(samples[k]), scale);
    samples[k] = RoundAndClampS16(gained);
  }
}

extern "C" void drv_fuchsia_mix_stereo_s16(const int16_t* first, const int16_t* second,
                                             int16_t* dest, size_t frame_count) {
  for (size_t frame = 0; frame < frame_count; ++frame) {
    for (size_t channel = 0; channel < 2; ++channel) {
      const size_t sample = frame * 2 + channel;
      float mixed;
      media_audio::MixSample<media_audio::GainType::kUnity, false>(
          static_cast<float>(first[sample]), &mixed, media_audio::kUnityGainScale);
      media_audio::MixSample<media_audio::GainType::kUnity, true>(
          static_cast<float>(second[sample]), &mixed, media_audio::kUnityGainScale);
      dest[sample] = RoundAndClampS16(mixed);
    }
  }
}


extern "C" size_t drv_fuchsia_resample_stereo_s16_44100_to_48000(
    const int16_t* source, size_t source_frame_count, int16_t* dest, size_t dest_frame_count) {
  if (source_frame_count == 0 || dest_frame_count == 0) {
    return 0;
  }

  // Fuchsia Fixed has 13 fractional bits. The exact source stride per destination frame is
  // (44100 * 8192) / 48000 = 7526 + 19200/48000 fractional source frames.
  constexpr int64_t kStepSize = 7526;
  constexpr uint64_t kStepSizeModulo = 19200;
  constexpr uint64_t kStepSizeDenominator = 48000;

  std::vector<float> float_dest(dest_frame_count * 2);
  media_audio::Fixed source_offset = media_audio::Fixed::FromRaw(0);
  int64_t dest_offset = 0;
  media_audio::PositionManager positions(/*source_channel_count=*/2, /*dest_channel_count=*/2,
                                         /*positive_length=*/1,
                                         /*negative_length=*/media_audio::kFracOneFrame);
  positions.SetSourceValues(source, static_cast<int64_t>(source_frame_count), &source_offset);
  positions.SetDestValues(float_dest.data(), static_cast<int64_t>(dest_frame_count), &dest_offset);
  positions.SetRateValues(kStepSize, kStepSizeModulo, kStepSizeDenominator,
                          /*source_pos_modulo=*/0);

  while (positions.CanFrameBeMixed()) {
    const int16_t* source_frame = positions.CurrentSourceFrame<const int16_t>();
    float* dest_frame = positions.CurrentDestFrame();
    media_audio::MixSample<media_audio::GainType::kUnity, false>(
        static_cast<float>(source_frame[0]), &dest_frame[0], media_audio::kUnityGainScale);
    media_audio::MixSample<media_audio::GainType::kUnity, false>(
        static_cast<float>(source_frame[1]), &dest_frame[1], media_audio::kUnityGainScale);
    positions.AdvanceFrame();
  }
  positions.UpdateOffsets();

  for (int64_t frame = 0; frame < dest_offset; ++frame) {
    for (size_t channel = 0; channel < 2; ++channel) {
      const size_t sample = static_cast<size_t>(frame) * 2 + channel;
      dest[sample] = static_cast<int16_t>(std::clamp(
          std::lrint(float_dest[sample]), static_cast<long>(std::numeric_limits<int16_t>::min()),
          static_cast<long>(std::numeric_limits<int16_t>::max())));
    }
  }
  return static_cast<size_t>(dest_offset);
}
