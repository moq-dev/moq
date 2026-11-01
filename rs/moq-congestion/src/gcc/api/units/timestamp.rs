/*
 *  Copyright (c) 2018 The WebRTC project authors. All Rights Reserved.
 *
 *  Use of this source code is governed by a BSD-style license
 *  that can be found in the LICENSE file in the root of the source
 *  tree. An additional intellectual property rights grant can be found
 *  in the file PATENTS.  All contributing project authors may
 *  be found in the AUTHORS file in the root of the source tree.
 */

//! Timestamp represents the time that has passed since some unspecified epoch.
//! The epoch is assumed to be before any represented timestamps, this means that
//! negative values are not valid. The most notable feature is that the
//! difference of two Timestamps results in a TimeDelta.
super::unit_base!(Timestamp);

use std::{fmt, sync::LazyLock, time::{Duration, Instant}};
use std::ops::*;

use super::TimeDelta;

impl Timestamp {
    const ONE_SIDED: bool = false;

    pub const fn from_seconds(value: i64) -> Self {
        Self::from_fraction(1_000_000, value)
    }

    pub fn from_seconds_float(value: f64) -> Self {
        Self::from_fraction_float(1_000_000.0, value)
    }

    pub const fn from_millis(value: i64) -> Self {
        Self::from_fraction(1_000, value)
    }

    pub fn from_millis_float(value: f64) -> Self {
        Self::from_fraction_float(1_000.0, value)
    }

    pub const fn from_micros(value: i64) -> Self {
        Self::from_value(value)
    }

    pub fn from_micros_float(value: f64) -> Self {
        Self::from_value_float(value)
    }

    pub const fn seconds(&self) -> i64 {
        self.to_fraction(1_000_000)
    }

    pub fn seconds_float(&self) -> f64 {
        self.to_fraction_float(1_000_000.0)
    }

    pub const fn ms(&self) -> i64 {
        self.to_fraction(1_000)
    }

    pub fn ms_float(&self) -> f64 {
        self.to_fraction_float(1_000.0)
    }

    pub const fn us(&self) -> i64 {
        self.to_value()
    }

    pub const fn us_float(&self) -> f64 {
        self.to_value_float()
    }

    pub const fn seconds_or(&self, fallback_value: i64) -> i64 {
        self.to_fraction_or(1_000_000, fallback_value)
    }

    pub const fn ms_or(&self, fallback_value: i64) -> i64 {
        self.to_fraction_or(1_000, fallback_value)
    }

    pub const fn us_or(&self, fallback_value: i64) -> i64 {
        self.to_value_or(fallback_value)
    }
}

impl Add<TimeDelta> for Timestamp {
    type Output = Self;

    fn add(self, delta: TimeDelta) -> Self {
        if self.is_plus_infinity() || delta.is_plus_infinity() {
            assert!(!self.is_minus_infinity());
            assert!(!delta.is_minus_infinity());
            return Self::plus_infinity();
        } else if self.is_minus_infinity() || delta.is_minus_infinity() {
            assert!(!self.is_plus_infinity());
            assert!(!delta.is_plus_infinity());
            return Self::minus_infinity();
        }
        Timestamp::from_micros(self.us() + delta.us())
    }
}

impl Sub<TimeDelta> for Timestamp {
    type Output = Self;

    fn sub(self, delta: TimeDelta) -> Self {
        if self.is_plus_infinity() || delta.is_minus_infinity() {
            assert!(!self.is_minus_infinity());
            assert!(!delta.is_plus_infinity());
            return Self::plus_infinity();
        } else if self.is_minus_infinity() || delta.is_plus_infinity() {
            assert!(!self.is_plus_infinity());
            assert!(!delta.is_minus_infinity());
            return Self::minus_infinity();
        }
        Timestamp::from_micros(self.us() - delta.us())
    }
}

impl Sub for Timestamp {
    type Output = TimeDelta;

    fn sub(self, other: Self) -> TimeDelta {
        if self.is_plus_infinity() || other.is_minus_infinity() {
            assert!(!self.is_minus_infinity());
            assert!(!other.is_plus_infinity());
            return TimeDelta::plus_infinity();
        } else if self.is_minus_infinity() || other.is_plus_infinity() {
            assert!(!self.is_plus_infinity());
            assert!(!other.is_minus_infinity());
            return TimeDelta::minus_infinity();
        }
        TimeDelta::from_micros(self.us() - other.us())
    }
}

impl AddAssign<TimeDelta> for Timestamp {
    fn add_assign(&mut self, delta: TimeDelta) {
        *self = *self + delta;
    }
}

impl SubAssign<TimeDelta> for Timestamp {
    fn sub_assign(&mut self, delta: TimeDelta) {
        *self = *self - delta;
    }
}

