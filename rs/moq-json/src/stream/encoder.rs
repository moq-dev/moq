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
/// one leaves a gap in the log but nothing undecodable, and encoding continues.
#[must_use = "write and commit the record"]
pub struct Pending<'a, T> {
	_encoder: &'a mut Encoder<T>,
	payload: Bytes,
}

impl<T> Pending<'_, T> {
	/// The frame payload to write.
	pub fn payload(&self) -> &Bytes {
		&self.payload
	}

	/// Acknowledge that the record reached the wire.
	///
	/// Only call this once the write has actually succeeded.
	pub fn commit(self) {}
}

/// Encodes JSON records into frame payloads.
///
/// The track-free core of [`Producer`](super::Producer). Unlike
/// [`snapshot::Encoder`](crate::snapshot::Encoder) there are no group boundaries to report: a log is
/// an unbroken sequence of self-contained records, so every payload is simply the next frame.
///
/// Payloads must reach the wire in order. A group has no size cap, so the encoder keeps no budget.
pub struct Encoder<T> {
	_marker: PhantomData<fn(T)>,
}

impl<T> Encoder<T> {
	/// Create an encoder.
	pub fn new(_config: Config) -> Self {
		Self { _marker: PhantomData }
	}
}

impl<T: Serialize> Encoder<T> {
	/// Encode one record into the next frame payload.
	///
	/// The record comes back as a [`Pending`] the caller writes and then
	/// [`commit`](Pending::commit)s.
	///
	/// Errors with [`moq_net::Error::FrameTooLarge`] if the record is larger than one frame may be
	/// ([`moq_net::group::MAX_CACHE_BYTES`]). The refused record leaves the encoder untouched.
	pub fn encode(&mut self, value: &T) -> Result<Pending<'_, T>> {
		let payload = Bytes::from(serde_json::to_vec(value)?);
		if payload.len() as u64 > moq_net::group::MAX_CACHE_BYTES {
			return Err(moq_net::Error::FrameTooLarge.into());
		}

		Ok(Pending {
			_encoder: self,
			payload,
		})
	}
}
