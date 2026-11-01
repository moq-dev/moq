// UnitBase is a superclass in C++.
// The closest we can do in Rust is a macro, as traits don't support const.
macro_rules! unit_base {
    ($ty:ident) => {
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
        pub struct $ty(i64);

        impl $ty {
            pub const fn zero() -> Self {
                Self(0)
            }

            pub const fn plus_infinity() -> Self {
                Self(i64::MAX)
            }

            pub const fn minus_infinity() -> Self {
                Self(i64::MIN)
            }

            pub const fn is_zero(&self) -> bool {
                self.0 == 0
            }

            pub const fn is_finite(&self) -> bool {
                !self.is_infinite()
            }

            pub const fn is_infinite(&self) -> bool {
                self.0 == i64::MAX || self.0 == i64::MIN
            }

            pub const fn is_plus_infinity(&self) -> bool {
                self.0 == i64::MAX
            }

            pub const fn is_minus_infinity(&self) -> bool {
                self.0 == i64::MIN
            }

            pub const fn round_to(&self, resolution: Self) -> Self {
                assert!(self.is_finite());
                assert!(resolution.is_finite());
                assert!(resolution.0 > 0);
                Self::from_value(((self.0 + resolution.0 / 2) / resolution.0) * resolution.0)
            }

            pub const fn round_up_to(&self, resolution: Self) -> Self {
                assert!(self.is_finite());
                assert!(resolution.is_finite());
                assert!(resolution.0 > 0);
                Self::from_value(((self.0 + resolution.0 - 1) / resolution.0) * resolution.0)
            }

            pub const fn round_down_to(&self, resolution: Self) -> Self {
                assert!(self.is_finite());
                assert!(resolution.is_finite());
                assert!(resolution.0 > 0);
                Self::from_value((self.0 / resolution.0) * resolution.0)
            }

            #[allow(dead_code)]
            const fn from_fraction(denominator: i64, value: i64) -> Self {
                assert!(denominator >= 0);
                Self::from_value(value * denominator)
            }

            #[allow(dead_code)]
            fn from_fraction_float(denominator: f64, value: f64) -> Self {
                Self::from_value_float(value * denominator)
            }

            #[allow(dead_code)]
            const fn to_fraction(self, denominator: i64) -> i64 {
                self.divide_round_to_nearest(denominator)
            }

            const fn divide_round_to_nearest(&self, d: i64) -> i64 {
                assert!(d >= 0);

                let v = self.to_value();
                let mut result = v / d;
                let remainder = v % d;

                if remainder.abs() * 2 >= d.abs() {
                    if (v < 0) != (d < 0) {
                        result -= 1
                    } else {
                        result += 1
                    }
                }
                result
            }

            #[allow(dead_code)]
            fn to_fraction_float(self, denominator: f64) -> f64 {
                assert!(denominator >= 0.0);
                self.to_value_float() / denominator
            }

            #[allow(dead_code)]
            const fn to_fraction_or(self, denominator: i64, fallback_value: i64) -> i64 {
                assert!(denominator >= 0);
                if self.is_finite() {
                    self.divide_round_to_nearest(denominator)
                } else {
                    fallback_value
                }
            }

            pub const fn to_multiple(self, factor: i64) -> i64 {
                assert!(factor >= 0);
                self.to_value() * factor
            }

            pub fn to_multiple_float(self, factor: f64) -> f64 {
                assert!(factor >= 0.0);
                self.to_value_float() * factor
            }

            const fn from_value(value: i64) -> Self {
                assert!(value != i64::MAX && value != i64::MIN);
                if Self::ONE_SIDED {
                    assert!(value >= 0);
                }

                Self(value)
            }

            fn from_value_float(value: f64) -> Self {
                assert!(!value.is_nan());

                if value == f64::INFINITY {
                    return Self::plus_infinity();
                }

                if Self::ONE_SIDED {
                    assert!(value >= 0.0);
                }

                if value == f64::NEG_INFINITY {
                    Self::minus_infinity()
                } else {
                    Self(value as i64)
                }
            }

            const fn to_value(self) -> i64 {
                assert!(self.is_finite());
                self.0
            }

            #[allow(dead_code)]
            const fn to_value_or(self, fallback_value: i64) -> i64 {
                if self.is_finite() {
                    self.0
                } else {
                    fallback_value
                }
            }

            const fn to_value_float(self) -> f64 {
                if self.is_plus_infinity() {
                    f64::INFINITY
                } else if self.is_minus_infinity() {
                    f64::NEG_INFINITY
                } else {
                    self.0 as f64
                }
            }
        }
    };
}

