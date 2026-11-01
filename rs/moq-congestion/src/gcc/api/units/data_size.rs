/*
 *  Copyright (c) 2019 The WebRTC project authors. All Rights Reserved.
 *
 *  Use of this source code is governed by a BSD-style license
 *  that can be found in the LICENSE file in the root of the source
 *  tree. An additional intellectual property rights grant can be found
 *  in the file PATENTS.  All contributing project authors may
 *  be found in the AUTHORS file in the root of the source tree.
 */

use std::fmt;

super::relative_unit!(DataSize);

impl DataSize {
    const ONE_SIDED: bool = true;

    pub const fn from_bytes(value: i64) -> Self {
        Self::from_value(value)
    }

    pub fn from_bytes_float(value: f64) -> Self {
        Self::from_value_float(value)
    }

    pub const fn infinity() -> Self {
        Self::plus_infinity()
    }

    pub const fn bytes(&self) -> i64 {
        self.to_value()
    }

    pub const fn bytes_float(&self) -> f64 {
        self.to_value_float()
    }

    pub const fn bytes_or(&self, fallback_value: i64) -> i64 {
        self.to_value_or(fallback_value)
    }

    pub const fn microbits(&self) -> i64 {
        const MAX_BEFORE_CONVERSION: i64 = i64::MAX / 8000000;
        assert!(
            self.bytes() <= MAX_BEFORE_CONVERSION,
            "size is too large to be expressed in microbits"
        );
        self.bytes() * 8000000
    }
}

impl fmt::Debug for DataSize {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.is_plus_infinity() {
            write!(f, "+inf bytes")
        } else if self.is_minus_infinity() {
            write!(f, "-inf bytes")
        } else {
            write!(f, "{} bytes", self.bytes())
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn const_expr() {
        const VALUE: i64 = 12345;
        const DATA_SIZE_ZERO: DataSize = DataSize::zero();
        const DATA_SIZE_INF: DataSize = DataSize::infinity();
        assert!(DataSize::default() == DATA_SIZE_ZERO);
        assert!(DATA_SIZE_ZERO.is_zero());
        assert!(DATA_SIZE_INF.is_infinite());
        assert!(DATA_SIZE_INF.bytes_or(-1) == -1);
        assert!(DATA_SIZE_INF > DATA_SIZE_ZERO);

        const DATA_SIZE: DataSize = DataSize::from_bytes(VALUE);
        assert!(DATA_SIZE.bytes_or(-1) == VALUE);

        assert_eq!(DATA_SIZE.bytes(), VALUE);
    }

    #[test]
    fn get_back_same_values() {
        const VALUE: i64 = 123 * 8;
        assert_eq!(DataSize::from_bytes(VALUE).bytes(), VALUE);
    }

    #[test]
    fn identity_checks() {
        const VALUE: i64 = 3000;
        assert!(DataSize::zero().is_zero());
        assert!(!DataSize::from_bytes(VALUE).is_zero());

        assert!(DataSize::infinity().is_infinite());
        assert!(!DataSize::zero().is_infinite());
        assert!(!DataSize::from_bytes(VALUE).is_infinite());

        assert!(!DataSize::infinity().is_finite());
        assert!(DataSize::from_bytes(VALUE).is_finite());
        assert!(DataSize::zero().is_finite());
    }

    #[test]
    fn comparison_operators() {
        const SMALL: i64 = 450;
        const LARGE: i64 = 451;
        let small: DataSize = DataSize::from_bytes(SMALL);
        let large: DataSize = DataSize::from_bytes(LARGE);

        assert_eq!(DataSize::zero(), DataSize::from_bytes(0));
        assert_eq!(DataSize::infinity(), DataSize::infinity());
        assert_eq!(small, small);
        assert!(small <= small);
        assert!(small >= small);
        assert!(small != large);
        assert!(small <= large);
        assert!(small < large);
        assert!(large >= small);
        assert!(large > small);
        assert!(DataSize::zero() < small);
        assert!(DataSize::infinity() > large);
    }

    #[test]
    fn converts_to_and_from_double() {
        const VALUE: i64 = 128;
        const DOUBLE_VALUE: f64 = VALUE as f64;

        assert_eq!(DataSize::from_bytes(VALUE).bytes_float(), DOUBLE_VALUE);
        assert_eq!(DataSize::from_bytes_float(DOUBLE_VALUE).bytes(), VALUE);

        const INFINITY: f64 = f64::INFINITY;
        assert_eq!(DataSize::infinity().bytes_float(), INFINITY);
        assert!(DataSize::from_bytes_float(INFINITY).is_infinite());
    }

    #[test]
    fn math_operations() {
        const VALUE_A: i64 = 450;
        const VALUE_B: i64 = 267;
        const SIZE_A: DataSize = DataSize::from_bytes(VALUE_A);
        const SIZE_B: DataSize = DataSize::from_bytes(VALUE_B);
        assert_eq!((SIZE_A + SIZE_B).bytes(), VALUE_A + VALUE_B);
        assert_eq!((SIZE_A - SIZE_B).bytes(), VALUE_A - VALUE_B);

        const INT32_VALUE: i32 = 123;
        const FLOAT_VALUE: f64 = 123.0;
        assert_eq!((SIZE_A * VALUE_B).bytes(), VALUE_A * VALUE_B);
        assert_eq!((SIZE_A * INT32_VALUE).bytes(), VALUE_A * INT32_VALUE as i64);
        assert_eq!(
            (SIZE_A * FLOAT_VALUE).bytes_float(),
            VALUE_A as f64 * FLOAT_VALUE
        );

        assert_eq!((SIZE_A / 10).bytes(), VALUE_A / 10);
        assert_eq!(SIZE_A / SIZE_B, VALUE_A as f64 / VALUE_B as f64);

        let mut mutable_size: DataSize = DataSize::from_bytes(VALUE_A);
        mutable_size += SIZE_B;
        assert_eq!(mutable_size.bytes(), VALUE_A + VALUE_B);
        mutable_size -= SIZE_A;
        assert_eq!(mutable_size.bytes(), VALUE_B);
    }
}
