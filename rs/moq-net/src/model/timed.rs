use bytes::Bytes;

use crate::Timestamp;

/// A value to publish, and optionally when it was captured.
///
/// `T` is the clock the time is on: a raw [`Timestamp`] by default, or whatever a higher layer
/// maps onto one (`moq-mux` takes a [`std::time::Instant`]). Converts from a bare payload (bytes,
/// or a `&V` to serialize), so a producer taking one still accepts the plain value, stamped when
/// written.
#[derive(Debug, Clone, Copy)]
pub struct Timed<P, T = Timestamp> {
	/// The value to publish.
	pub value: P,

	/// When the value was captured. `None` stamps it when written.
	pub at: Option<T>,
}

impl<P, T> Timed<P, T> {
	/// Set when the value was captured.
	pub fn at(mut self, at: T) -> Self {
		self.at = Some(at);
		self
	}
}

impl<B: Into<Bytes>, T> From<B> for Timed<Bytes, T> {
	fn from(value: B) -> Self {
		Self {
			value: value.into(),
			at: None,
		}
	}
}

impl<'a, V, T> From<&'a V> for Timed<&'a V, T> {
	fn from(value: &'a V) -> Self {
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
		let timed: Timed<&u32, std::time::Instant> = (&value).into();
		assert_eq!(*timed.value, 7);
	}
}