macro_rules! relative_unit {
    ($ty:ident) => {
        crate::api::units::unit_base!($ty);

        impl $ty {
            pub fn clamp_mut(&mut self, min_value: Self, max_value: Self) {
                *self = Self(self.0.max(min_value.0).min(max_value.0));
            }
        }

        impl ::std::ops::Add for $ty {
            type Output = Self;

            fn add(self, rhs: Self) -> Self::Output {
                if self.is_plus_infinity() || rhs.is_plus_infinity() {
                    assert!(!self.is_minus_infinity());
                    assert!(!rhs.is_minus_infinity());
                    return Self::plus_infinity();
                } else if self.is_minus_infinity() || rhs.is_minus_infinity() {
                    assert!(!self.is_plus_infinity());
                    assert!(!rhs.is_plus_infinity());
                    return Self::minus_infinity();
                }
                Self::from_value(self.to_value() + rhs.to_value())
            }
        }

        impl ::std::ops::Sub for $ty {
            type Output = Self;

            fn sub(self, rhs: Self) -> Self::Output {
                if self.is_plus_infinity() || rhs.is_minus_infinity() {
                    assert!(!self.is_minus_infinity());
                    assert!(!rhs.is_plus_infinity());
                    return Self::plus_infinity();
                } else if self.is_minus_infinity() || rhs.is_plus_infinity() {
                    assert!(!self.is_plus_infinity());
                    assert!(!rhs.is_minus_infinity());
                    return Self::minus_infinity();
                }
                Self::from_value(self.to_value() - rhs.to_value())
            }
        }

        impl ::std::ops::AddAssign for $ty {
            fn add_assign(&mut self, rhs: Self) {
                *self = Self::from_value(self.to_value() + rhs.to_value());
            }
        }

        impl ::std::ops::SubAssign for $ty {
            fn sub_assign(&mut self, rhs: Self) {
                *self = Self::from_value(self.to_value() - rhs.to_value());
            }
        }

        impl ::std::ops::Div for $ty {
            type Output = f64;

            fn div(self, rhs: Self) -> Self::Output {
                self.to_value_float() / rhs.to_value_float()
            }
        }

        impl ::std::ops::Div<f64> for $ty {
            type Output = Self;

            fn div(self, rhs: f64) -> Self::Output {
                Self::from_value_float((self.to_value_float() / rhs).round())
            }
        }

        impl ::std::ops::Div<i64> for $ty {
            type Output = Self;

            fn div(self, rhs: i64) -> Self::Output {
                Self::from_value(self.to_value() / rhs)
            }
        }

        impl ::std::ops::Mul<isize> for $ty {
            type Output = Self;

            fn mul(self, rhs: isize) -> Self::Output {
                Self::from_value(self.to_value() * rhs as i64)
            }
        }

        impl ::std::ops::Mul<i64> for $ty {
            type Output = Self;

            fn mul(self, rhs: i64) -> Self::Output {
                Self::from_value(self.to_value() * rhs)
            }
        }

        impl ::std::ops::Mul<i32> for $ty {
            type Output = Self;

            fn mul(self, rhs: i32) -> Self::Output {
                Self::from_value(self.to_value() * rhs as i64)
            }
        }

        impl ::std::ops::Mul<i16> for $ty {
            type Output = Self;

            fn mul(self, rhs: i16) -> Self::Output {
                Self::from_value(self.to_value() * rhs as i64)
            }
        }

        impl ::std::ops::Mul<usize> for $ty {
            type Output = Self;

            fn mul(self, rhs: usize) -> Self::Output {
                Self::from_value(self.to_value() * i64::try_from(rhs).unwrap())
            }
        }

        impl ::std::ops::Mul<u64> for $ty {
            type Output = Self;

            fn mul(self, rhs: u64) -> Self::Output {
                Self::from_value(self.to_value() * i64::try_from(rhs).unwrap())
            }
        }

        impl ::std::ops::Mul<u32> for $ty {
            type Output = Self;

            fn mul(self, rhs: u32) -> Self::Output {
                Self::from_value(self.to_value() * i64::try_from(rhs).unwrap())
            }
        }

        impl ::std::ops::Mul<u16> for $ty {
            type Output = Self;

            fn mul(self, rhs: u16) -> Self::Output {
                Self::from_value(self.to_value() * i64::try_from(rhs).unwrap())
            }
        }

        impl ::std::ops::Mul<f64> for $ty {
            type Output = Self;

            fn mul(self, rhs: f64) -> Self::Output {
                Self::from_value_float((self.to_value_float() * rhs).round())
            }
        }

        impl ::std::ops::Mul<f32> for $ty {
            type Output = Self;

            fn mul(self, rhs: f32) -> Self::Output {
                Self::from_value_float((self.to_value_float() * rhs as f64).round())
            }
        }

        impl ::std::ops::Neg for $ty {
            type Output = Self;

            fn neg(self) -> Self::Output {
                if self.is_plus_infinity() {
                    Self::minus_infinity()
                } else if self.is_minus_infinity() {
                    Self::plus_infinity()
                } else {
                    Self::from_value(-self.to_value())
                }
            }
        }

        impl ::std::ops::Mul<$ty> for isize {
            type Output = $ty;

            fn mul(self, rhs: $ty) -> Self::Output {
                rhs * self
            }
        }

        impl ::std::ops::Mul<$ty> for i64 {
            type Output = $ty;

            fn mul(self, rhs: $ty) -> Self::Output {
                rhs * self
            }
        }

        impl ::std::ops::Mul<$ty> for i32 {
            type Output = $ty;

            fn mul(self, rhs: $ty) -> Self::Output {
                rhs * self
            }
        }

        impl ::std::ops::Mul<$ty> for i16 {
            type Output = $ty;

            fn mul(self, rhs: $ty) -> Self::Output {
                rhs * self
            }
        }

        impl ::std::ops::Mul<$ty> for usize {
            type Output = $ty;

            fn mul(self, rhs: $ty) -> Self::Output {
                rhs * self
            }
        }

        impl ::std::ops::Mul<$ty> for u64 {
            type Output = $ty;

            fn mul(self, rhs: $ty) -> Self::Output {
                rhs * self
            }
        }

        impl ::std::ops::Mul<$ty> for u32 {
            type Output = $ty;

            fn mul(self, rhs: $ty) -> Self::Output {
                rhs * self
            }
        }

        impl ::std::ops::Mul<$ty> for u16 {
            type Output = $ty;

            fn mul(self, rhs: $ty) -> Self::Output {
                rhs * self
            }
        }

        impl ::std::ops::Mul<$ty> for f64 {
            type Output = $ty;

            fn mul(self, rhs: $ty) -> Self::Output {
                rhs * self
            }
        }

        impl ::std::ops::Mul<$ty> for f32 {
            type Output = $ty;

            fn mul(self, rhs: $ty) -> Self::Output {
                rhs * self
            }
        }

        impl ::std::ops::MulAssign<isize> for $ty {
            fn mul_assign(&mut self, rhs: isize) {
                *self = *self * rhs;
            }
        }

        impl ::std::ops::MulAssign<i64> for $ty {
            fn mul_assign(&mut self, rhs: i64) {
                *self = *self * rhs;
            }
        }

        impl ::std::ops::MulAssign<i32> for $ty {
            fn mul_assign(&mut self, rhs: i32) {
                *self = *self * rhs;
            }
        }

        impl ::std::ops::MulAssign<i16> for $ty {
            fn mul_assign(&mut self, rhs: i16) {
                *self = *self * rhs;
            }
        }

        impl ::std::ops::MulAssign<usize> for $ty {
            fn mul_assign(&mut self, rhs: usize) {
                *self = *self * rhs;
            }
        }

        impl ::std::ops::MulAssign<u64> for $ty {
            fn mul_assign(&mut self, rhs: u64) {
                *self = *self * rhs;
            }
        }

        impl ::std::ops::MulAssign<u32> for $ty {
            fn mul_assign(&mut self, rhs: u32) {
                *self = *self * rhs;
            }
        }

        impl ::std::ops::MulAssign<u16> for $ty {
            fn mul_assign(&mut self, rhs: u16) {
                *self = *self * rhs;
            }
        }

        impl ::std::ops::MulAssign<f64> for $ty {
            fn mul_assign(&mut self, rhs: f64) {
                *self = *self * rhs;
            }
        }

        impl ::std::ops::MulAssign<f32> for $ty {
            fn mul_assign(&mut self, rhs: f32) {
                *self = *self * rhs;
            }
        }
    };
}

