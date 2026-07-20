//! RTMP timestamps are 32 byte unsigned integers representing the number of milliseconds from
//! and unknown epoch.
//!
//! Since it's meant to support streams that can go on forever, timestamps have to work with
//! time values that overflow and underflow a 32 bit integer but still be able to do comparisons.
//! To support this the `RtmpTimestamp` struct was created to abstract away the calculations
//! and make it easy to work with RTMP timestamps.
//!
//! According to the RTMP spec, times are adjacent if they are within 2<sup>31</sup> - 1 milliseconds
//! of each other.
//!
//! # Examples
//!
//! Basic arithmetic and comparison support:
//!
//! ```ignore
//! use rml_rtmp::time::RtmpTimestamp;
//!
//! let time1 = RtmpTimestamp::new(10);
//! let time2 = RtmpTimestamp::new(20);
//! let time3 = RtmpTimestamp::new(30);
//! let mut time4 = RtmpTimestamp::new(10);
//!
//! assert!(time1 < time2);
//! assert_eq!(time3, time1 + time2);
//! assert_eq!(time2, time1 + 10);
//!
//! time4.set(30);
//! assert_eq!(RtmpTimestamp::new(30), time4);
//! ```
//!
//! Value Wrapping support:
//!
//! ```ignore
//! use rml_rtmp::time::RtmpTimestamp;
//!
//! let time1 = RtmpTimestamp::new(10000);
//! let time2 = RtmpTimestamp::new(4000000000);
//! let time3 = RtmpTimestamp::new(3000000000);
//!
//! assert!(time1 > time2);
//! assert!(time3 < time2);
//! ```
//!
//! For ease of use, a `RtmpTimestamp` can be directly compared to u32s:
//!
//! ```ignore
//! use rml_rtmp::time::RtmpTimestamp;
//!
//! let time = RtmpTimestamp::new(50);
//!
//! assert!(time < 60);
//! assert!(time > 20);
//! assert!(time == 50);
//! ```

use std::cmp::{Ordering, max, min};
use std::num::Wrapping;
use std::ops::{Add, Sub};

/// The representation of a RTMP timestamp
#[derive(Eq, PartialEq, Debug, Copy, Clone)]
pub struct RtmpTimestamp {
	/// The time (as milliseconds from an unknown epoch) being represented by the timestamp
	pub value: u32,
}

impl RtmpTimestamp {
	/// Creates a new timestamp with the specified time value
	pub fn new(initial_value: u32) -> Self {
		RtmpTimestamp { value: initial_value }
	}

	/// Sets the timestamp to a new time value
	pub fn set(&mut self, new_value: u32) {
		self.value = new_value;
	}
}

impl Add for RtmpTimestamp {
	type Output = RtmpTimestamp;

	fn add(self, other: RtmpTimestamp) -> Self {
		RtmpTimestamp {
			value: add_values(self.value, other.value),
		}
	}
}

impl Add<u32> for RtmpTimestamp {
	type Output = RtmpTimestamp;

	fn add(self, other: u32) -> Self {
		RtmpTimestamp {
			value: add_values(self.value, other),
		}
	}
}

impl Sub for RtmpTimestamp {
	type Output = RtmpTimestamp;

	fn sub(self, other: RtmpTimestamp) -> Self {
		RtmpTimestamp {
			value: sub_values(self.value, other.value),
		}
	}
}

impl Sub<u32> for RtmpTimestamp {
	type Output = RtmpTimestamp;

	fn sub(self, other: u32) -> Self {
		RtmpTimestamp {
			value: sub_values(self.value, other),
		}
	}
}

impl Ord for RtmpTimestamp {
	fn cmp(&self, other: &Self) -> Ordering {
		compare(&self.value, &other.value)
	}
}

impl PartialOrd for RtmpTimestamp {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(compare(&self.value, &other.value))
	}
}

impl PartialEq<u32> for RtmpTimestamp {
	fn eq(&self, other: &u32) -> bool {
		self.value == *other
	}
}

impl PartialEq<RtmpTimestamp> for u32 {
	fn eq(&self, other: &RtmpTimestamp) -> bool {
		self == &other.value
	}
}

impl PartialOrd<u32> for RtmpTimestamp {
	fn partial_cmp(&self, other: &u32) -> Option<Ordering> {
		Some(compare(&self.value, other))
	}
}

