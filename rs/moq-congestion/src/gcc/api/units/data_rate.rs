/*
 *  Copyright (c) 2018 The WebRTC project authors. All Rights Reserved.
 *
 *  Use of this source code is governed by a BSD-style license
 *  that can be found in the LICENSE file in the root of the source
 *  tree. An additional intellectual property rights grant can be found
 *  in the file PATENTS.  All contributing project authors may
 *  be found in the AUTHORS file in the root of the source tree.
 */

//! DataRate is a class that represents a given data rate. This can be used to
//! represent bandwidth, encoding bitrate, etc. The internal storage is bits per
//! second (bps).

use std::fmt;
use std::ops::*;

use super::{DataSize, Frequency, TimeDelta};

super::relative_unit!(DataRate);

impl DataRate {
    const ONE_SIDED: bool = true;

    pub const fn from_bits_per_sec(value: i64) -> Self {
        Self::from_value(value)
    }

    pub fn from_bits_per_sec_float(value: f64) -> Self {
        Self::from_value_float(value)
    }

    pub const fn from_bytes_per_sec(value: i64) -> Self {
        Self::from_fraction(8, value)
    }

    pub fn from_bytes_per_sec_float(value: f64) -> Self {
        Self::from_fraction_float(8.0, value)
    }

    pub const fn from_kilobits_per_sec(value: i64) -> Self {
        Self::from_fraction(1000, value)
    }

    pub fn from_kilobits_per_sec_float(value: f64) -> Self {
        Self::from_fraction_float(1000.0, value)
    }

    pub const fn infinity() -> Self {
        Self::plus_infinity()
    }

    pub const fn bps(&self) -> i64 {
        self.to_value()
    }

    pub const fn bps_float(&self) -> f64 {
        self.to_value_float()
    }

    pub const fn bytes_per_sec(&self) -> i64 {
        self.to_fraction(8)
    }

    pub fn bytes_per_sec_float(&self) -> f64 {
        self.to_fraction_float(8.0)
    }

    pub const fn kbps(&self) -> i64 {
        self.to_fraction(1000)
    }

    pub fn kbps_float(&self) -> f64 {
        self.to_fraction_float(1000.0)
    }

    pub const fn bps_or(&self, fallback_value: i64) -> i64 {
        self.to_fraction_or(1, fallback_value)
    }

    pub const fn kbps_or(&self, fallback_value: i64) -> i64 {
        self.to_fraction_or(1000, fallback_value)
    }

    pub const fn millibyte_per_sec(&self) -> i64 {
        const MAX_BEFORE_CONVERSION: i64 = i64::MAX / (1000 / 8);
        assert!(
            self.bps() < MAX_BEFORE_CONVERSION,
            "rate is too large to be expressed in microbytes per second"
        );
        self.bps() * (1000 / 8)
    }
}

impl Div<TimeDelta> for DataSize {
    type Output = DataRate;

    fn div(self, duration: TimeDelta) -> Self::Output {
        DataRate::from_bits_per_sec(self.microbits() / duration.us())
    }
}

impl Div<DataRate> for DataSize {
    type Output = TimeDelta;

    fn div(self, rate: DataRate) -> Self::Output {
        TimeDelta::from_micros(self.microbits() / rate.bps())
    }
}

impl Mul<TimeDelta> for DataRate {
    type Output = DataSize;

    fn mul(self, duration: TimeDelta) -> Self::Output {
        let microbits: i64 = self.bps() * duration.us();
        DataSize::from_bytes((microbits + 4000000) / 8000000)
    }
}

impl Mul<DataRate> for TimeDelta {
    type Output = DataSize;

    fn mul(self, rate: DataRate) -> Self::Output {
        rate * self
    }
}

impl Div<Frequency> for DataRate {
    type Output = DataSize;

    fn div(self, frequency: Frequency) -> Self::Output {
        let millihertz: i64 = frequency.millihertz();
        // Note that the value is truncated here reather than rounded, potentially
        // introducing an error of .5 bytes if rounding were expected.
        DataSize::from_bytes(self.millibyte_per_sec() / millihertz)
    }
}

