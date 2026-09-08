#pragma once
#include <cstdint>
namespace zx {
class duration { public: constexpr duration(int64_t = 0) {} };
class time { public: constexpr time(int64_t = 0) {} };
}
namespace media { class TimelineFunction {}; }
