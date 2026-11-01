/*
 *  Copyright (c) 2019 The WebRTC project authors. All Rights Reserved.
 *
 *  Use of this source code is governed by a BSD-style license
 *  that can be found in the LICENSE file in the root of the source
 *  tree. An additional intellectual property rights grant can be found
 *  in the file PATENTS.  All contributing project authors may
 *  be found in the AUTHORS file in the root of the source tree.
 */

use std::{fmt, ops::*};

use super::TimeDelta;

super::relative_unit!(Frequency);

impl Frequency {
    const ONE_SIDED: bool = true;

    pub const fn from_milli_hertz(value: i64) -> Self {
        Self::from_value(value)
    }

    pub fn from_milli_hertz_float(value: f64) -> Self {
        Self::from_value_float(value)
    }

    pub const fn from_hertz(value: i64) -> Self {
        Self::from_fraction(1_000, value)
    }

    pub fn from_hertz_float(value: f64) -> Self {
        Self::from_fraction_float(1_000.0, value)
    }

    pub const fn from_kilo_hertz(value: i64) -> Self {
        Self::from_fraction(1_000_000, value)
    }

    pub fn from_kilo_hertz_float(value: f64) -> Self {
        Self::from_fraction_float(1_000_000.0, value)
    }

    pub const fn hertz(&self) -> i64 {
        self.to_fraction(1000)
    }

    pub fn hertz_float(&self) -> f64 {
        self.to_fraction_float(1000.0)
    }

    pub const fn millihertz(&self) -> i64 {
        self.to_value()
    }

    pub const fn millihertz_float(&self) -> f64 {
        self.to_value_float()
    }
}

impl Div<TimeDelta> for i64 {
    type Output = Frequency;

    fn div(self, interval: TimeDelta) -> Frequency {
        const KILO_PER_MICRO: i64 = 1000 * 1000000;
        assert!(self <= i64::MAX / KILO_PER_MICRO);
        assert!(interval.is_finite());
        assert!(!interval.is_zero());
        Frequency::from_milli_hertz(self * KILO_PER_MICRO / interval.us())
    }
}

impl Div<Frequency> for i64 {
    type Output = TimeDelta;

    fn div(self, frequency: Frequency) -> TimeDelta {
        const MEGA_PER_MILLI: i64 = 1000000 * 1000;
        assert!(self <= i64::MAX / MEGA_PER_MILLI);
        assert!(frequency.is_finite());
        assert!(!frequency.is_zero());
        TimeDelta::from_micros(self * MEGA_PER_MILLI / frequency.millihertz())
    }
}

impl Mul<TimeDelta> for Frequency {
    type Output = f64;

    fn mul(self, time_delta: TimeDelta) -> f64 {
        self.hertz_float() * time_delta.seconds_float()
    }
}

impl Mul<Frequency> for TimeDelta {
    type Output = f64;

    fn mul(self, frequency: Frequency) -> f64 {
        frequency * self
    }
}