impl fmt::Debug for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.is_plus_infinity() {
            write!(f, "+inf ms")
        } else if self.is_minus_infinity() {
            write!(f, "-inf ms")
        } else if self.us() == 0 || (self.us() % 1000) != 0 {
            write!(f, "{} us", self.us())
        } else if self.ms() % 1000 != 0 {
            write!(f, "{} ms", self.ms())
        } else {
            write!(f, "{} s", self.seconds())
        }
    }
}

// Custom
impl From<Instant> for Timestamp {
    fn from(instant: Instant) -> Self {
        let r = *REF;
        if let Some(after) = instant.checked_duration_since(r) {
            Timestamp::from_micros(after.as_micros() as i64)
        } else if let Some(before) = r.checked_duration_since(instant) {
            Timestamp::from_micros(-(before.as_micros() as i64))
        } else {
            Timestamp::zero()
        }
    }
}

impl From<Timestamp> for Instant {
    fn from(timestamp: Timestamp) -> Self {
        let r = *REF;
        let duration = Duration::from_micros(timestamp.us().unsigned_abs());

        if timestamp.0 > 0 {
            r.checked_add(duration).expect("Timestamp is too far in the future")
        } else {
            r.checked_sub(duration).expect("Timestamp is from before the program started")
        }
    }
}

static REF: LazyLock<std::time::Instant> = LazyLock::new(|| {
    std::time::Instant::now()
});


