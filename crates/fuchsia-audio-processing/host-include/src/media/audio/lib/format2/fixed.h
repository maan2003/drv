#pragma once
#include <cstdint>
namespace media_audio {
class Fixed {
 public:
  struct Format { static constexpr int FractionalBits = 13; };
  constexpr Fixed(int64_t value = 0) : raw_(value << Format::FractionalBits) {}
  static constexpr Fixed FromRaw(int64_t raw) { return Fixed(raw, Raw{}); }
  constexpr int64_t raw_value() const { return raw_; }
 private:
  struct Raw {};
  constexpr Fixed(int64_t raw, Raw) : raw_(raw) {}
  int64_t raw_;
};
inline constexpr Fixed kOneFrame = Fixed(1);
}
