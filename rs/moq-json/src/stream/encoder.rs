//! The track-free half of stream publishing: records in, frame payloads out.

use std::marker::PhantomData;

use bytes::Bytes;
use serde::Serialize;

use crate::Result;

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
	_marker: PhantomData<fn(T)>,
}

impl<T> Encoder<T> {
	/// Create an encoder with a cold window.
	pub fn new(config: ProducerConfig) -> Self {
		Self {
			flate: config.compression.then(moq_flate::Encoder::new),
			compression: config.compression,
			_marker: PhantomData,
		}
	}

	/// Start a cold DEFLATE window, for a caller that has just rolled a group.
	pub fn reset(&mut self) {
		self.flate = self.compression.then(moq_flate::Encoder::new);
	}
}

impl<T: Serialize> Encoder<T> {
	/// Encode one record into the next frame payload.
	pub fn encode(&mut self, value: &T) -> Result<Bytes> {
		let payload = serde_json::to_vec(value)?;
		Ok(match self.flate.as_mut() {
			Some(flate) => flate.frame(&payload),
			None => Bytes::from(payload),
		})
	}
}
