/*
 *  Copyright (c) 2018 The WebRTC project authors. All Rights Reserved.
 *
 *  Use of this source code is governed by a BSD-style license
 *  that can be found in the LICENSE file in the root of the source
 *  tree. An additional intellectual property rights grant can be found
 *  in the file PATENTS.  All contributing project authors may
 *  be found in the AUTHORS file in the root of the source tree.
 */

//! TimeDelta represents the difference between two timestamps. Commonly this can
//! be a duration. However since two Timestamps are not guaranteed to have the
//! same epoch (they might come from different computers, making exact
//! synchronisation infeasible), the duration covered by a TimeDelta can be
//! undefined. To simplify usage, it can be constructed and converted to
//! different units, specifically seconds (s), milliseconds (ms) and
//! microseconds (us).
super::relative_unit!(TimeDelta);

use std::{fmt, time::Duration};

impl TimeDelta {
    const ONE_SIDED: bool = false;

    pub const fn from_minutes(value: i64) -> Self {
        Self::from_fraction(60_000_000, value)
    }

    pub fn from_minutes_float(value: f64) -> Self {
        Self::from_fraction_float(60_000_000.0, value)
    }

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

    pub const fn ns(&self) -> i64 {
        self.to_multiple(1000)
    }

    pub fn ns_float(&self) -> f64 {
        self.to_multiple_float(1000.0)
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

    pub const fn abs(&self) -> Self {
        if self.us() < 0 {
            Self::from_micros(-self.us())
        } else {
            *self
        }
    }
}

impl fmt::Debug for TimeDelta {
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
impl From<Duration> for TimeDelta {
    fn from(duration: Duration) -> Self {
        Self::from_micros(duration.as_micros() as i64)
    }
}

impl TryFrom<TimeDelta> for Duration {
    type Error = std::num::TryFromIntError;

    fn try_from(delta: TimeDelta) -> Result<Self, Self::Error> {
        Ok(Duration::from_micros(delta.us().try_into()?))
    }
}

#[cfg(test)]
mod test {
    use approx::assert_relative_eq;

    use super::*;

    #[test]
    fn const_expr() {
        const VALUE: i64 = -12345;
        const TIME_DELTA_ZERO: TimeDelta = TimeDelta::zero();
        const TIME_DELTA_PLUS_INF: TimeDelta = TimeDelta::plus_infinity();
        const TIME_DELTA_MINUS_INF: TimeDelta = TimeDelta::minus_infinity();
        assert!(TimeDelta::default() == TIME_DELTA_ZERO);
        assert!(TIME_DELTA_ZERO.is_zero());
        assert!(TIME_DELTA_PLUS_INF.is_plus_infinity());
        assert!(TIME_DELTA_MINUS_INF.is_minus_infinity());
        assert!(TIME_DELTA_PLUS_INF.ms_or(-1) == -1);

        assert!(TIME_DELTA_PLUS_INF > TIME_DELTA_ZERO);

        const TIME_DELTA_MINUTES: TimeDelta = TimeDelta::from_minutes(VALUE);
        const TIME_DELTA_SECONDS: TimeDelta = TimeDelta::from_seconds(VALUE);
        const TIME_DELTA_MS: TimeDelta = TimeDelta::from_millis(VALUE);
        const TIME_DELTA_US: TimeDelta = TimeDelta::from_micros(VALUE);

        assert!(TIME_DELTA_MINUTES.seconds_or(0) == VALUE * 60);
        assert!(TIME_DELTA_SECONDS.seconds_or(0) == VALUE);
        assert!(TIME_DELTA_MS.ms_or(0) == VALUE);
        assert!(TIME_DELTA_US.us_or(0) == VALUE);
    }

    #[test]
    fn get_back_same_values() {
        const VALUE: i64 = 499;
        for sign in [-1, 0, 1] {
            let value: i64 = VALUE * sign;
            assert_eq!(TimeDelta::from_millis(value).ms(), value);
            assert_eq!(TimeDelta::from_micros(value).us(), value);
            assert_eq!(TimeDelta::from_seconds(value).seconds(), value);
            assert_eq!(TimeDelta::from_seconds(value).seconds(), value);
        }
        assert_eq!(TimeDelta::zero().us(), 0);
    }

    #[test]
    fn get_different_prefix() {
        const VALUE: i64 = 3000000;
        assert_eq!(TimeDelta::from_micros(VALUE).seconds(), VALUE / 1000000);
        assert_eq!(TimeDelta::from_millis(VALUE).seconds(), VALUE / 1000);
        assert_eq!(TimeDelta::from_micros(VALUE).ms(), VALUE / 1000);
        assert_eq!(TimeDelta::from_minutes(VALUE / 60).seconds(), VALUE);

        assert_eq!(TimeDelta::from_millis(VALUE).us(), VALUE * 1000);
        assert_eq!(TimeDelta::from_seconds(VALUE).ms(), VALUE * 1000);
        assert_eq!(TimeDelta::from_seconds(VALUE).us(), VALUE * 1000000);
        assert_eq!(TimeDelta::from_minutes(VALUE / 60).seconds(), VALUE);
    }

