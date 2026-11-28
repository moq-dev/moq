//! Consuming a window from a track: a [`Decoder`] plus the track it reads from.

use std::task::{Poll, ready};

use serde::de::DeserializeOwned;

use super::decoder::Codec;
use super::{Decoder, Event};
use crate::Result;

/// Configuration for a [`Consumer`], and so for the [`Decoder`] inside one.
///
/// Empty for now. Build from [`Default`]; the struct is `#[non_exhaustive]`, so new options stay
/// additive.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Config {}

/// Consumes a sliding window of JSON records from a track, yielding one event per change.
///
/// A [`Decoder`] that owns its track: it reads groups and turns each group's header into just the changes this reader has not been told
/// about. When something else already owns the track, use the [`Decoder`] directly.
///
/// Group rolls never surface. A header restating the
/// window yields nothing for records already delivered, so this reads as one continuous stream of
/// [`Event`]s regardless of how the publisher framed them.
///
/// Each event carries the timestamp of the frame that produced it, its group key on a scale-less
/// track. The wire has no per-record time, so a record a reader first learns of from a group header
/// (one it joined after, or missed while behind) gets the header frame's time, not its original
/// push time.
pub struct Consumer<T> {
	track: moq_net::track::Ordered,
	group: Option<moq_net::group::Consumer>,
	codec: Option<Codec>,
	decoder: Decoder<T>,
	/// Timestamp of the frame whose events the decoder is draining, `None` before the first.
	at: Option<moq_net::Timestamp>,
}

impl<T: DeserializeOwned> Consumer<T> {
	/// Create a consumer reading from the given track subscriber.
	///
	/// Takes a subscriber where [`snapshot::Consumer::new`](crate::snapshot::Consumer::new) takes the
	/// track: a reader that falls behind learns which records it missed, so the caller picks the
	/// subscription's delay budget and priority, while a snapshot only wants the newest value and
	/// subscribes itself at a zero delay.
	pub fn new(track: moq_net::track::Subscriber, config: Config) -> Self {
		Self {
			track: track.ordered(),
			group: None,
			codec: None,
			decoder: Decoder::new(config),
			at: None,
		}
	}

	/// Absolute index of the oldest record in the window, and of the next to arrive.
	pub fn range(&self) -> std::ops::Range<u64> {
		self.decoder.range()
	}

	/// Get the next event and its frame's timestamp, or `None` once the track ends.
	///
	/// A group that fails, whatever with, is read past from the next group's checkpoint; only
	/// the track's own error ends the reader.
	#[cfg_attr(feature = "async", kio::wrapper)]
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<crate::Timed<Event<T>>>>> {
		loop {
			// Drain what the last frame produced before reading more, so every queued event is that
			// frame's and shares its timestamp.
			if let Some(value) = self.decoder.next_event() {
				let at = self.at.expect("an event follows the frame that produced it");
				return Poll::Ready(Ok(Some(crate::Timed { value, at })));
			}

			let Some(group) = &mut self.group else {
				// The window advances by index, so a group given up on is read past.
				match ready!(self.track.poll_next_group(waiter)?) {
					Some(group) => {
						self.codec = Some(Codec::new());
						self.group = Some(group);
						continue;
					}
					None => return Poll::Ready(Ok(None)),
				}
			};

			match group.poll_read_frame(waiter) {
				// A failed group is that group's gap, whatever it failed with: the next one starts
				// with a checkpoint that accounts for everything this reader missed, and only the
				// track's own error, from the next group read, ends the reader.
				Poll::Ready(Err(_)) => {
					self.group = None;
					self.codec = None;
				}
				Poll::Ready(Ok(Some(frame))) => {
					let codec = self.codec.as_mut().expect("an open MoQ group has a window codec");
					self.at = Some(frame.timestamp);
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
