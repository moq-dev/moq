//! Consuming an ordered log of binary payloads from a track.

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

/// Consumes an ordered log of binary payloads from a track, yielding every one in order.
///
/// A [`Producer`](super::Producer) writes the whole log into one group, but a publisher that rolls
/// its own (the way a failed write is recovered) is read here too: each group starts a cold
/// decompression window.
pub struct Consumer {
	track: moq_net::track::Subscriber,
	group: Option<moq_net::group::Consumer>,
	/// The DEFLATE decoder for the current group, `Some` while decompressing.
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
			track,
			group: None,
			flate: None,
			compression: config.compression,
		}
	}

	/// Get the next payload, or `None` once the track ends.
	pub async fn next(&mut self) -> Result<Option<Bytes>> {
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

	/// Poll for the next payload, without blocking.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Bytes>>> {
		loop {
			let Some(group) = &mut self.group else {
				match self.track.poll_next_group(waiter)? {
					Poll::Ready(Some(group)) => {
						// Each group is its own compressed stream, so the window starts cold.
						self.flate = self.compression.then(moq_flate::Decoder::new);
						self.group = Some(group);
						continue;
					}
					Poll::Ready(None) => return Poll::Ready(Ok(None)),
					Poll::Pending => return Poll::Pending,
				}
			};

			match group.poll_read_frame(waiter)? {
				Poll::Ready(Some(frame)) => return Poll::Ready(self.decode(&frame.payload).map(Some)),
				Poll::Ready(None) => {
					// This group is exhausted. Clear it and poll for a later one, which starts its own
					// window; the log ends only when the track does.
					self.group = None;
				}
				Poll::Pending => return Poll::Pending,
			}
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
