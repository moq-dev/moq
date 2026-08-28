//! Consuming a binary value from a track.

use std::task::Poll;

use bytes::Bytes;

use crate::Result;

/// Configuration for a [`Consumer`].
///
/// Build from [`Default`] and override fields (the struct is `#[non_exhaustive]`, so new options
/// stay additive).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ConsumerConfig {
	/// Whether the frames are DEFLATE-compressed. Must match the producer's
	/// [`ProducerConfig::compression`](super::ProducerConfig::compression). Defaults to `false`.
	pub compression: bool,
}

impl ConsumerConfig {
	/// Set [`compression`](Self::compression) (a builder, since the struct is `#[non_exhaustive]`).
	pub fn with_compression(mut self, compression: bool) -> Self {
		self.compression = compression;
		self
	}
}

/// Consumes a binary value from a track, yielding the newest one.
///
/// Jumps to the newest group and reads the value out of it, so a late joiner starts at the current
/// value rather than replaying superseded ones.
pub struct Consumer {
	track: moq_net::track::Ordered,
	group: Option<moq_net::group::Consumer>,
	/// The DEFLATE decoder for the current group, `Some` while decompressing. A snapshot group is
	/// normally one frame, but the window is per group either way.
	flate: Option<moq_flate::Decoder>,
	compression: bool,
}

impl Consumer {
	/// Create a consumer reading from the given track subscriber.
	///
	/// Set [`ConsumerConfig::compression`] to read a track written by a producer with
	/// [`ProducerConfig::compression`](super::ProducerConfig::compression) on.
	pub fn new(track: moq_net::track::Subscriber, config: ConsumerConfig) -> Self {
		Self {
			track: track.ordered(),
			group: None,
			flate: None,
			compression: config.compression,
		}
	}

	/// Get the next value, or `None` once the track ends.
	pub async fn next(&mut self) -> Result<Option<Bytes>> {
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

	/// Poll for the next value, without blocking.
	///
	/// Jumps to the newest group and drains everything buffered in it, yielding only the last value:
	/// the earlier ones are already superseded, so a consumer that has fallen behind catches up to
	/// the head in a single step. A compressed group's frames are still decoded in order, since they
	/// share one window; only the yield is skipped. Switching to a newer group discards the older one.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Bytes>>> {
		// Drain to the newest group, starting a cold window whenever we switch.
		let track_finished = loop {
			match self.track.poll_next_group(waiter)? {
				Poll::Ready(Some(group)) => {
					self.group = Some(group);
					self.flate = self.compression.then(moq_flate::Decoder::new);
				}
				Poll::Ready(None) => break true,
				Poll::Pending => break false,
			}
		};

		// Decode every frame currently buffered in the group, keeping only the last.
		// `poll_read_frame` returns an owned `Poll`, so the borrow of `self.group` ends before the
		// match arms, leaving `decode` (and clearing the group) free to take `&mut self`.
		let mut latest = None;
		let mut group_pending = false;
		while let Some(group) = &mut self.group {
			match group.poll_read_frame(waiter)? {
				Poll::Ready(Some(frame)) => latest = Some(self.decode(&frame.payload)?),
				// The current group is exhausted; wait for a newer one.
				Poll::Ready(None) => {
					self.group = None;
					break;
				}
				// The group is still open but has nothing buffered yet.
				Poll::Pending => {
					group_pending = true;
					break;
				}
			}
		}

		if let Some(payload) = latest {
			return Poll::Ready(Ok(Some(payload)));
		}

		// An open group may still deliver frames even after the track finishes (it was appended before
		// the finish), so wait on it rather than ending the stream.
		if group_pending {
			return Poll::Pending;
		}

		match track_finished {
			true => Poll::Ready(Ok(None)),
			false => Poll::Pending,
		}
	}

	/// Decompress one frame, if the track is compressed.
	fn decode(&mut self, payload: &Bytes) -> Result<Bytes> {
		Ok(match self.flate.as_mut() {
			Some(flate) => flate.frame(payload)?,
			None => payload.clone(),
		})
	}
}
