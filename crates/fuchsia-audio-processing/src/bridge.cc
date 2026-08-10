// SPDX-License-Identifier: BSD-2-Clause

#include <algorithm>
#include <cmath>
#include <cstddef>
#include <cstdint>
#include <limits>

#include "src/media/audio/lib/processing/gain.h"

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