#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn const_expr() {
        const VALUE: i64 = 12345;
        const TIMESTAMP_INF: Timestamp = Timestamp::plus_infinity();
        assert!(TIMESTAMP_INF.is_infinite());
        assert!(TIMESTAMP_INF.ms_or(-1) == -1);

        const TIMESTAMP_SECONDS: Timestamp = Timestamp::from_seconds(VALUE);
        const TIMESTAMP_MS: Timestamp = Timestamp::from_millis(VALUE);
        const TIMESTAMP_US: Timestamp = Timestamp::from_micros(VALUE);

        assert!(TIMESTAMP_SECONDS.seconds_or(0) == VALUE);
        assert!(TIMESTAMP_MS.ms_or(0) == VALUE);
        assert!(TIMESTAMP_US.us_or(0) == VALUE);

        assert!(TIMESTAMP_MS > TIMESTAMP_US);

        assert_eq!(TIMESTAMP_SECONDS.seconds(), VALUE);
        assert_eq!(TIMESTAMP_MS.ms(), VALUE);
        assert_eq!(TIMESTAMP_US.us(), VALUE);
    }

    #[test]
    fn get_back_same_values() {
        const VALUE: i64 = 499;
        assert_eq!(Timestamp::from_millis(VALUE).ms(), VALUE);
        assert_eq!(Timestamp::from_micros(VALUE).us(), VALUE);
        assert_eq!(Timestamp::from_seconds(VALUE).seconds(), VALUE);
    }

    #[test]
    fn get_different_prefix() {
        const VALUE: i64 = 3000000;
        assert_eq!(Timestamp::from_micros(VALUE).seconds(), VALUE / 1000000);
        assert_eq!(Timestamp::from_millis(VALUE).seconds(), VALUE / 1000);
        assert_eq!(Timestamp::from_micros(VALUE).ms(), VALUE / 1000);

        assert_eq!(Timestamp::from_millis(VALUE).us(), VALUE * 1000);
        assert_eq!(Timestamp::from_seconds(VALUE).ms(), VALUE * 1000);
        assert_eq!(Timestamp::from_seconds(VALUE).us(), VALUE * 1000000);
    }

    #[test]
    fn identity_checks() {
        const VALUE: i64 = 3000;

        assert!(Timestamp::plus_infinity().is_infinite());
        assert!(Timestamp::minus_infinity().is_infinite());
        assert!(!Timestamp::from_millis(VALUE).is_infinite());

        assert!(!Timestamp::plus_infinity().is_finite());
        assert!(!Timestamp::minus_infinity().is_finite());
        assert!(Timestamp::from_millis(VALUE).is_finite());

        assert!(Timestamp::plus_infinity().is_plus_infinity());
        assert!(!Timestamp::minus_infinity().is_plus_infinity());

        assert!(Timestamp::minus_infinity().is_minus_infinity());
        assert!(!Timestamp::plus_infinity().is_minus_infinity());
    }

    #[test]
    fn comparison_operators() {
        const SMALL: i64 = 450;
        const LARGE: i64 = 451;

        assert_eq!(Timestamp::plus_infinity(), Timestamp::plus_infinity());
        assert!(Timestamp::plus_infinity() >= Timestamp::plus_infinity());
        assert!(Timestamp::plus_infinity() > Timestamp::from_millis(LARGE));
        assert_eq!(Timestamp::from_millis(SMALL), Timestamp::from_millis(SMALL));
        assert!(Timestamp::from_millis(SMALL) <= Timestamp::from_millis(SMALL));
        assert!(Timestamp::from_millis(SMALL) >= Timestamp::from_millis(SMALL));
        assert!(Timestamp::from_millis(SMALL) != Timestamp::from_millis(LARGE));
        assert!(Timestamp::from_millis(SMALL) <= Timestamp::from_millis(LARGE));
        assert!(Timestamp::from_millis(SMALL) < Timestamp::from_millis(LARGE));
        assert!(Timestamp::from_millis(LARGE) >= Timestamp::from_millis(SMALL));
        assert!(Timestamp::from_millis(LARGE) > Timestamp::from_millis(SMALL));
    }

    #[test]
    fn can_be_inititialized_from_large_int() {
        const MAX_INT: i32 = i32::MAX;
        assert_eq!(
            Timestamp::from_seconds(MAX_INT as _).us(),
            MAX_INT as i64 * 1000000
        );
        assert_eq!(
            Timestamp::from_millis(MAX_INT as _).us(),
            MAX_INT as i64 * 1000
        );
    }

    #[test]
    fn converts_to_and_from_double() {
        const MICROS: i64 = 17017;
        const MICROS_DOUBLE: f64 = MICROS as f64;
        const MILLIS_DOUBLE: f64 = MICROS as f64 * 1e-3;
        const SECONDS_DOUBLE: f64 = MILLIS_DOUBLE * 1e-3;

        assert_eq!(
            Timestamp::from_micros(MICROS).seconds_float(),
            SECONDS_DOUBLE
        );
        assert_eq!(Timestamp::from_seconds_float(SECONDS_DOUBLE).us(), MICROS);

        assert_eq!(Timestamp::from_micros(MICROS).ms_float(), MILLIS_DOUBLE);
        assert_eq!(Timestamp::from_millis_float(MILLIS_DOUBLE).us(), MICROS);

        assert_eq!(Timestamp::from_micros(MICROS).us_float(), MICROS_DOUBLE);
        assert_eq!(Timestamp::from_micros_float(MICROS_DOUBLE).us(), MICROS);

        const PLUS_INFINITY: f64 = f64::INFINITY;
        const MINUS_INFINITY: f64 = -PLUS_INFINITY;

        assert_eq!(Timestamp::plus_infinity().seconds_float(), PLUS_INFINITY);
        assert_eq!(Timestamp::minus_infinity().seconds_float(), MINUS_INFINITY);
        assert_eq!(Timestamp::plus_infinity().ms_float(), PLUS_INFINITY);
        assert_eq!(Timestamp::minus_infinity().ms_float(), MINUS_INFINITY);
        assert_eq!(Timestamp::plus_infinity().us_float(), PLUS_INFINITY);
        assert_eq!(Timestamp::minus_infinity().us_float(), MINUS_INFINITY);

        assert!(Timestamp::from_seconds_float(PLUS_INFINITY).is_plus_infinity());
        assert!(Timestamp::from_seconds_float(MINUS_INFINITY).is_minus_infinity());
        assert!(Timestamp::from_millis_float(PLUS_INFINITY).is_plus_infinity());
        assert!(Timestamp::from_millis_float(MINUS_INFINITY).is_minus_infinity());
        assert!(Timestamp::from_micros_float(PLUS_INFINITY).is_plus_infinity());
        assert!(Timestamp::from_micros_float(MINUS_INFINITY).is_minus_infinity());
    }

    #[test]
    fn timestamp_and_time_delta_math() {
        const VALUE_A: i64 = 267;
        const VALUE_B: i64 = 450;
        const TIME_A: Timestamp = Timestamp::from_millis(VALUE_A);
        const TIME_B: Timestamp = Timestamp::from_millis(VALUE_B);
        const DELTA_A: TimeDelta = TimeDelta::from_millis(VALUE_A);
        const DELTA_B: TimeDelta = TimeDelta::from_millis(VALUE_B);

        assert_eq!((TIME_A - TIME_B), TimeDelta::from_millis(VALUE_A - VALUE_B));
        assert_eq!(
            (TIME_B - DELTA_A),
            Timestamp::from_millis(VALUE_B - VALUE_A)
        );
        assert_eq!(
            (TIME_B + DELTA_A),
            Timestamp::from_millis(VALUE_B + VALUE_A)
        );

        let mut mutable_time: Timestamp = TIME_A;
        mutable_time += DELTA_B;
        assert_eq!(mutable_time, TIME_A + DELTA_B);
        mutable_time -= DELTA_B;
        assert_eq!(mutable_time, TIME_A);
    }

    #[test]
    fn infinity_operations() {
        const VALUE: i64 = 267;
        const FINITE_TIME: Timestamp = Timestamp::from_millis(VALUE);
        const FINITE_DELTA: TimeDelta = TimeDelta::from_millis(VALUE);
        assert!((Timestamp::plus_infinity() + FINITE_DELTA).is_infinite());
        assert!((Timestamp::plus_infinity() - FINITE_DELTA).is_infinite());
        assert!((FINITE_TIME + TimeDelta::plus_infinity()).is_infinite());
        assert!((FINITE_TIME - TimeDelta::minus_infinity()).is_infinite());
    }
} // namespace test
