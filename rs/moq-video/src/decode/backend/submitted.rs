//! Exact timestamps for codecs that carry only whole microseconds.
//!
//! A hardware decoder copies a timestamp from each access unit onto the picture
//! it becomes, but as an integer of microseconds. A [`Timestamp`] is an instant
//! at its own scale, and a 90 kHz or nanosecond tick is usually not a whole
//! microsecond, so rebuilding one from what the codec returns names a different
//! instant: it truncates, and a frame can land below its group's start.
//!
//! [`Submitted`] keeps the real timestamps beside the codec, keyed by the
//! microseconds that ride through it, so each picture gets back the exact one its
//! access unit went in with.

use std::collections::VecDeque;

use moq_net::{TimeOverflow, Timestamp};

use crate::Error;

/// How many access units may be waiting for a picture before the oldest is
/// forgotten.
///
/// Bounded by design, not by the deepest reordering any codec holds: an access
/// unit that never produces a picture (a dropped or corrupt one) costs a slot,
/// not the stream, and a peer cannot grow it.
const REMEMBERED: usize = 64;

/// The access units a codec has taken whose pictures are not out yet.
#[derive(Default)]
pub(super) struct Submitted {
	/// Oldest first, with the microseconds each one rides the codec under.
	units: VecDeque<(u64, Timestamp)>,
}

impl Submitted {
	/// The microseconds an access unit rides the codec under, refused for an
	/// [unscaled](Timestamp::unscaled) timestamp, which has no time to carry.
	///
	/// Two timestamps within one microsecond share a key and pair with their
	/// pictures in submission order, which is the nearest a whole-microsecond
	/// codec can tell them apart.
	pub(super) fn key(timestamp: Timestamp) -> Result<u64, TimeOverflow> {
		u64::try_from(timestamp.as_micros()?).map_err(|_| TimeOverflow)
	}

	/// Record an access unit handed to the codec under `key`, forgetting the
	/// oldest once [`REMEMBERED`] are outstanding.
	pub(super) fn remember(&mut self, key: u64, timestamp: Timestamp) {
		if self.units.len() == REMEMBERED {
			self.units.pop_front();
		}
		self.units.push_back((key, timestamp));
	}

	/// The timestamp the access unit behind a picture went in with, given the
	/// microseconds the codec returned with it.
	///
	/// Falls back to those microseconds when the codec stamped a picture with
	/// nothing that was submitted, which is the best that can be said of it.
	pub(super) fn restore(&mut self, key: u64) -> Result<Timestamp, Error> {
		// Searched rather than popped: a decoder is free to hand pictures back in
		// presentation order, which is not the order they went in.
		let found = self.units.iter().position(|(at, _)| *at == key);
		match found.and_then(|at| self.units.remove(at)) {
			Some((_, timestamp)) => Ok(timestamp),
			None => Ok(Timestamp::from_micros(key)?),
		}
	}

	/// Forget everything outstanding, once the codec has dropped or drained it.
	pub(super) fn clear(&mut self) {
		self.units.clear();
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// What a backend does around a submission.
	fn remember(submitted: &mut Submitted, timestamp: Timestamp) -> u64 {
		let key = Submitted::key(timestamp).unwrap();
		submitted.remember(key, timestamp);
		key
	}

	fn micros(micros: u64) -> Timestamp {
		Timestamp::from_micros(micros).unwrap()
	}

	/// A picture comes back with the timestamp its access unit went in with, at
	/// its own scale, however the codec truncated it. Pictures may also come back
	/// in presentation rather than submission order.
	#[test]
	fn a_picture_carries_its_access_unit_timestamp_unchanged() {
		let ninety_khz = moq_net::Timescale::new(90_000).unwrap();
		let first = Timestamp::new(3003, ninety_khz).unwrap();
		let second = Timestamp::new(6006, ninety_khz).unwrap();
		assert_ne!(micros(first.as_micros().unwrap() as u64), first);

		let mut submitted = Submitted::default();
		let first_key = remember(&mut submitted, first);
		let second_key = remember(&mut submitted, second);

		assert_eq!(submitted.restore(second_key).unwrap(), second);
		assert_eq!(submitted.restore(first_key).unwrap(), first);
		assert!(submitted.units.is_empty());
	}

	/// The same at nanosecond scale, where almost no tick is a whole microsecond.
	#[test]
	fn a_nanosecond_timestamp_survives() {
		let timestamp = Timestamp::from_nanos(33_333_333).unwrap();
		assert_ne!(micros(timestamp.as_micros().unwrap() as u64), timestamp);

		let mut submitted = Submitted::default();
		let key = remember(&mut submitted, timestamp);
		assert_eq!(submitted.restore(key).unwrap(), timestamp);
	}

	/// An unscaled timestamp has no time to carry, so it is refused before the
	/// codec sees the access unit, and nothing is left waiting for it.
	#[test]
	fn an_unscaled_timestamp_is_refused() {
		assert!(Submitted::key(Timestamp::unscaled(5)).is_err());
	}

	/// A picture stamped with nothing that was submitted still gets a timestamp,
	/// the codec's own, rather than failing the stream.
	#[test]
	fn an_unknown_picture_keeps_the_codec_timestamp() {
		let mut submitted = Submitted::default();
		remember(&mut submitted, micros(10));
		assert_eq!(submitted.restore(7).unwrap(), micros(7));
		// The one that was submitted is still waiting for its picture.
		assert_eq!(submitted.units.len(), 1);
	}

	/// What is remembered is bounded, and it is the oldest that is forgotten.
	#[test]
	fn remembered_access_units_are_bounded() {
		let mut submitted = Submitted::default();
		for at in 0..=REMEMBERED as u64 {
			remember(&mut submitted, micros(at));
		}
		assert_eq!(submitted.units.len(), REMEMBERED);
		assert_eq!(submitted.restore(0).unwrap(), micros(0));
		assert_eq!(submitted.units.len(), REMEMBERED);
		assert_eq!(submitted.restore(1).unwrap(), micros(1));
		assert_eq!(submitted.units.len(), REMEMBERED - 1);
	}

	/// Clearing drops what the codec dropped, so a stale entry never pairs with
	/// the next stream's picture.
	#[test]
	fn clear_forgets_outstanding_units() {
		let mut submitted = Submitted::default();
		let key = remember(&mut submitted, Timestamp::from_nanos(1_500).unwrap());
		submitted.clear();
		assert_eq!(submitted.restore(key).unwrap(), micros(1));
	}
}
