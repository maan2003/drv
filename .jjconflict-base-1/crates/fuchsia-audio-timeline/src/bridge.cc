// SPDX-License-Identifier: BSD-2-Clause

#include <cstdint>

#include "src/media/audio/lib/timeline/timeline_function.h"

extern "C" int64_t drv_fuchsia_timeline_apply(int64_t subject_time, int64_t reference_time,
                                                uint64_t subject_delta,
                                                uint64_t reference_delta,
                                                int64_t reference_input) {
  return media::TimelineFunction::Apply(subject_time, reference_time,
                                        media::TimelineRate(subject_delta, reference_delta),
                                        reference_input);
}
