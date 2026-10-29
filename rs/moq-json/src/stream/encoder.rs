//! The track-free half of stream publishing: records in, frame payloads out.

use std::marker::PhantomData;

use bytes::Bytes;
use serde::Serialize;

use crate::Result;

/// Codec options for an [`Encoder`], and so for the [`Producer`](super::Producer) wrapping one.
///
/// Empty for now. Build from [`Default`]; the struct is `#[non_exhaustive]`, so new options stay
/// additive.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Config {}

/// An encoded record the caller has not yet acknowledged writing.
///
/// Returned by [`Encoder::encode`]. Write the [`payload`](Self::payload), then
/// [`commit`](Self::commit).
///
/// A frame that is never committed never reached the wire. Each record stands alone, so a dropped
/// one leaves a gap in the log but nothing undecodable, and encoding continues; it is just not
/// charged to the group's budget.
#[must_use = "write and commit the record so it counts against the group's budget"]
pub struct Pending<'a, T> {
	encoder: &'a mut Encoder<T>,
	payload: Bytes,
}

impl<T> Pending<'_, T> {
	/// The frame payload to write.
	pub fn payload(&self) -> &Bytes {
		&self.payload
	}

	/// Acknowledge that the record reached the wire, charging it to the group's budget.
	///
	/// Only call this once the write has actually succeeded.
	pub fn commit(self) {
		self.encoder.frames += 1;
		self.encoder.bytes += self.payload.len() as u64;
	}
}

/// Encodes JSON records into frame payloads.
///
/// The track-free core of [`Producer`](super::Producer). Unlike
/// [`snapshot::Encoder`](crate::snapshot::Encoder) there are no group boundaries to report: a log is
/// an unbroken sequence of self-contained records, so every payload is simply the next frame.
///
/// Payloads must reach the wire in order. If the caller does roll a group, call
/// [`reset`](Self::reset) so the budget restarts with it.
pub struct Encoder<T> {
	/// Frames and bytes committed to the current group, checked against moq-net's group budget
	/// before each record is encoded.
	frames: usize,
	bytes: u64,

	_marker: PhantomData<fn(T)>,
}

impl<T> Encoder<T> {
	/// Create an encoder for an empty group.
	pub fn new(_config: Config) -> Self {
		Self {
			frames: 0,
			bytes: 0,
			_marker: PhantomData,
		}
	}

	/// Restart the group's budget, for a caller that has just rolled a group.
	pub fn reset(&mut self) {
		self.frames = 0;
		self.bytes = 0;
	}
}

impl<T: Serialize> Encoder<T> {
	/// Encode one record into the next frame payload.
	///
	/// The record comes back as a [`Pending`] the caller writes and then
	/// [`commit`](Pending::commit)s.
	///
	/// Errors with [`moq_net::Error::GroupTooLarge`] if the record does not fit in what is left of
	/// the group's budget ([`moq_net::group::MAX_CACHE_BYTES`] and
	/// [`moq_net::group::MAX_GROUP_FRAMES`]), counting every record committed since the last
	/// [`reset`](Self::reset). The refused record leaves the encoder untouched.
	pub fn encode(&mut self, value: &T) -> Result<Pending<'_, T>> {
		let payload = Bytes::from(serde_json::to_vec(value)?);
		if self.frames >= moq_net::group::MAX_GROUP_FRAMES
			|| self.bytes.saturating_add(payload.len() as u64) > moq_net::group::MAX_CACHE_BYTES
		{
			return Err(moq_net::Error::GroupTooLarge.into());
		}

		Ok(Pending { encoder: self, payload })
	}
}
