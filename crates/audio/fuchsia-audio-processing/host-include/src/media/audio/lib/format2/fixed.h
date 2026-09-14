#pragma once
#include <cstdint>
namespace media_audio {
class Fixed {
 public:
  struct Format { static constexpr int FractionalBits = 13; };
  constexpr Fixed(int64_t value = 0) : raw_(value << Format::FractionalBits) {}
  static constexpr Fixed FromRaw(int64_t raw) { return Fixed(raw, Raw{}); }
  constexpr int64_t raw_value() const { return raw_; }
  constexpr int64_t Floor() const { return raw_ >> Format::FractionalBits; }
  constexpr Fixed Integral() const {
    return FromRaw((raw_ >> Format::FractionalBits) << Format::FractionalBits);
  }
  constexpr Fixed Fraction() const {
    return FromRaw(raw_ - Integral().raw_value());
  }
 private:
  struct Raw {};
  constexpr Fixed(int64_t raw, Raw) : raw_(raw) {}
  int64_t raw_;
};
inline constexpr int64_t kFracOneFrame = int64_t{1} << Fixed::Format::FractionalBits;
inline constexpr Fixed kOneFrame = Fixed(1);
}
