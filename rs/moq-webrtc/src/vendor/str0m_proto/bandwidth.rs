use std::fmt;
use std::iter::Sum;
use std::ops::{Add, AddAssign, Div, Mul, Sub, SubAssign};
use std::time::Duration;

/// A data rate expressed as bits per second(bps).
///
/// Internally the value is tracked as a floating point number for accuracy in the presence of
/// repeated calculations that can yield decimal values.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Bitrate(f64);

impl Bitrate {
    /// A bitrate of zero bit/s.
    pub const ZERO: Self = Self::bps(0);
    /// The maximum bitrate that can be represented.
    pub const MAX: Self = Self::bps(u64::MAX);
    /// Positive infinity, useful as an invalid value with comparison semantics
    pub const INFINITY: Self = Self(f64::INFINITY);
    /// Negative infinity, useful as an invalid value with comparison semantics
    pub const NEG_INFINITY: Self = Self(f64::NEG_INFINITY);

    /// Create a bitrate of some bit per second(bps).
    pub const fn bps(bps: u64) -> Self {
        Bitrate(bps as f64)
    }

    /// Create a bitrate of some **Kilobits** per second(kbps).
    pub const fn kbps(kbps: u64) -> Self {
        Self::bps(kbps * 10_u64.pow(3))
    }

    /// Create a bitrate of some **Megabits** per second(mbps).
    pub const fn mbps(mbps: u64) -> Self {
        Self::bps(mbps * 10_u64.pow(6))
    }

    /// Create a bitrate of some **Gigabits** per second(gbps).
    pub const fn gbps(gbps: u64) -> Self {
        Self::bps(gbps * 10_u64.pow(9))
    }

    /// The number of bits per second as f64.
    pub fn as_f64(&self) -> f64 {
        self.0
    }

    /// The number of bits per second rounded upwards as u64.
    pub fn as_u64(&self) -> u64 {
        self.0.ceil() as u64
    }

    /// Clamp the value between a min and a max.
    pub fn clamp(&self, min: Self, max: Self) -> Self {
        Self(self.0.clamp(min.0, max.0))
    }

    /// Return the minimum bitrate between `self` and `other`.
    pub fn min(&self, other: Self) -> Self {
        Self(self.0.min(other.0))
    }

    /// Return the maximum bitrate between `self` and `other`.
    pub fn max(&self, other: Self) -> Self {
        Self(self.0.max(other.0))
    }

    /// Whether this bitrate is valid
    pub fn is_valid(&self) -> bool {
        self.0.is_finite()
    }

    /// Turn self into `Option<Bitrate>` based on its validity
    pub fn as_valid(&self) -> Option<Bitrate> {
        self.is_valid().then_some(*self)
    }

    /// Whether this value is zero
    pub fn is_zero(&self) -> bool {
        *self == Bitrate::ZERO
    }
}

impl From<u64> for Bitrate {
    fn from(value: u64) -> Self {
        Self::bps(value)
    }
}

impl From<f64> for Bitrate {
    fn from(value: f64) -> Self {
        Self(value)
    }
}

impl Mul<Duration> for Bitrate {
    type Output = DataSize;

    fn mul(self, rhs: Duration) -> Self::Output {
        let bits = self.0 * rhs.as_secs_f64();
        let bytes = bits / 8.0;

        DataSize::bytes(bytes.round() as i64)
    }
}

impl Mul<f64> for Bitrate {
    type Output = Bitrate;

    fn mul(self, rhs: f64) -> Self::Output {
        Bitrate(self.0 * rhs)
    }
}

impl Sub<Bitrate> for Bitrate {
    type Output = Bitrate;

    fn sub(self, rhs: Bitrate) -> Self::Output {
        assert!(
            self.0 >= rhs.0,
            "Attempted to subtract Bitrates that would result in overflow. lhs={}, rhs={}",
            self,
            rhs
        );

        Self(self.0 - rhs.0)
    }
}

impl Add<Bitrate> for Bitrate {
    type Output = Bitrate;

    fn add(self, rhs: Bitrate) -> Self::Output {
        Self(self.0 + rhs.0)
    }
}

