//! Consuming a window from a track: a [`Decoder`] plus the track it reads from.

use std::task::Poll;

use serde::de::DeserializeOwned;

use super::decoder::Codec;
use super::{ConsumerConfig, Decoder, Event};
use crate::Result;

/// Consumes a sliding window of JSON records from a track, yielding one event per change.
///
/// A [`Decoder`] that owns its track: it reads groups, starts a cold DEFLATE window at each
/// boundary, and turns each group's header into just the changes this reader has not been told
/// about. When something else already owns the track, use the [`Decoder`] directly.
///
/// Group rolls never surface. A publisher rolls for compression's sake, and a header restating the
/// window yields nothing for records already delivered, so this reads as one continuous stream of
/// [`Event`]s regardless of how the publisher framed them.
pub struct Consumer<T> {
	track: moq_net::track::Ordered,
	group: Option<moq_net::group::Consumer>,
	codec: Option<Codec>,
	decoder: Decoder<T>,
}

impl<T: DeserializeOwned> Consumer<T> {
	/// Create a consumer reading from the given track subscriber.
	pub fn new(track: moq_net::track::Subscriber, config: ConsumerConfig) -> Self {
		Self {
			track: track.ordered(),
			group: None,
			codec: None,
			decoder: Decoder::new(config),
		}
	}

	/// Absolute index of the oldest record in the window, and of the next to arrive.
	pub fn range(&self) -> std::ops::Range<u64> {
		self.decoder.range()
	}

	/// Get the next event, or `None` once the track ends.
	pub async fn next(&mut self) -> Result<Option<Event<T>>>
	where
		T: Unpin,
	{
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

	/// Poll for the next event, without blocking.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Event<T>>>> {
		loop {
			// Drain what the frames already decoded produced before reading more.
			if let Some(event) = self.decoder.next_event() {
				return Poll::Ready(Ok(Some(event)));
			}

			let Some(group) = &mut self.group else {
				match self.track.poll_next_group(waiter)? {
					Poll::Ready(Some(group)) => {
						self.codec = Some(Codec::new());
						self.group = Some(group);
						continue;
					}
					Poll::Ready(None) => return Poll::Ready(Ok(None)),
					Poll::Pending => return Poll::Pending,
				}
			};

			match group.poll_read_frame(waiter) {
				Poll::Ready(Err(moq_net::Error::Old | moq_net::Error::Lagged | moq_net::Error::Evicted)) => {
					// This group is no longer complete, but the next one starts with a checkpoint
					// that can account for everything this reader missed.
					self.group = None;
					self.codec = None;
				}
				Poll::Ready(Err(err)) => return Poll::Ready(Err(err.into())),
				Poll::Ready(Ok(Some(frame))) => {
					let codec = self.codec.as_mut().expect("an open MoQ group has a window codec");
					self.decoder.decode(codec, &frame.payload)?;
				}
				Poll::Ready(Ok(None)) => {
					// This group is exhausted. Clear it and poll for a later one, which restates the
					// window; the stream ends only when the track does.
					self.group = None;
					self.codec = None;
				}
				Poll::Pending => return Poll::Pending,
			}
		}
	}
}