pub(crate) use relative_unit;
pub(crate) use unit_base;

#[cfg(test)]
mod test {
    use std::fmt;

    use approx::assert_relative_eq;

    use super::*;

    relative_unit!(TestUnit);

    impl TestUnit {
        const ONE_SIDED: bool = false;

        pub const fn from_kilo(kilo: i64) -> Self {
            Self::from_fraction(1000, kilo)
        }

        pub fn from_kilo_float(kilo: f64) -> Self {
            Self::from_fraction_float(1000.0, kilo)
        }

        pub const fn to_kilo(self) -> i64 {
            self.to_fraction(1000)
        }

        pub fn to_kilo_float(self) -> f64 {
            self.to_fraction_float(1000.0)
        }

        pub const fn to_kilo_or(self, fallback: i64) -> i64 {
            self.to_fraction_or(1000, fallback)
        }

        pub const fn to_milli(self) -> i64 {
            self.to_multiple(1000)
        }

        pub fn to_milli_float(self) -> f64 {
            self.to_multiple_float(1000.0)
        }
    }

    impl fmt::Debug for TestUnit {
        fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
            if self.is_plus_infinity() {
                write!(f, "+inf")
            } else if self.is_minus_infinity() {
                write!(f, "-inf")
            } else {
                write!(f, "{}", self.0)
            }
        }
    }

    fn test_unit_add_kilo(mut value: TestUnit, add_kilo: i64) -> TestUnit {
        value += TestUnit::from_kilo(add_kilo);
        value
    }

    #[test]
    fn const_expr() {
        const VALUE: i64 = -12345;
        const TEST_UNIT_ZERO: TestUnit = TestUnit::zero();
        const TEST_UNIT_PLUS_INF: TestUnit = TestUnit::plus_infinity();
        const TEST_UNIT_MINUS_INF: TestUnit = TestUnit::minus_infinity();

        assert!(TEST_UNIT_ZERO.is_zero());
        assert!(TEST_UNIT_PLUS_INF.is_plus_infinity());
        assert!(TEST_UNIT_MINUS_INF.is_minus_infinity());
        assert!(TEST_UNIT_PLUS_INF.to_kilo_or(-1) == -1);

        // Check from is constexpr for floats.
        assert!(TestUnit::from_value_float(0.0).is_zero());
        assert!(TestUnit::from_value_float(f64::INFINITY).is_plus_infinity());
        assert!(TestUnit::from_value_float(-f64::INFINITY).is_minus_infinity());
        assert!(TestUnit::from_value_float(250.0) == TestUnit::from_value(250));
        assert!(TestUnit::from_value_float(-250.0) == TestUnit::from_value(-250));

        assert!(TEST_UNIT_PLUS_INF > TEST_UNIT_ZERO);

        const TEST_UNIT_KILO: TestUnit = TestUnit::from_kilo(VALUE);
        const TEST_UNIT_VALUE: TestUnit = TestUnit::from_value(VALUE);

        assert!(TEST_UNIT_KILO.to_kilo_or(0) == VALUE);
        assert!(TEST_UNIT_VALUE.to_value_or(0) == VALUE);
        assert!(test_unit_add_kilo(TEST_UNIT_VALUE, 2).to_value() == VALUE + 2000);
        assert!(TestUnit::from_value(500) / 2 == TestUnit::from_value(250));
        assert!(TestUnit::from_value_float(500.0) / 2 == TestUnit::from_value_float(250.0));
    }

    #[test]
    fn get_back_same_values() {
        const VALUE: i64 = 499;
        for sign in [-1, 0, 1] {
            let value: i64 = VALUE * sign;
            assert_eq!(TestUnit::from_kilo(value).to_kilo(), value);
            assert_eq!(TestUnit::from_value(value).to_value(), value);
        }
        assert_eq!(TestUnit::zero().to_value(), 0);
    }

    #[test]
    fn get_different_prefix() {
        const VALUE: i64 = 3000000;
        assert_eq!(TestUnit::from_value(VALUE).to_kilo(), VALUE / 1000);
        assert_eq!(TestUnit::from_kilo(VALUE).to_value(), VALUE * 1000);
    }

    #[test]
    fn identity_checks() {
        const VALUE: i64 = 3000;
        assert!(TestUnit::zero().is_zero());
        assert!(!TestUnit::from_kilo(VALUE).is_zero());

        assert!(TestUnit::plus_infinity().is_infinite());
        assert!(TestUnit::minus_infinity().is_infinite());
        assert!(!TestUnit::zero().is_infinite());
        assert!(!TestUnit::from_kilo(-VALUE).is_infinite());
        assert!(!TestUnit::from_kilo(VALUE).is_infinite());

        assert!(!TestUnit::plus_infinity().is_finite());
        assert!(!TestUnit::minus_infinity().is_finite());
        assert!(TestUnit::from_kilo(-VALUE).is_finite());
        assert!(TestUnit::from_kilo(VALUE).is_finite());
        assert!(TestUnit::zero().is_finite());

        assert!(TestUnit::plus_infinity().is_plus_infinity());
        assert!(!TestUnit::minus_infinity().is_plus_infinity());

        assert!(TestUnit::minus_infinity().is_minus_infinity());
        assert!(!TestUnit::plus_infinity().is_minus_infinity());
    }

    #[test]
    fn comparison_operators() {
        const SMALL: i64 = 450;
        const LARGE: i64 = 451;
        let small: TestUnit = TestUnit::from_kilo(SMALL);
        let large: TestUnit = TestUnit::from_kilo(LARGE);

        assert_eq!(TestUnit::zero(), TestUnit::from_kilo(0));
        assert_eq!(TestUnit::plus_infinity(), TestUnit::plus_infinity());
        assert_eq!(small, TestUnit::from_kilo(SMALL));
        assert!(small <= TestUnit::from_kilo(SMALL));
        assert!(small >= TestUnit::from_kilo(SMALL));
        assert!(small != TestUnit::from_kilo(LARGE));
        assert!(small <= TestUnit::from_kilo(LARGE));
        assert!(small < TestUnit::from_kilo(LARGE));
        assert!(large >= TestUnit::from_kilo(SMALL));
        assert!(large > TestUnit::from_kilo(SMALL));
        assert!(TestUnit::zero() < small);
        assert!(TestUnit::zero() > TestUnit::from_kilo(-SMALL));
        assert!(TestUnit::zero() > TestUnit::from_kilo(-SMALL));

        assert!(TestUnit::plus_infinity() > large);
        assert!(TestUnit::minus_infinity() < TestUnit::zero());
    }

    #[test]
    fn can_be_inititialized_from_large_int() {
        const MAX_INT: i32 = i32::MAX;
        assert_eq!(
            TestUnit::from_kilo(MAX_INT as i64).to_value(),
            (MAX_INT as i64) * 1000
        );
    }

    #[test]
    fn converts_to_and_from_double() {
        const VALUE: i64 = 17017;
        const MILLI_DOUBLE: f64 = VALUE as f64 * 1e3;
        const VALUE_DOUBLE: f64 = VALUE as f64;
        const KILO_DOUBLE: f64 = VALUE as f64 * 1e-3;

        assert_eq!(TestUnit::from_value(VALUE).to_kilo_float(), KILO_DOUBLE);
        assert_eq!(TestUnit::from_kilo_float(KILO_DOUBLE).to_value(), VALUE);

        assert_eq!(TestUnit::from_value(VALUE).to_value_float(), VALUE_DOUBLE);
        assert_eq!(TestUnit::from_value_float(VALUE_DOUBLE).to_value(), VALUE);

        assert_relative_eq!(
            TestUnit::from_value(VALUE).to_milli_float(),
            MILLI_DOUBLE,
            epsilon = 1.0
        );

        const PLUS_INFINITY: f64 = f64::INFINITY;
        const MINUS_INFINITY: f64 = -PLUS_INFINITY;

        assert_eq!(TestUnit::plus_infinity().to_kilo_float(), PLUS_INFINITY);
        assert_eq!(TestUnit::minus_infinity().to_kilo_float(), MINUS_INFINITY);
        assert_eq!(TestUnit::plus_infinity().to_kilo_float(), PLUS_INFINITY);
        assert_eq!(TestUnit::minus_infinity().to_kilo_float(), MINUS_INFINITY);
        assert_eq!(TestUnit::plus_infinity().to_milli_float(), PLUS_INFINITY);
        assert_eq!(TestUnit::minus_infinity().to_milli_float(), MINUS_INFINITY);

        assert!(TestUnit::from_kilo_float(PLUS_INFINITY).is_plus_infinity());
        assert!(TestUnit::from_kilo_float(MINUS_INFINITY).is_minus_infinity());
        assert!(TestUnit::from_value_float(PLUS_INFINITY).is_plus_infinity());
        assert!(TestUnit::from_value_float(MINUS_INFINITY).is_minus_infinity());
    }

    #[test]
    #[should_panic]
    fn crashes_when_created_from_nan1() {
        TestUnit::from_value_float(f64::NAN);
    }

    #[test]
    #[should_panic]
    fn crashes_when_created_from_nan2() {
        #[allow(clippy::zero_divided_by_zero)]
        TestUnit::from_value_float(0.0 / 0.0);
    }

    #[test]
    #[should_panic]
    fn crashes_when_created_from_nan3() {
        TestUnit::from_value_float(f64::INFINITY - f64::INFINITY);
    }

    #[test]
    fn clamping() {
        const UPPER: TestUnit = TestUnit::from_value(800);
        const LOWER: TestUnit = TestUnit::from_value(100);
        const UNDER: TestUnit = TestUnit::from_value(100);
        const INSIDE: TestUnit = TestUnit::from_value(500);
        const OVER: TestUnit = TestUnit::from_value(1000);
        assert_eq!(UNDER.clamp(LOWER, UPPER), LOWER);
        assert_eq!(INSIDE.clamp(LOWER, UPPER), INSIDE);
        assert_eq!(OVER.clamp(LOWER, UPPER), UPPER);

        let mut mutable_delta: TestUnit = LOWER;
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
    fn math_operations() {
        const VALUE_A: i64 = 267;
        const VALUE_B: i64 = 450;
        const DELTA_A: TestUnit = TestUnit::from_kilo(VALUE_A);
        const DELTA_B: TestUnit = TestUnit::from_kilo(VALUE_B);
        assert_eq!((DELTA_A + DELTA_B).to_kilo(), VALUE_A + VALUE_B);
        assert_eq!((DELTA_A - DELTA_B).to_kilo(), VALUE_A - VALUE_B);

        const INT32_VALUE: i32 = 123;
        const FLOAT_VALUE: f64 = 123.0;
        assert_eq!(
            (TestUnit::from_value(VALUE_A) * VALUE_B).to_value(),
            VALUE_A * VALUE_B
        );
        assert_eq!(
            (TestUnit::from_value(VALUE_A) * INT32_VALUE).to_value(),
            VALUE_A * INT32_VALUE as i64
        );
        assert_eq!(
            (TestUnit::from_value(VALUE_A) * FLOAT_VALUE).to_value(),
            VALUE_A * FLOAT_VALUE as i64
        );

        assert_eq!((DELTA_B / 10).to_kilo(), VALUE_B / 10);
        assert_eq!(DELTA_B / DELTA_A, VALUE_B as f64 / VALUE_A as f64);

        let mut mutable_delta: TestUnit = TestUnit::from_kilo(VALUE_A);
        mutable_delta += TestUnit::from_kilo(VALUE_B);
        assert_eq!(mutable_delta, TestUnit::from_kilo(VALUE_A + VALUE_B));
        mutable_delta -= TestUnit::from_kilo(VALUE_B);
        assert_eq!(mutable_delta, TestUnit::from_kilo(VALUE_A));

        // Division by an int rounds towards zero to follow regular int division.
        assert_eq!(TestUnit::from_value(789) / 10, TestUnit::from_value(78));
        assert_eq!(TestUnit::from_value(-789) / 10, TestUnit::from_value(-78));
    }

    #[test]
    fn infinity_operations() {
        const VALUE: i64 = 267;
        const FINITE: TestUnit = TestUnit::from_value(VALUE);
        assert!((TestUnit::plus_infinity() + FINITE).is_plus_infinity());
        assert!((TestUnit::plus_infinity() - FINITE).is_plus_infinity());
        assert!((FINITE + TestUnit::plus_infinity()).is_plus_infinity());
        assert!((FINITE - TestUnit::minus_infinity()).is_plus_infinity());

        assert!((TestUnit::minus_infinity() + FINITE).is_minus_infinity());
        assert!((TestUnit::minus_infinity() - FINITE).is_minus_infinity());
        assert!((FINITE + TestUnit::minus_infinity()).is_minus_infinity());
        assert!((FINITE - TestUnit::plus_infinity()).is_minus_infinity());
    }

    #[test]
    fn unary_minus() {
        const VALUE: i64 = 1337;
        const UNIT: TestUnit = TestUnit::from_value(VALUE);
        assert_eq!(-UNIT.to_value(), -VALUE);

        // Check infinity.
        assert_eq!(-TestUnit::plus_infinity(), TestUnit::minus_infinity());
        assert_eq!(-TestUnit::minus_infinity(), TestUnit::plus_infinity());
    }
}
