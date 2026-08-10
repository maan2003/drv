// Copyright 2026 The drv Authors.
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Host value-type boundary for the Zircon API.

use std::ops::{Add, AddAssign, Mul, Neg, Sub, SubAssign};
use std::sync::OnceLock;
use std::time::{Duration as StdDuration, Instant as StdInstant};

pub use zx_status::*;

pub mod sys {
    pub use zx_types::*;
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct MonotonicInstant(i64);

impl MonotonicInstant {
    pub const ZERO: Self = Self(0);
    pub const INFINITE: Self = Self(i64::MAX);
    pub const INFINITE_PAST: Self = Self(i64::MIN);

    pub fn get() -> Self {
        static EPOCH: OnceLock<StdInstant> = OnceLock::new();
        let elapsed = EPOCH.get_or_init(StdInstant::now).elapsed().as_nanos();
        Self(i64::try_from(elapsed).unwrap_or(i64::MAX))
    }

    pub fn now() -> Self {
        Self::get()
    }

    pub fn after(duration: MonotonicDuration) -> Self {
        Self::get() + duration
    }

    pub fn sleep(self) {
        let remaining = self - Self::get();
        if remaining.0 > 0 {
            std::thread::sleep(StdDuration::from_nanos(remaining.0 as u64));
        }
    }

    pub const fn from_nanos(nanos: i64) -> Self {
        Self(nanos)
    }

    pub const fn into_nanos(self) -> i64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct MonotonicDuration(i64);

impl MonotonicDuration {
    pub const ZERO: Self = Self(0);
    pub const INFINITE: Self = Self(i64::MAX);
    pub const INFINITE_PAST: Self = Self(i64::MIN);

    pub const fn from_nanos(nanos: i64) -> Self {
        Self(nanos)
    }

    pub const fn from_micros(micros: i64) -> Self {
        Self(micros.saturating_mul(1_000))
    }

    pub const fn from_millis(millis: i64) -> Self {
        Self::from_micros(millis.saturating_mul(1_000))
    }

    pub const fn from_seconds(seconds: i64) -> Self {
        Self::from_millis(seconds.saturating_mul(1_000))
    }

    pub const fn from_minutes(minutes: i64) -> Self {
        Self::from_seconds(minutes.saturating_mul(60))
    }

    pub const fn from_hours(hours: i64) -> Self {
        Self::from_minutes(hours.saturating_mul(60))
    }

    pub const fn into_nanos(self) -> i64 {
        self.0
    }

    pub const fn into_micros(self) -> i64 {
        self.0 / 1_000
    }

    pub const fn into_millis(self) -> i64 {
        self.into_micros() / 1_000
    }

    pub const fn into_seconds(self) -> i64 {
        self.into_millis() / 1_000
    }

    pub const fn into_minutes(self) -> i64 {
        self.into_seconds() / 60
    }

    pub const fn into_hours(self) -> i64 {
        self.into_minutes() / 60
    }

    pub fn into_seconds_f64(self) -> f64 {
        self.0 as f64 / 1_000_000_000.0
    }

    pub fn sleep(self) {
        MonotonicInstant::after(self).sleep();
    }
}

impl From<StdDuration> for MonotonicDuration {
    fn from(duration: StdDuration) -> Self {
        Self(i64::try_from(duration.as_nanos()).unwrap_or(i64::MAX))
    }
}

impl Add<MonotonicDuration> for MonotonicInstant {
    type Output = Self;

    fn add(self, rhs: MonotonicDuration) -> Self::Output {
        Self(self.0.saturating_add(rhs.0))
    }
}

impl Sub<MonotonicDuration> for MonotonicInstant {
    type Output = Self;

    fn sub(self, rhs: MonotonicDuration) -> Self::Output {
        Self(self.0.saturating_sub(rhs.0))
    }
}

impl Sub for MonotonicInstant {
    type Output = MonotonicDuration;

    fn sub(self, rhs: Self) -> Self::Output {
        MonotonicDuration(self.0.saturating_sub(rhs.0))
    }
}

impl AddAssign<MonotonicDuration> for MonotonicInstant {
    fn add_assign(&mut self, rhs: MonotonicDuration) {
        self.0 = self.0.saturating_add(rhs.0);
    }
}

impl SubAssign<MonotonicDuration> for MonotonicInstant {
    fn sub_assign(&mut self, rhs: MonotonicDuration) {
        self.0 = self.0.saturating_sub(rhs.0);
    }
}

impl Add for MonotonicDuration {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self(self.0.saturating_add(rhs.0))
    }
}

impl AddAssign for MonotonicDuration {
    fn add_assign(&mut self, rhs: Self) {
        self.0 = self.0.saturating_add(rhs.0);
    }
}

impl Sub for MonotonicDuration {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Self(self.0.saturating_sub(rhs.0))
    }
}

impl Neg for MonotonicDuration {
    type Output = Self;

    fn neg(self) -> Self::Output {
        Self(self.0.saturating_neg())
    }
}

macro_rules! duration_mul {
    ($($ty:ty),* $(,)?) => {$ (
        impl Mul<$ty> for MonotonicDuration {
            type Output = Self;

            fn mul(self, rhs: $ty) -> Self::Output {
                Self(self.0.saturating_mul(i64::from(rhs)))
            }
        }
    )* };
}

duration_mul!(u8, u16, u32, i64);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_values_and_raw_type_are_exact() {
        let raw: sys::zx_status_t = Status::INVALID_ARGS.into_raw();
        assert_eq!(raw, zx_types::ZX_ERR_INVALID_ARGS);
        assert_eq!(Status::from_raw(raw), Status::INVALID_ARGS);
    }

    #[test]
    fn monotonic_time_uses_pinned_units_and_saturating_arithmetic() {
        assert_eq!(
            MonotonicDuration::from_seconds(2).into_nanos(),
            2_000_000_000
        );
        assert_eq!(
            MonotonicInstant::from_nanos(i64::MAX - 1) + MonotonicDuration::from_nanos(2),
            MonotonicInstant::INFINITE
        );
        assert_eq!(
            MonotonicInstant::from_nanos(9) - MonotonicInstant::from_nanos(4),
            MonotonicDuration::from_nanos(5)
        );
    }

    #[test]
    fn monotonic_clock_does_not_go_backwards() {
        let first = MonotonicInstant::get();
        let second = MonotonicInstant::get();
        assert!(second >= first);
    }
}
