//! Consuming an ordered log from a track: a [`Decoder`] plus the track it reads from.

use std::task::Poll;

use serde::de::DeserializeOwned;

use super::{ConsumerConfig, Decoder};
use crate::Result;

/// Consumes an ordered log of JSON records from a track, yielding every record in order.
///
/// A [`Decoder`] that owns its track: it reads one record per frame, in order. The log is a single
/// group, which is what makes the mode lossless: rolling to a second group means the records that
/// would have completed the first are gone, so a [`Producer`](super::Producer) that cannot write
/// ends the track instead. A second group is therefore a broken publisher, and reading it would
/// present a gap as a continuous log, so it fails with [`Error::Rolled`](crate::Error::Rolled)
/// rather than yielding the remainder. When something else already owns the track, use the
/// [`Decoder`] directly.
pub struct Consumer<T> {
	track: moq_net::track::Subscriber,
	group: Option<moq_net::group::Consumer>,
	/// Whether the log's one group has been taken, so a second is a rolled log rather than the first.
	taken: bool,
	decoder: Decoder<T>,
}

impl<T: DeserializeOwned> Consumer<T> {
	/// Create a consumer reading from the given track subscriber.
	///
	/// Set [`ConsumerConfig::compression`] to read a track written by a producer with
	/// [`ProducerConfig::compression`](super::ProducerConfig::compression) on.
	pub fn new(track: moq_net::track::Subscriber, config: ConsumerConfig) -> Self {
		Self {
			track,
			group: None,
			taken: false,
			decoder: Decoder::new(config),
		}
	}

	/// Get the next record, or `None` once the track ends.
	pub async fn next(&mut self) -> Result<Option<T>>
	where
		T: Unpin,
	{
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

	/// Poll for the next record, without blocking.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<T>>> {
		loop {
			let Some(group) = &mut self.group else {
				// Arrival order rather than sequence order, because there is only ever one group to
				// take and a second one has to be seen whatever its sequence. The monotonic
				// `poll_next_group` would drop a late lower sequence, which is the very loss this
				// has to report.
				match self.track.poll_recv_group(waiter)? {
					Poll::Ready(Some(group)) => {
						if self.taken {
							return Poll::Ready(Err(crate::Error::Rolled));
						}
						self.taken = true;
						self.decoder.reset();
						self.group = Some(group);
						continue;
					}
					Poll::Ready(None) => return Poll::Ready(Ok(None)),
					Poll::Pending => return Poll::Pending,
				}
			};

			match group.poll_read_frame(waiter)? {
				Poll::Ready(Some(frame)) => return Poll::Ready(Ok(Some(self.decoder.decode(&frame.payload)?))),
				Poll::Ready(None) => {
					// The log's one group is exhausted. Keep polling the track so a clean end still
					// reports the log as complete, and so a second group is caught as `Rolled`.
					self.group = None;
				}
				Poll::Pending => return Poll::Pending,
			}
		}
	}
}
