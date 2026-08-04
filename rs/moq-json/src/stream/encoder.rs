//! The track-free half of stream publishing: records in, frame payloads out.

use std::marker::PhantomData;

use bytes::Bytes;
use serde::Serialize;

use crate::{Error, Result};

/// Configuration for an [`Encoder`], and so for the [`Producer`](super::Producer) wrapping one.
///
/// Build from [`Default`] and override fields (the struct is `#[non_exhaustive]`, so new
/// options stay additive).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ProducerConfig {
	/// Compress the group as one sync-flushed DEFLATE stream, so each record reuses the earlier
	/// ones as context and shrinks sharply.
	///
	/// `false` (the default) emits plaintext JSON frames. A [`Decoder`](super::Decoder) reading them
	/// must set [`ConsumerConfig::compression`](super::ConsumerConfig::compression) to match.
	pub compression: bool,
}

impl ProducerConfig {
	/// Set [`compression`](Self::compression) (a builder, since the struct is `#[non_exhaustive]`).
	pub fn with_compression(mut self, compression: bool) -> Self {
		self.compression = compression;
		self
	}
}

/// An encoded record the caller has not yet acknowledged writing.
///
/// Returned by [`Encoder::encode`]. Write the [`payload`](Self::payload), then
/// [`commit`](Self::commit).
///
/// A frame that is never committed never reached the wire. With compression on that is
/// unrecoverable within the group: the window is ahead of what the consumer holds, and a log has no
/// keyframe to resynchronize on the way [`snapshot`](crate::snapshot) does. So the encoder refuses
/// to encode anything further ([`Error::Desync`]) until the caller rolls a new group and calls
/// [`Encoder::reset`]. Without compression each record stands alone, so a dropped one leaves a gap
/// in the log but nothing undecodable, and encoding continues.
#[must_use = "the record must be written and committed, or the encoder will refuse to continue"]
pub struct Pending<'a, T> {
	encoder: &'a mut Encoder<T>,
	payload: Bytes,
	committed: bool,
}

impl<T> Pending<'_, T> {
	/// The frame payload to write.
	pub fn payload(&self) -> &Bytes {
		&self.payload
	}

	/// Acknowledge that the record reached the wire, keeping the encoder's window.
	///
	/// Only call this once the write has actually succeeded.
	pub fn commit(mut self) {
		self.committed = true;
	}
}

impl<T> Drop for Pending<'_, T> {
	fn drop(&mut self) {
		if !self.committed {
			self.encoder.desync();
		}
	}
}

/// Encodes JSON records into frame payloads, sharing one DEFLATE window across the log.
///
/// The track-free core of [`Producer`](super::Producer). Unlike
/// [`snapshot::Encoder`](crate::snapshot::Encoder) there are no group boundaries to report: a log is
/// an unbroken sequence of self-contained records, so every payload is simply the next frame.
///
/// The window spans everything encoded so far, so payloads must reach the wire in order and be
/// decoded in the same order. If the caller does roll a group, call [`reset`](Self::reset) so the
/// next record starts a cold window that the new group's decoder can follow.
pub struct Encoder<T> {
	/// The DEFLATE encoder (one window for the whole log), `Some` while compressing.
	flate: Option<moq_flate::Encoder>,
	compression: bool,

	/// Set when a compressed record was encoded but never written. The window is then ahead of the
	/// consumer for the rest of the group, so encoding stops until the caller rolls a new one.
	desynced: bool,

	_marker: PhantomData<fn(T)>,
}

impl<T> Encoder<T> {
	/// Create an encoder with a cold window.
	pub fn new(config: ProducerConfig) -> Self {
		Self {
			flate: config.compression.then(moq_flate::Encoder::new),
			compression: config.compression,
			desynced: false,
			_marker: PhantomData,
		}
	}

	/// Start a cold DEFLATE window, for a caller that has just rolled a group.
	///
	/// This is also how a caller clears an [`Error::Desync`]: roll a new group so the consumer starts
	/// its own cold window, then reset.
	pub fn reset(&mut self) {
		self.flate = self.compression.then(moq_flate::Encoder::new);
		self.desynced = false;
	}

	/// Mark the window as ahead of the consumer, after a record that was never written.
	///
	/// Only meaningful while compressing: an uncompressed record carries no shared state, so losing
	/// one leaves a gap in the log rather than an undecodable stream.
	fn desync(&mut self) {
		self.desynced = self.compression;
	}
}

impl<T: Serialize> Encoder<T> {
	/// Encode one record into the next frame payload.
	///
	/// The record comes back as a [`Pending`] the caller writes and then
	/// [`commit`](Pending::commit)s. Errors with [`Error::Desync`] if a previous compressed record
	/// was left uncommitted, since every frame after it would be undecodable.
	pub fn encode(&mut self, value: &T) -> Result<Pending<'_, T>> {
		if self.desynced {
			return Err(Error::Desync);
		}

		let bytes = serde_json::to_vec(value)?;
		let payload = match self.flate.as_mut() {
			Some(flate) => flate.frame(&bytes),
			None => Bytes::from(bytes),
		};

		Ok(Pending {
			encoder: self,
			payload,
			committed: false,
		})
	}
}
