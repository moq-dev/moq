use std::{fmt, time};

use libc::{time_t, timeval};

#[derive(Debug, Default, Clone, Copy)]
/// Timestamp consisting of a seconds and a microseconds component
pub struct Timestamp {
	pub sec: time_t,
	pub usec: time_t,
}

impl Timestamp {
	/// Returns a timestamp representation
	///
	/// # Arguments
	///
	/// * `sec` - Seconds
	/// * `usec` - Microseconds
	///
	/// # Example
	///
	/// ```
	/// use moq_v4l::timestamp::Timestamp;
	/// let ts = Timestamp::new(5, 5);
	/// ```
	pub fn new(sec: time_t, usec: time_t) -> Self {
		Timestamp { sec, usec }
	}
}

impl fmt::Display for Timestamp {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		let floating: f64 = self.sec as f64 + self.usec as f64 / 1_000_000.0;
		write!(f, "{} [s]", floating)
	}
}

impl From<timeval> for Timestamp {
	fn from(tv: timeval) -> Self {
		Self {
			sec: tv.tv_sec as time_t,
			usec: tv.tv_usec as time_t,
		}
	}
}

impl From<Timestamp> for timeval {
	fn from(timestamp: Timestamp) -> Self {
		Self {
			tv_sec: timestamp.sec as time_t,
			tv_usec: timestamp.usec as time_t,
		}
	}
}

impl From<time::Duration> for Timestamp {
	fn from(duration: time::Duration) -> Self {
		Self::new(duration.as_secs() as time_t, duration.subsec_micros() as time_t)
	}
}

impl From<Timestamp> for time::Duration {
	fn from(ts: Timestamp) -> Self {
		Self::new(ts.sec as u64, (ts.usec * 1000) as u32)
	}
}