impl Div<DataSize> for DataRate {
    type Output = Frequency;

    fn div(self, size: DataSize) -> Self::Output {
        Frequency::from_milli_hertz(self.millibyte_per_sec() / size.bytes())
    }
}

impl Mul<Frequency> for DataSize {
    type Output = DataRate;

    fn mul(self, frequency: Frequency) -> Self::Output {
        assert!(frequency.is_zero() || self.bytes() <= i64::MAX / 8 / frequency.millihertz());
        let millibits_per_second: i64 = self.bytes() * 8 * frequency.millihertz();
        DataRate::from_bits_per_sec((millibits_per_second + 500) / 1000)
    }
}

impl Mul<DataSize> for Frequency {
    type Output = DataRate;

    fn mul(self, size: DataSize) -> Self::Output {
        size * self
    }
}

impl fmt::Debug for DataRate {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.is_plus_infinity() {
            write!(f, "+inf bps")
        } else if self.is_minus_infinity() {
            write!(f, "-inf bps")
        } else if self.bps() == 0 || self.bps() % 1000 != 0 {
            write!(f, "{} bps", self.bps())
        } else {
            write!(f, "{} kbps", self.kbps())
        }
    }
}

#[cfg(test)]
mod test {
    use approx::assert_relative_eq;

    use super::*;

    #[test]
    fn compiles_with_checks_and_logs() {
        let a: DataRate = DataRate::from_kilobits_per_sec(300);
        let b: DataRate = DataRate::from_kilobits_per_sec(210);
        assert!(a > b);
    }

    #[test]
    fn const_expr() {
        const VALUE: i64 = 12345;
        const DATA_RATE_ZERO: DataRate = DataRate::zero();
        const DATA_RATE_INF: DataRate = DataRate::infinity();
        assert_eq!(DataRate::default(), DATA_RATE_ZERO);
        assert!(DATA_RATE_ZERO.is_zero());
        assert!(DATA_RATE_INF.is_infinite());
        assert!(DATA_RATE_INF.bps_or(-1) == -1);
        assert!(DATA_RATE_INF > DATA_RATE_ZERO);

        const DATA_RATE_BPS: DataRate = DataRate::from_bits_per_sec(VALUE);
        const DATA_RATE_KBPS: DataRate = DataRate::from_kilobits_per_sec(VALUE);
        assert!(DATA_RATE_BPS.bps() == VALUE);
        assert!(DATA_RATE_BPS.bps_or(0) == VALUE);
        assert!(DATA_RATE_KBPS.kbps_or(0) == VALUE);
    }

    #[test]
    fn get_back_same_values() {
        const VALUE: i64 = 123 * 8;
        assert_eq!(DataRate::from_bits_per_sec(VALUE).bps(), VALUE);
        assert_eq!(DataRate::from_kilobits_per_sec(VALUE).kbps(), VALUE);
    }

    #[test]
    fn get_different_prefix() {
        const VALUE: i64 = 123 * 8000;
        assert_eq!(DataRate::from_bits_per_sec(VALUE).kbps(), VALUE / 1000);
    }

    #[test]
    fn identity_checks() {
        const VALUE: i64 = 3000;
        assert!(DataRate::zero().is_zero());
        assert!(!DataRate::from_bits_per_sec(VALUE).is_zero());

        assert!(DataRate::infinity().is_infinite());
        assert!(!DataRate::zero().is_infinite());
        assert!(!DataRate::from_bits_per_sec(VALUE).is_infinite());

        assert!(!DataRate::infinity().is_finite());
        assert!(DataRate::from_bits_per_sec(VALUE).is_finite());
        assert!(DataRate::zero().is_finite());
    }