impl fmt::Debug for Frequency {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.is_plus_infinity() {
            write!(f, "+inf Hz")
        } else if self.is_minus_infinity() {
            write!(f, "-inf Hz")
        } else if self.millihertz() % 1000 != 0 {
            write!(f, "{:.3} Hz", self.hertz_float())
        } else {
            write!(f, "{} Hz", self.hertz())
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn const_expr() {
        const FREQUENCY_ZERO: Frequency = Frequency::zero();
        const FREQUENCY_PLUS_INF: Frequency = Frequency::plus_infinity();
        const FREQUENCY_MINUS_INF: Frequency = Frequency::minus_infinity();
        assert!(Frequency::default() == FREQUENCY_ZERO);
        assert!(FREQUENCY_ZERO.is_zero());
        assert!(FREQUENCY_PLUS_INF.is_plus_infinity());
        assert!(FREQUENCY_MINUS_INF.is_minus_infinity());

        assert!(FREQUENCY_PLUS_INF > FREQUENCY_ZERO);
    }

    #[test]
    fn get_back_same_values() {
        const VALUE: i64 = 31;
        assert_eq!(Frequency::from_hertz(VALUE).hertz(), VALUE);
        assert_eq!(Frequency::zero().hertz(), 0);
    }

    #[test]
    fn get_different_prefix() {
        const VALUE: i64 = 30000;
        assert_eq!(Frequency::from_milli_hertz(VALUE).hertz(), VALUE / 1000);
        assert_eq!(Frequency::from_hertz(VALUE).millihertz(), VALUE * 1000);
        assert_eq!(Frequency::from_kilo_hertz(VALUE).hertz(), VALUE * 1000);
    }

    #[test]
    fn identity_checks() {
        const VALUE: i64 = 31;
        assert!(Frequency::zero().is_zero());
        assert!(!Frequency::from_hertz(VALUE).is_zero());

        assert!(Frequency::plus_infinity().is_infinite());
        assert!(Frequency::minus_infinity().is_infinite());
        assert!(!Frequency::zero().is_infinite());
        assert!(!Frequency::from_hertz(VALUE).is_infinite());

        assert!(!Frequency::plus_infinity().is_finite());
        assert!(!Frequency::minus_infinity().is_finite());
        assert!(Frequency::from_hertz(VALUE).is_finite());
        assert!(Frequency::zero().is_finite());

        assert!(Frequency::plus_infinity().is_plus_infinity());
        assert!(!Frequency::minus_infinity().is_plus_infinity());

        assert!(Frequency::minus_infinity().is_minus_infinity());
        assert!(!Frequency::plus_infinity().is_minus_infinity());
    }

    #[test]
    fn comparison_operators() {
        const SMALL: i64 = 42;
        const LARGE: i64 = 45;
        let small: Frequency = Frequency::from_hertz(SMALL);
        let large: Frequency = Frequency::from_hertz(LARGE);

        assert_eq!(Frequency::zero(), Frequency::from_hertz(0));
        assert_eq!(Frequency::plus_infinity(), Frequency::plus_infinity());
        assert_eq!(small, Frequency::from_hertz(SMALL));
        assert!(small <= Frequency::from_hertz(SMALL));
        assert!(small >= Frequency::from_hertz(SMALL));
        assert!(small != Frequency::from_hertz(LARGE));
        assert!(small <= Frequency::from_hertz(LARGE));
        assert!(small < Frequency::from_hertz(LARGE));
        assert!(large >= Frequency::from_hertz(SMALL));
        assert!(large > Frequency::from_hertz(SMALL));
        assert!(Frequency::zero() < small);

        assert!(Frequency::plus_infinity() > large);
        assert!(Frequency::minus_infinity() < Frequency::zero());
    }

    #[test]
    fn clamping() {
        const UPPER: Frequency = Frequency::from_hertz(800);
        const LOWER: Frequency = Frequency::from_hertz(100);
        const UNDER: Frequency = Frequency::from_hertz(100);
        const INSIDE: Frequency = Frequency::from_hertz(500);
        const OVER: Frequency = Frequency::from_hertz(1000);
        assert_eq!(UNDER.clamp(LOWER, UPPER), LOWER);
        assert_eq!(INSIDE.clamp(LOWER, UPPER), INSIDE);
        assert_eq!(OVER.clamp(LOWER, UPPER), UPPER);

        let mut mutable_frequency: Frequency = LOWER;
        mutable_frequency.clamp_mut(LOWER, UPPER);
        assert_eq!(mutable_frequency, LOWER);
        mutable_frequency = INSIDE;
        mutable_frequency.clamp_mut(LOWER, UPPER);
        assert_eq!(mutable_frequency, INSIDE);
        mutable_frequency = OVER;
        mutable_frequency.clamp_mut(LOWER, UPPER);
        assert_eq!(mutable_frequency, UPPER);
    }

    #[test]
    fn math_operations() {
        const VALUE_A: i64 = 457;
        const VALUE_B: i64 = 260;
        const FREQUENCY_A: Frequency = Frequency::from_hertz(VALUE_A);
        const FREQUENCY_B: Frequency = Frequency::from_hertz(VALUE_B);
        assert_eq!((FREQUENCY_A + FREQUENCY_B).hertz(), VALUE_A + VALUE_B);
        assert_eq!((FREQUENCY_A - FREQUENCY_B).hertz(), VALUE_A - VALUE_B);

        assert_eq!(
            (Frequency::from_hertz(VALUE_A) * VALUE_B).hertz(),
            VALUE_A * VALUE_B
        );

        assert_eq!((FREQUENCY_B / 10).hertz(), VALUE_B / 10);
        assert_eq!(FREQUENCY_B / FREQUENCY_A, VALUE_B as f64 / VALUE_A as f64);

        let mut mutable_frequency: Frequency = Frequency::from_hertz(VALUE_A);
        mutable_frequency += Frequency::from_hertz(VALUE_B);
        assert_eq!(mutable_frequency, Frequency::from_hertz(VALUE_A + VALUE_B));
        mutable_frequency -= Frequency::from_hertz(VALUE_B);
        assert_eq!(mutable_frequency, Frequency::from_hertz(VALUE_A));
    }
    #[test]
    fn rounding() {
        let freq_high: Frequency = Frequency::from_hertz_float(23.976);
        assert_eq!(freq_high.hertz(), 24);
        assert_eq!(
            freq_high.round_down_to(Frequency::from_hertz(1)),
            Frequency::from_hertz(23)
        );
        assert_eq!(
            freq_high.round_to(Frequency::from_hertz(1)),
            Frequency::from_hertz(24)
        );
        assert_eq!(
            freq_high.round_up_to(Frequency::from_hertz(1)),
            Frequency::from_hertz(24)
        );

        let freq_low: Frequency = Frequency::from_hertz_float(23.4);
        assert_eq!(freq_low.hertz(), 23);
        assert_eq!(
            freq_low.round_down_to(Frequency::from_hertz(1)),
            Frequency::from_hertz(23)
        );
        assert_eq!(
            freq_low.round_to(Frequency::from_hertz(1)),
            Frequency::from_hertz(23)
        );
        assert_eq!(
            freq_low.round_up_to(Frequency::from_hertz(1)),
            Frequency::from_hertz(24)
        );
    }

    #[test]
    fn infinity_operations() {
        const VALUE: f64 = 267.0;
        let finite: Frequency = Frequency::from_hertz_float(VALUE);
        assert!((Frequency::plus_infinity() + finite).is_plus_infinity());
        assert!((Frequency::plus_infinity() - finite).is_plus_infinity());
        assert!((finite + Frequency::plus_infinity()).is_plus_infinity());
        assert!((finite - Frequency::minus_infinity()).is_plus_infinity());

        assert!((Frequency::minus_infinity() + finite).is_minus_infinity());
        assert!((Frequency::minus_infinity() - finite).is_minus_infinity());
        assert!((finite + Frequency::minus_infinity()).is_minus_infinity());
        assert!((finite - Frequency::plus_infinity()).is_minus_infinity());
    }

    #[test]
    fn time_delta_and_frequency() {
        assert_eq!(1 / Frequency::from_hertz(50), TimeDelta::from_millis(20));
        assert_eq!(1 / TimeDelta::from_millis(20), Frequency::from_hertz(50));
        assert_eq!(
            Frequency::from_kilo_hertz(200) * TimeDelta::from_millis(2),
            400.0
        );
    }
}
