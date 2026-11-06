use bytes::Bytes;

use crate::Timestamp;

/// A value to publish, and optionally when it was captured.
///
/// Converts from a bare payload (bytes, or a `&V` to serialize), which stays untimed.
#[derive(Debug, Clone, Copy)]
pub struct Timed<P> {
	/// The value to publish.
	pub value: P,

	/// When the value was captured. `None` leaves it untimed.
	pub at: Option<Timestamp>,
}

impl<P> Timed<P> {
	/// Set when the value was captured.
	pub fn at(mut self, at: Timestamp) -> Self {
		self.at = Some(at);
		self
	}
}

impl<B: Into<Bytes>> From<B> for Timed<Bytes> {
	fn from(value: B) -> Self {
		Self {
			value: value.into(),
			at: None,
		}
	}
}

impl<'a, V> From<&'a V> for Timed<&'a V> {
	fn from(value: &'a V) -> Self {
		Self { value, at: None }
	}
}

impl From<u64> for Timed<u64> {
	fn from(value: u64) -> Self {
		Self { value, at: None }
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_bare_payload_converts_untimed() {
		let timed: Timed<Bytes> = vec![1u8, 2].into();
		assert_eq!(timed.value, Bytes::from_static(&[1, 2]));
		assert_eq!(timed.at, None);

		let stamped = Timed::from(&b"x"[..]).at(Timestamp::from_millis(5).unwrap());
		assert_eq!(stamped.at, Some(Timestamp::from_millis(5).unwrap()));

		let value = 7u32;
		let timed: Timed<&u32> = (&value).into();
		assert_eq!(*timed.value, 7);
	}
}