impl fmt::Display for Bitrate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let rate = self.0;
        let log = rate.log10().floor() as u64;

        match log {
            0..=2 => write!(f, "{rate}bit/s"),
            3..=5 => write!(f, "{:.3}kbit/s", rate / 10.0_f64.powf(3.0)),
            6..=8 => write!(f, "{:.3}Mbit/s", rate / 10.0_f64.powf(6.0)),
            9..=11 => write!(f, "{:.3}Gbit/s", rate / 10.0_f64.powf(9.0)),
            12.. => write!(f, "{:.3}Tbit/s", rate / 10.0_f64.powf(12.0)),
        }
    }
}

/// A data size expressed in bytes.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DataSize(i64);

impl DataSize {
    /// A data size of zero bytes.
    pub const ZERO: Self = DataSize::bytes(0);

    /// Create a data size of some bytes.
    pub const fn bytes(bytes: i64) -> DataSize {
        Self(bytes)
    }

    /// Create a data size of some kilobytes.
    pub const fn kbytes(kbytes: i64) -> DataSize {
        Self(kbytes * 1024)
    }

    /// Create a data size of some megabytes.
    pub const fn mbytes(mbytes: i64) -> DataSize {
        Self(mbytes * 1024 * 1024)
    }

    /// Create a data size of some gigabytes.
    pub const fn gbytes(gbytes: i64) -> DataSize {
        Self(gbytes * 1024 * 1024 * 1024)
    }

    /// The number of bytes as f64.
    pub fn as_bytes_f64(&self) -> f64 {
        self.0 as f64
    }

    /// The number of bytes as usize.
    pub fn as_bytes_usize(&self) -> usize {
        self.0
            .try_into()
            .expect("DataSize i64 value must fit in usize")
    }

    /// The number of bytes as i64.
    pub fn as_bytes_i64(&self) -> i64 {
        self.0
    }

    /// Subtract, saturating at zero.
    pub fn saturating_sub(self, rhs: Self) -> Self {
        Self((self.0 - rhs.0).max(0))
    }

    /// The number of kilobytes as f64.
    pub fn as_kb(&self) -> f64 {
        self.0 as f64 / 1000.0
    }
}

impl From<usize> for DataSize {
    fn from(value: usize) -> Self {
        Self(value as i64)
    }
}

impl From<u8> for DataSize {
    fn from(value: u8) -> Self {
        Self(value as i64)
    }
}

impl Div<Duration> for DataSize {
    type Output = Bitrate;

    fn div(self, rhs: Duration) -> Self::Output {
        let bytes = self.as_bytes_f64();
        let s = rhs.as_secs_f64();

        if s == 0.0 {
            return Bitrate::ZERO;
        }

        let bps = (bytes * 8.0) / s;

        bps.into()
    }
}

impl Div<Bitrate> for DataSize {
    type Output = Duration;

    fn div(self, rhs: Bitrate) -> Self::Output {
        let bits = self.as_bytes_f64() * 8.0;
        let rhs = rhs.as_f64();

        if rhs == 0.0 {
            return Duration::ZERO;
        }

        let seconds = bits / rhs;

        Duration::from_secs_f64(seconds)
    }
}

impl Mul<i64> for DataSize {
    type Output = i64;

    fn mul(self, rhs: i64) -> Self::Output {
        self.as_bytes_i64() * rhs
    }
}

impl Div<f64> for Bitrate {
    type Output = Bitrate;

    fn div(self, rhs: f64) -> Self::Output {
        if rhs == 0.0 {
            return Self::ZERO;
        }

        Self(self.0 / rhs)
    }
}

impl Mul<u64> for DataSize {
    type Output = DataSize;

    fn mul(self, rhs: u64) -> Self::Output {
        Self(self.0 * rhs as i64)
    }
}

impl AddAssign<DataSize> for DataSize {
    fn add_assign(&mut self, rhs: DataSize) {
        self.0 += rhs.0;
    }
}

impl Sub<DataSize> for DataSize {
    type Output = DataSize;

    fn sub(self, rhs: DataSize) -> Self::Output {
        Self(self.0 - rhs.0)
    }
}

impl SubAssign<DataSize> for DataSize {
    fn sub_assign(&mut self, rhs: DataSize) {
        self.0 -= rhs.0;
    }
}

impl Add<DataSize> for DataSize {
    type Output = DataSize;

    fn add(self, rhs: DataSize) -> Self::Output {
        Self(self.0 + rhs.0)
    }
}

impl Sum<DataSize> for DataSize {
    fn sum<I: Iterator<Item = DataSize>>(iter: I) -> Self {
        iter.fold(DataSize::ZERO, |acc, s| acc + s)
    }
}

