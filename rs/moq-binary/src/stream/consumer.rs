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

/// Consumes an ordered log of binary payloads from a track, yielding every one.
///
/// The log is a single group. That is what makes the mode lossless: rolling to a second group
/// means the records that would have completed the first are gone, so a publisher that cannot
/// write ends the track instead. A second group is therefore a broken publisher, and reading it
/// would present a gap as a continuous log, so it fails with
/// [`Error::Rolled`](crate::Error::Rolled) rather than yielding the remainder.
///
/// The failure does not wait for the first group to end: whatever has already arrived in it is
/// yielded, and the read then fails rather than blocking on a group a broken publisher may never
/// finish.
pub struct Consumer {
	track: moq_net::track::Subscriber,
	group: Option<moq_net::group::Consumer>,
	/// Whether the log's one group has been taken, so a second is a rolled log rather than the first.
	taken: bool,
	/// Sticky once a second group is seen: the payloads it displaced are gone, so every later read
	/// fails too rather than reporting the rest of the log as a whole one.
	rolled: bool,
	/// The DEFLATE decoder for the group, `Some` while decompressing.
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
			taken: false,
			rolled: false,
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
			if self.rolled {
				return Poll::Ready(Err(crate::Error::Rolled));
			}

			let Some(group) = &mut self.group else {
				// Arrival order rather than sequence order, because there is only ever one group to
				// take and a second one has to be seen whatever its sequence. The monotonic
				// `poll_next_group` would drop a late lower sequence, which is the very loss this
				// has to report.
				match self.track.poll_recv_group(waiter)? {
					Poll::Ready(Some(_)) if self.taken => self.rolled = true,
					Poll::Ready(Some(group)) => {
						self.taken = true;
						self.flate = self.compression.then(moq_flate::Decoder::new);
						self.group = Some(group);
					}
					Poll::Ready(None) => return Poll::Ready(Ok(None)),
					Poll::Pending => return Poll::Pending,
				}
				continue;
			};

			match group.poll_read_frame(waiter)? {
				Poll::Ready(Some(frame)) => return Poll::Ready(self.decode(&frame.payload).map(Some)),
				Poll::Ready(None) => {
					// The log's one group is exhausted. Keep polling the track so a clean end still
					// reports the log as complete, and so a second group is caught as `Rolled`.
					self.group = None;
				}
				// Nothing more in the group yet, so ask the track before parking on it. A publisher
				// that opens a second group and leaves the first open would otherwise hold this read
				// open forever, on a log that already lost the payloads the second one displaced.
				// Both polls register, so either source wakes this read.
				Poll::Pending => match self.track.poll_recv_group(waiter)? {
					Poll::Ready(Some(_)) => self.rolled = true,
					// A finished track does not truncate the group in hand; its frames may still arrive.
					Poll::Ready(None) | Poll::Pending => return Poll::Pending,
				},
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