    #[test]
    fn comparison_operators() {
        const SMALL: i64 = 450;
        const LARGE: i64 = 451;
        let small: DataRate = DataRate::from_bits_per_sec(SMALL);
        let large: DataRate = DataRate::from_bits_per_sec(LARGE);

        assert_eq!(DataRate::zero(), DataRate::from_bits_per_sec(0));
        assert_eq!(DataRate::infinity(), DataRate::infinity());
        assert_eq!(small, small);
        assert!(small <= small);
        assert!(small >= small);
        assert!(small != large);
        assert!(small <= large);
        assert!(small < large);
        assert!(large >= small);
        assert!(large > small);
        assert!(DataRate::zero() < small);
        assert!(DataRate::infinity() > large);
    }

    #[test]
    fn converts_to_and_from_double() {
        const VALUE: i64 = 128;
        const DOUBLE_VALUE: f64 = VALUE as f64;
        const DOUBLE_KBPS: f64 = VALUE as f64 * 1e-3;
        const FLOAT_KBPS: f32 = DOUBLE_KBPS as f32;

        assert_eq!(DataRate::from_bits_per_sec(VALUE).bps_float(), DOUBLE_VALUE);
        assert_eq!(DataRate::from_bits_per_sec(VALUE).kbps_float(), DOUBLE_KBPS);
        assert_eq!(
            DataRate::from_bits_per_sec(VALUE).kbps_float() as f32,
            FLOAT_KBPS
        );
        assert_eq!(DataRate::from_bits_per_sec_float(DOUBLE_VALUE).bps(), VALUE);
        assert_eq!(
            DataRate::from_kilobits_per_sec_float(DOUBLE_KBPS).bps(),
            VALUE
        );

        const INFINITY: f64 = f64::INFINITY;
        assert_eq!(DataRate::infinity().bps_float(), INFINITY);
        assert!(DataRate::from_bits_per_sec_float(INFINITY).is_infinite());
        assert!(DataRate::from_kilobits_per_sec_float(INFINITY).is_infinite());
    }
    #[test]
    fn clamping() {
        const UPPER: DataRate = DataRate::from_kilobits_per_sec(800);
        const LOWER: DataRate = DataRate::from_kilobits_per_sec(100);
        const UNDER: DataRate = DataRate::from_kilobits_per_sec(100);
        const INSIDE: DataRate = DataRate::from_kilobits_per_sec(500);
        const OVER: DataRate = DataRate::from_kilobits_per_sec(1000);
        assert_eq!(UNDER.clamp(LOWER, UPPER), LOWER);
        assert_eq!(INSIDE.clamp(LOWER, UPPER), INSIDE);
        assert_eq!(OVER.clamp(LOWER, UPPER), UPPER);

        let mut mutable_rate: DataRate = LOWER;
        mutable_rate.clamp_mut(LOWER, UPPER);
        assert_eq!(mutable_rate, LOWER);
        mutable_rate = INSIDE;
        mutable_rate.clamp_mut(LOWER, UPPER);
        assert_eq!(mutable_rate, INSIDE);
        mutable_rate = OVER;
        mutable_rate.clamp_mut(LOWER, UPPER);
        assert_eq!(mutable_rate, UPPER);
    }

    #[test]
    fn math_operations() {
        const VALUE_A: i64 = 450;
        const VALUE_B: i64 = 267;
        const RATE_A: DataRate = DataRate::from_bits_per_sec(VALUE_A);
        const RATE_B: DataRate = DataRate::from_bits_per_sec(VALUE_B);
        const INT32_VALUE: i32 = 123;
        const FLOAT_VALUE: f64 = 123.0;

        assert_eq!((RATE_A + RATE_B).bps(), VALUE_A + VALUE_B);
        assert_eq!((RATE_A - RATE_B).bps(), VALUE_A - VALUE_B);

        assert_eq!((RATE_A * VALUE_B).bps(), VALUE_A * VALUE_B);
        assert_eq!((RATE_A * INT32_VALUE).bps(), VALUE_A * INT32_VALUE as i64);
        assert_eq!((RATE_A * FLOAT_VALUE).bps(), VALUE_A * FLOAT_VALUE as i64);

        assert_eq!(RATE_A / RATE_B, VALUE_A as f64 / VALUE_B as f64);

        assert_eq!((RATE_A / 10).bps(), VALUE_A / 10);
        assert_relative_eq!(
            (RATE_A / 0.5).bps_float(),
            VALUE_A as f64 * 2.0,
            epsilon = 1.0
        );

        let mut mutable_rate: DataRate = DataRate::from_bits_per_sec(VALUE_A);
        mutable_rate += RATE_B;
        assert_eq!(mutable_rate.bps(), VALUE_A + VALUE_B);
        mutable_rate -= RATE_A;
        assert_eq!(mutable_rate.bps(), VALUE_B);
    }

