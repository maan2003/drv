#pragma once

#include <cstdlib>

namespace drv_fuchsia_audio_host {
class LogSink {
 public:
  template <typename T>
  LogSink& operator<<(const T&) { return *this; }
};

class CheckSink : public LogSink {
 public:
  explicit CheckSink(bool condition) : condition_(condition) {}
  ~CheckSink() {
    if (!condition_) {
      std::abort();
    }
  }

 private:
  bool condition_;
};
}  // namespace drv_fuchsia_audio_host

#define FX_CHECK(condition) ::drv_fuchsia_audio_host::CheckSink(static_cast<bool>(condition))
#define FX_DCHECK(condition) FX_CHECK(condition)
#define FX_LOGS(level) ::drv_fuchsia_audio_host::LogSink()
