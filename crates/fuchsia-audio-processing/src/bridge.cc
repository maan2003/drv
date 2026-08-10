// SPDX-License-Identifier: BSD-2-Clause

#include <algorithm>
#include <cmath>
#include <cstddef>
#include <cstdint>
#include <limits>

#include "src/media/audio/lib/processing/gain.h"
#include "src/media/audio/lib/processing/channel_strip.h"
#include "src/media/audio/lib/processing/sampler.h"

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

extern "C" void drv_fuchsia_mix_stereo_s16(const int16_t* first, const int16_t* second,
                                             int16_t* dest, size_t frame_count) {
  media_audio::ChannelStrip strip(2, static_cast<int64_t>(frame_count));
  for (size_t frame = 0; frame < frame_count; ++frame) {
    for (size_t channel = 0; channel < 2; ++channel) {
      const size_t sample = frame * 2 + channel;
      media_audio::MixSample<media_audio::GainType::kUnity, false>(
          static_cast<float>(first[sample]), &strip[channel][frame], media_audio::kUnityGainScale);
      media_audio::MixSample<media_audio::GainType::kUnity, true>(
          static_cast<float>(second[sample]), &strip[channel][frame], media_audio::kUnityGainScale);
      dest[sample] = static_cast<int16_t>(std::clamp(
          std::lrint(strip[channel][frame]),
          static_cast<long>(std::numeric_limits<int16_t>::min()),
          static_cast<long>(std::numeric_limits<int16_t>::max())));
    }
  }
}