    #[test]
    fn identity_checks() {
        const VALUE: i64 = 3000;
        assert!(TimeDelta::zero().is_zero());
        assert!(!TimeDelta::from_millis(VALUE).is_zero());

        assert!(TimeDelta::plus_infinity().is_infinite());
        assert!(TimeDelta::minus_infinity().is_infinite());
        assert!(!TimeDelta::zero().is_infinite());
        assert!(!TimeDelta::from_millis(-VALUE).is_infinite());
        assert!(!TimeDelta::from_millis(VALUE).is_infinite());

        assert!(!TimeDelta::plus_infinity().is_finite());
        assert!(!TimeDelta::minus_infinity().is_finite());
        assert!(TimeDelta::from_millis(-VALUE).is_finite());
        assert!(TimeDelta::from_millis(VALUE).is_finite());
        assert!(TimeDelta::zero().is_finite());

        assert!(TimeDelta::plus_infinity().is_plus_infinity());
        assert!(!TimeDelta::minus_infinity().is_plus_infinity());

        assert!(TimeDelta::minus_infinity().is_minus_infinity());
        assert!(!TimeDelta::plus_infinity().is_minus_infinity());
    }

    #[test]
    fn comparison_operators() {
        const SMALL: i64 = 450;
        const LARGE: i64 = 451;
        let small: TimeDelta = TimeDelta::from_millis(SMALL);
        let large: TimeDelta = TimeDelta::from_millis(LARGE);

        assert_eq!(TimeDelta::zero(), TimeDelta::from_millis(0));
        assert_eq!(TimeDelta::plus_infinity(), TimeDelta::plus_infinity());
        assert_eq!(small, TimeDelta::from_millis(SMALL));
        assert!(small <= TimeDelta::from_millis(SMALL));
        assert!(small >= TimeDelta::from_millis(SMALL));
        assert!(small != TimeDelta::from_millis(LARGE));
        assert!(small <= TimeDelta::from_millis(LARGE));
        assert!(small < TimeDelta::from_millis(LARGE));
        assert!(large >= TimeDelta::from_millis(SMALL));
        assert!(large > TimeDelta::from_millis(SMALL));
        assert!(TimeDelta::zero() < small);
        assert!(TimeDelta::zero() > TimeDelta::from_millis(-SMALL));
        assert!(TimeDelta::zero() > TimeDelta::from_millis(-SMALL));

        assert!(TimeDelta::plus_infinity() > large);
        assert!(TimeDelta::minus_infinity() < TimeDelta::zero());
    }

    #[test]
    fn clamping() {
        const UPPER: TimeDelta = TimeDelta::from_millis(800);
        const LOWER: TimeDelta = TimeDelta::from_millis(100);
        const UNDER: TimeDelta = TimeDelta::from_millis(100);
        const INSIDE: TimeDelta = TimeDelta::from_millis(500);
        const OVER: TimeDelta = TimeDelta::from_millis(1000);
        assert_eq!(UNDER.clamp(LOWER, UPPER), LOWER);
        assert_eq!(INSIDE.clamp(LOWER, UPPER), INSIDE);
        assert_eq!(OVER.clamp(LOWER, UPPER), UPPER);

        let mut mutable_delta: TimeDelta = LOWER;
        mutable_delta.clamp_mut(LOWER, UPPER);
        assert_eq!(mutable_delta, LOWER);
        mutable_delta = INSIDE;
        mutable_delta.clamp_mut(LOWER, UPPER);
        assert_eq!(mutable_delta, INSIDE);
        mutable_delta = OVER;
        mutable_delta.clamp_mut(LOWER, UPPER);
        assert_eq!(mutable_delta, UPPER);
    }

    #[test]
    fn can_be_inititialized_from_large_int() {
        const MAX_INT: i32 = i32::MAX;
        assert_eq!(
            TimeDelta::from_seconds(MAX_INT as i64).us(),
            MAX_INT as i64 * 1000000
        );
        assert_eq!(
            TimeDelta::from_millis(MAX_INT as i64).us(),
            MAX_INT as i64 * 1000
        );
    }