impl PartialOrd<RtmpTimestamp> for u32 {
	fn partial_cmp(&self, other: &RtmpTimestamp) -> Option<Ordering> {
		Some(compare(&self, &other.value))
	}
}

fn add_values(value1: u32, value2: u32) -> u32 {
	(Wrapping(value1) + Wrapping(value2)).0
}

fn sub_values(value1: u32, value2: u32) -> u32 {
	(Wrapping(value1) - Wrapping(value2)).0
}

fn compare(value1: &u32, value2: &u32) -> Ordering {
	const MAX_ADJACENT_VALUE: u32 = 2147483647; //2u32.pow(31) - 1

	let max_val = max(value1, value2);
	let min_val = min(value1, value2);
	let difference = max_val - min_val;
	match difference <= MAX_ADJACENT_VALUE {
		true => value1.cmp(value2),
		false => value2.cmp(value1),
	}
}

#[cfg(test)]
mod tests {
	use super::RtmpTimestamp;

	#[test]
	fn two_timestamps_can_be_added_together() {
		let time1 = RtmpTimestamp::new(50);
		let time2 = RtmpTimestamp::new(60);
		let result = time1 + time2;

		assert_eq!(result.value, 110);
	}

	#[test]
	fn can_add_number_to_timestamp() {
		let time = RtmpTimestamp::new(50);
		let result = time + 60;

		assert_eq!(result.value, 110);
	}

	#[test]
	fn can_add_timestamps_that_overflow_u32() {
		let time1 = RtmpTimestamp::new(u32::max_value());
		let time2 = RtmpTimestamp::new(60);
		let result = time1 + time2;

		assert_eq!(result.value, 59);
	}

	#[test]
	fn can_add_number_to_timestamp_that_overflows_u32() {
		let time = RtmpTimestamp::new(u32::max_value());
		let result = time + 60;

		assert_eq!(result.value, 59);
	}

	#[test]
	fn two_timestamps_can_be_subtracted_from_each_other() {
		let time1 = RtmpTimestamp::new(60);
		let time2 = RtmpTimestamp::new(50);
		let result = time1 - time2;

		assert_eq!(result.value, 10);
	}

	#[test]
	fn can_subtract_number_from_timestamp() {
		let time = RtmpTimestamp::new(60);
		let result = time - 50;

		assert_eq!(result.value, 10);
	}

	#[test]
	fn can_subtract_timestamps_that_underflow() {
		let time1 = RtmpTimestamp::new(0);
		let time2 = RtmpTimestamp::new(50);
		let result = time1 - time2;

		assert_eq!(result.value, u32::max_value() - 49);
	}

	#[test]
	fn can_subtract_number_from_timestamp_that_underflow_u32() {
		let time = RtmpTimestamp::new(0);
		let result = time - 50;

		assert_eq!(result.value, u32::max_value() - 49);
	}

	#[test]
	fn can_do_basic_comparisons_of_timestamps() {
		let time1 = RtmpTimestamp::new(50);
		let time2 = RtmpTimestamp::new(60);

		assert!(time1 < time2, "time1 was not less than time2");
		assert!(time2 > time1, "time2 was not greater than time2");
		assert_eq!(
			time1,
			RtmpTimestamp::new(50),
			"Two timestamps with the same time were not equal"
		);
	}

	#[test]
	fn can_do_comparisons_with_timestamps_that_wrap_around() {
		let time1 = RtmpTimestamp::new(10000);
		let time2 = RtmpTimestamp::new(4000000000);
		let time3 = RtmpTimestamp::new(3000000000);

		assert!(time1 > time2, "10000 was not marked as greater than 4000000000");
		assert!(time3 < time2, "4000000000 was not marked greater than 3000000000");
	}

	#[test]
	fn can_compare_timestamps_with_u32() {
		let time1 = RtmpTimestamp::new(50);

		assert!(time1 < 60, "time1 was not less than 60");
		assert!(time1 > 20, "time1 was not greater than 20");
		assert_eq!(time1, 50, "time1 was not equal to 50");
	}

	#[test]
	fn can_set_timestamp_value() {
		let mut time = RtmpTimestamp::new(50);
		time.set(60);

		assert_eq!(time, 60);
	}
}