    #[test]
    fn data_rate_and_data_size_and_time_delta() {
        const SECONDS: i64 = 5;
        const BITS_PER_SECOND: i64 = 440;
        const BYTES: i64 = 44000;
        const DELTA_A: TimeDelta = TimeDelta::from_seconds(SECONDS);
        const RATE_B: DataRate = DataRate::from_bits_per_sec(BITS_PER_SECOND);
        const SIZE_C: DataSize = DataSize::from_bytes(BYTES);
        assert_eq!((DELTA_A * RATE_B).bytes(), SECONDS * BITS_PER_SECOND / 8);
        assert_eq!((RATE_B * DELTA_A).bytes(), SECONDS * BITS_PER_SECOND / 8);
        assert_eq!((SIZE_C / DELTA_A).bps(), BYTES * 8 / SECONDS);
        assert_eq!((SIZE_C / RATE_B).seconds(), BYTES * 8 / BITS_PER_SECOND);
    }

    #[test]
    fn data_rate_and_data_size_and_frequency() {
        const HERTZ: i64 = 30;
        const BITS_PER_SECOND: i64 = 96000;
        const BYTES: i64 = 1200;
        const FREQ_A: Frequency = Frequency::from_hertz(HERTZ);
        const RATE_B: DataRate = DataRate::from_bits_per_sec(BITS_PER_SECOND);
        const SIZE_C: DataSize = DataSize::from_bytes(BYTES);
        assert_eq!((FREQ_A * SIZE_C).bps(), HERTZ * BYTES * 8);
        assert_eq!((SIZE_C * FREQ_A).bps(), HERTZ * BYTES * 8);
        assert_eq!((RATE_B / SIZE_C).hertz(), BITS_PER_SECOND / BYTES / 8);
        assert_eq!((RATE_B / FREQ_A).bytes(), BITS_PER_SECOND / HERTZ / 8);
    }

    const JUST_SMALL_ENOUGH_FOR_DIVISION: i64 = i64::MAX / 8000000;
    const TOOLARGE_FOR_DIVISION: DataSize =
        DataSize::from_bytes(JUST_SMALL_ENOUGH_FOR_DIVISION + 1);

    #[test]
    fn division_fails_on_large_size() {
        // Note that the failure is expected since the current implementation  is
        // implementated in a way that does not support division of large sizes. If
        // the implementation is changed, this test can safely be removed.
        const LARGE_SIZE: DataSize = DataSize::from_bytes(JUST_SMALL_ENOUGH_FOR_DIVISION);
        const DATA_RATE: DataRate = DataRate::from_kilobits_per_sec(100);
        const TIME_DELTA: TimeDelta = TimeDelta::from_millis(100);
        assert!((LARGE_SIZE / DATA_RATE).is_finite());
        assert!((LARGE_SIZE / TIME_DELTA).is_finite());
    }

    #[test]
    #[should_panic]
    fn division_fails_on_large_size1() {
        const DATA_RATE: DataRate = DataRate::from_kilobits_per_sec(100);
        let _ = TOOLARGE_FOR_DIVISION / DATA_RATE;
    }
    #[test]
    #[should_panic]
    fn division_fails_on_large_size2() {
        const TIME_DELTA: TimeDelta = TimeDelta::from_millis(100);
        let _ = TOOLARGE_FOR_DIVISION / TIME_DELTA;
    }
}