    #[test]
    fn converts_to_and_from_double() {
        const MICROS: i64 = 17017;
        const NANOS_DOUBLE: f64 = MICROS as f64 * 1e3;
        const MICROS_DOUBLE: f64 = MICROS as f64;
        const MILLIS_DOUBLE: f64 = MICROS as f64 * 1e-3;
        const SECONDS_DOUBLE: f64 = MILLIS_DOUBLE * 1e-3;

        assert_eq!(
            TimeDelta::from_micros(MICROS).seconds_float(),
            SECONDS_DOUBLE
        );
        assert_eq!(TimeDelta::from_seconds_float(SECONDS_DOUBLE).us(), MICROS);

        assert_eq!(TimeDelta::from_micros(MICROS).ms_float(), MILLIS_DOUBLE);
        assert_eq!(TimeDelta::from_millis_float(MILLIS_DOUBLE).us(), MICROS);

        assert_eq!(TimeDelta::from_micros(MICROS).us_float(), MICROS_DOUBLE);
        assert_eq!(TimeDelta::from_micros_float(MICROS_DOUBLE).us(), MICROS);

        assert_relative_eq!(
            TimeDelta::from_micros(MICROS).ns_float(),
            NANOS_DOUBLE,
            epsilon = 1.0
        );

        const PLUS_INFINITY: f64 = f64::INFINITY;
        const MINUS_INFINITY: f64 = -PLUS_INFINITY;

        assert_eq!(TimeDelta::plus_infinity().seconds_float(), PLUS_INFINITY);
        assert_eq!(TimeDelta::minus_infinity().seconds_float(), MINUS_INFINITY);
        assert_eq!(TimeDelta::plus_infinity().ms_float(), PLUS_INFINITY);
        assert_eq!(TimeDelta::minus_infinity().ms_float(), MINUS_INFINITY);
        assert_eq!(TimeDelta::plus_infinity().us_float(), PLUS_INFINITY);
        assert_eq!(TimeDelta::minus_infinity().us_float(), MINUS_INFINITY);
        assert_eq!(TimeDelta::plus_infinity().ns_float(), PLUS_INFINITY);
        assert_eq!(TimeDelta::minus_infinity().ns_float(), MINUS_INFINITY);

        assert!(TimeDelta::from_seconds_float(PLUS_INFINITY).is_plus_infinity());
        assert!(TimeDelta::from_seconds_float(MINUS_INFINITY).is_minus_infinity());
        assert!(TimeDelta::from_millis_float(PLUS_INFINITY).is_plus_infinity());
        assert!(TimeDelta::from_millis_float(MINUS_INFINITY).is_minus_infinity());
        assert!(TimeDelta::from_micros_float(PLUS_INFINITY).is_plus_infinity());
        assert!(TimeDelta::from_micros_float(MINUS_INFINITY).is_minus_infinity());
    }

    #[test]
    fn math_operations() {
        const VALUE_A: i64 = 267;
        const VALUE_B: i64 = 450;
        const DELTA_A: TimeDelta = TimeDelta::from_millis(VALUE_A);
        const DELTA_B: TimeDelta = TimeDelta::from_millis(VALUE_B);
        assert_eq!((DELTA_A + DELTA_B).ms(), VALUE_A + VALUE_B);
        assert_eq!((DELTA_A - DELTA_B).ms(), VALUE_A - VALUE_B);

        assert_eq!((DELTA_B / 10).ms(), VALUE_B / 10);
        assert_eq!(DELTA_B / DELTA_A, VALUE_B as f64 / VALUE_A as f64);

        assert_eq!(TimeDelta::from_micros(-VALUE_A).abs().us(), VALUE_A);
        assert_eq!(TimeDelta::from_micros(VALUE_A).abs().us(), VALUE_A);

        let mut mutable_delta: TimeDelta = TimeDelta::from_millis(VALUE_A);
        mutable_delta += TimeDelta::from_millis(VALUE_B);
        assert_eq!(mutable_delta, TimeDelta::from_millis(VALUE_A + VALUE_B));
        mutable_delta -= TimeDelta::from_millis(VALUE_B);
        assert_eq!(mutable_delta, TimeDelta::from_millis(VALUE_A));
    }

    #[test]
    fn multiply_by_scalar() {
        const VALUE: TimeDelta = TimeDelta::from_micros(267);
        const INT64: i64 = 450;
        const INT32: i32 = 123;
        const UNSIGNED_INT: usize = 125;
        const FLOAT: f64 = 123.0;

        assert_eq!((VALUE * INT64).us(), VALUE.us() * INT64);
        assert_eq!(VALUE * INT64, INT64 * VALUE);

        assert_eq!((VALUE * INT32).us(), VALUE.us() * INT32 as i64);
        assert_eq!(VALUE * INT32, INT32 * VALUE);

        assert_eq!(
            (VALUE * UNSIGNED_INT).us(),
            VALUE.us() * UNSIGNED_INT as i64
        );
        assert_eq!(VALUE * UNSIGNED_INT, UNSIGNED_INT * VALUE);

        assert_relative_eq!(
            (VALUE * FLOAT).us_float(),
            VALUE.us_float() * FLOAT,
            epsilon = 0.1
        );
        assert_eq!(VALUE * FLOAT, FLOAT * VALUE);
    }

    #[test]
    fn infinity_operations() {
        const VALUE: i64 = 267;
        const FINITE: TimeDelta = TimeDelta::from_millis(VALUE);
        assert!((TimeDelta::plus_infinity() + FINITE).is_plus_infinity());
        assert!((TimeDelta::plus_infinity() - FINITE).is_plus_infinity());
        assert!((FINITE + TimeDelta::plus_infinity()).is_plus_infinity());
        assert!((FINITE - TimeDelta::minus_infinity()).is_plus_infinity());

        assert!((TimeDelta::minus_infinity() + FINITE).is_minus_infinity());
        assert!((TimeDelta::minus_infinity() - FINITE).is_minus_infinity());
        assert!((FINITE + TimeDelta::minus_infinity()).is_minus_infinity());
        assert!((FINITE - TimeDelta::plus_infinity()).is_minus_infinity());
    }
}
