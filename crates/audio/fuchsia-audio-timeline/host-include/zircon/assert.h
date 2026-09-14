// SPDX-License-Identifier: BSD-2-Clause
// Host packaging facade for the two assertion macros used by audio/lib/timeline.
#ifndef DRV_FUCHSIA_HOST_ZIRCON_ASSERT_H_
#define DRV_FUCHSIA_HOST_ZIRCON_ASSERT_H_

#include <cassert>

#define ZX_ASSERT(condition) assert(condition)
#define ZX_DEBUG_ASSERT(condition) assert(condition)

#endif  // DRV_FUCHSIA_HOST_ZIRCON_ASSERT_H_