impl fmt::Display for DataSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let size = self.0 as f64;
        let log = (size as u64).ilog10();

        match log {
            0..=2 => write!(f, "{size}B"),
            3..=5 => write!(f, "{:.3}kB", size / 10.0_f64.powf(3.0)),
            6..=8 => write!(f, "{:.3}MB", size / 10.0_f64.powf(6.0)),
            9..=11 => write!(f, "{:.3}GB", size / 10.0_f64.powf(9.0)),
            12.. => write!(f, "{:.3}TB", size / 10.0_f64.powf(12.0)),
        }
    }
}

#[cfg(test)]
mod test {
    use std::time::Duration;

    use super::{Bitrate, DataSize};

    #[test]
    fn test_bitrate_display() {
        let rate = Bitrate::bps(1);
        assert_eq!(rate.to_string(), "1bit/s");

        let rate = Bitrate::bps(12);
        assert_eq!(rate.to_string(), "12bit/s");

        let rate = Bitrate::bps(123);
        assert_eq!(rate.to_string(), "123bit/s");

        let rate = Bitrate::bps(1234);
        assert_eq!(rate.to_string(), "1.234kbit/s");

        let rate = Bitrate::bps(12345);
        assert_eq!(rate.to_string(), "12.345kbit/s");

        let rate = Bitrate::bps(123456);
        assert_eq!(rate.to_string(), "123.456kbit/s");

        let rate = Bitrate::bps(1234567);
        assert_eq!(rate.to_string(), "1.235Mbit/s");

        let rate = Bitrate::bps(12345678);
        assert_eq!(rate.to_string(), "12.346Mbit/s");

        let rate = Bitrate::bps(123456789);
        assert_eq!(rate.to_string(), "123.457Mbit/s");

        let rate = Bitrate::bps(1234567898);
        assert_eq!(rate.to_string(), "1.235Gbit/s");

        let rate = Bitrate::bps(12345678987);
        assert_eq!(rate.to_string(), "12.346Gbit/s");

        let rate = Bitrate::bps(123456789876);
        assert_eq!(rate.to_string(), "123.457Gbit/s");

        let rate = Bitrate::bps(1234567898765);
        assert_eq!(rate.to_string(), "1.235Tbit/s");
    }

    #[test]
    fn test_data_size_div_duration() {
        let size = DataSize::bytes(2_500_000);
        let rate = size / Duration::from_secs(1);

        assert_eq!(rate.as_u64(), 20_000_000);
    }

    #[test]
    fn test_data_size_div_bitrate() {
        let size = DataSize::bytes(12_500);
        let rate = Bitrate::kbps(2_500);
        let duration = size / rate;

        assert_eq!(duration.as_millis(), 40);
    }

    #[test]
    fn test_bitrate_div_f64() {
        let rate = Bitrate::kbps(2_500);
        let new_rate = rate / 2.0;

        assert_eq!(new_rate, Bitrate::kbps(1250));
    }

    #[test]
    fn test_datasize_negative() {
        let size = DataSize::bytes(-100);
        assert_eq!(size.as_bytes_i64(), -100);

        // Test negative kilobytes
        let size_kb = DataSize::kbytes(-5);
        assert_eq!(size_kb.as_bytes_i64(), -5 * 1024);

        // Test that negative values work in comparisons
        let positive = DataSize::bytes(50);
        let negative = DataSize::bytes(-50);
        assert!(negative < positive);
        assert!(negative < DataSize::ZERO);
    }

    #[test]
    fn test_datasize_mul_negative() {
        let size = DataSize::bytes(100);

        // Multiply by -1 to negate
        let negated = size * -1i64;
        assert_eq!(negated, -100);

        // Multiply by -2
        let doubled_neg = size * -2i64;
        assert_eq!(doubled_neg, -200);

        // Negative size multiplied by -1
        let negative_size = DataSize::bytes(-50);
        let positive_result = negative_size * -1i64;
        assert_eq!(positive_result, 50);
    }

    #[test]
    fn test_datasize_arithmetic_with_negatives() {
        let positive = DataSize::bytes(100);
        let negative = DataSize::bytes(-50);

        // Add negative to positive
        let result = positive + negative;
        assert_eq!(result.as_bytes_i64(), 50);

        // Subtract negative from positive (should add)
        let result2 = positive - negative;
        assert_eq!(result2.as_bytes_i64(), 150);
    }
}
