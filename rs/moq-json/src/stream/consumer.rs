//! Consuming an ordered log from a track: a [`Decoder`] plus the track it reads from.

use std::task::Poll;

use serde::de::DeserializeOwned;

use super::{ConsumerConfig, Decoder};
use crate::Result;

/// Consumes an ordered log of JSON records from a track, yielding every record in order.
///
/// A [`Decoder`] that owns its track. The log rides a single group, so this reads that group's
/// frames in order; one record per frame. When something else already owns the track, use the
/// [`Decoder`] directly.
pub struct Consumer<T> {
	track: moq_net::track::Subscriber,
	group: Option<moq_net::group::Consumer>,
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
				match self.track.poll_next_group(waiter)? {
					Poll::Ready(Some(group)) => {
						// Each group is its own compressed stream, so the window starts cold.
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
					// The group is finished; the log rides just this one, so the next poll for a
					// group ends the stream.
					self.group = None;
				}
				Poll::Pending => return Poll::Pending,
			}
		}
	}
}
