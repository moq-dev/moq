//! The track-free half of stream publishing: records in, frame payloads out.

use std::marker::PhantomData;

use bytes::Bytes;
use serde::Serialize;

use crate::{Compression, Error, Result};

/// Codec options for an [`Encoder`], and so for the [`Producer`](super::Producer) wrapping one.
///
/// Build from [`Default`] and override fields (the struct is `#[non_exhaustive]`, so new
/// options stay additive).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Config {
	/// Compress the group as one sync-flushed DEFLATE stream, so each record reuses the earlier
	/// ones as context and shrinks sharply.
	///
	/// [`Compression::None`] (the default) emits plaintext JSON frames. A [`Decoder`](super::Decoder)
	/// reading them must set the same [`compression`](Self::compression).
	pub compression: Compression,
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
#[must_use = "write and commit the record; an uncommitted compressed record stops the encoder"]
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
		self.encoder.frames += 1;
		self.encoder.bytes += self.payload.len() as u64;
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

	/// Frames and payload bytes committed to the current group, checked against moq-net's group
	/// budget before each record is encoded.
	frames: usize,
	bytes: u64,

	_marker: PhantomData<fn(T)>,
}

impl<T> Encoder<T> {
	/// Create an encoder with a cold window.
	pub fn new(config: Config) -> Self {
		Self {
			flate: config.compression.is_deflate().then(moq_flate::Encoder::new),
			compression: config.compression.is_deflate(),
			desynced: false,
			frames: 0,
			bytes: 0,
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
		self.frames = 0;
		self.bytes = 0;
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
	///
	/// Errors with [`moq_net::Error::GroupTooLarge`] if the record might not fit in what is left of
	/// the group's budget ([`moq_net::group::MAX_CACHE_BYTES`] and
	/// [`moq_net::group::MAX_GROUP_FRAMES`]), counting every record committed since the last
	/// [`reset`](Self::reset). The refused record leaves the encoder untouched.
	pub fn encode(&mut self, value: &T) -> Result<Pending<'_, T>> {
		if self.desynced {
			return Err(Error::Desync);
		}

		let bytes = serde_json::to_vec(value)?;

		// Check before compressing: encoding advances the window, so a record refused afterwards
		// would leave the encoder ahead of every reader. The worst case is checked rather than the
		// actual size for the same reason. The budget is also below moq-flate's per-frame decode cap,
		// so any record that fits is one every consumer can inflate.
		let size = bytes.len() as u64;
		let bound = if self.compression { deflate_bound(size) } else { size };
		if self.frames >= moq_net::group::MAX_GROUP_FRAMES
			|| self.bytes.saturating_add(bound) > moq_net::group::MAX_CACHE_BYTES
		{
			return Err(moq_net::Error::GroupTooLarge.into());
		}

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

/// The largest a sync-flushed DEFLATE frame of `len` raw bytes can grow to.
///
/// zlib's `deflateBound` for its default window and memory level, which both moq-flate and the
/// browser's pako use: incompressible input falls back to stored blocks, 5 bytes per 16 KiB. The
/// constant covers the block headers and the flush, whose fixed 4-byte marker is stripped anyway.
fn deflate_bound(len: u64) -> u64 {
	len + (len >> 12) + (len >> 14) + (len >> 25) + 13
}

#[cfg(test)]
mod test {
	use super::*;

	/// The bound holds for incompressible input, the worst case, at sizes straddling the 16 KiB
	/// block boundary, and on a window already primed by earlier frames.
	#[test]
	fn deflate_bound_covers_incompressible_frames() {
		// xorshift: incompressible enough to force stored blocks, and deterministic.
		let mut state = 0x9e37_79b9_7f4a_7c15u64;
		let mut noise = |len: usize| {
			(0..len)
				.map(|_| {
					state ^= state << 13;
					state ^= state >> 7;
					state ^= state << 17;
					state as u8
				})
				.collect::<Vec<u8>>()
		};

		let mut flate = moq_flate::Encoder::new();
		for len in [1, 2, 100, 16_383, 16_384, 16_385, 65_535, 65_536, 1 << 20, 3 << 20] {
			let payload = flate.frame(&noise(len));
			assert!(
				payload.len() as u64 <= deflate_bound(len as u64),
				"{len} raw bytes deflated to {}, past the bound {}",
				payload.len(),
				deflate_bound(len as u64)
			);
		}
	}
}
