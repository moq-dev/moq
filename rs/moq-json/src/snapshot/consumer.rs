//! Consuming a JSON value from a track: a [`Decoder`] plus the track it reads from.

use std::task::Poll;

use serde::de::DeserializeOwned;

use super::{ConsumerConfig, Decoder};
use crate::Result;

/// Consumes a JSON value from a track, reconstructing it from snapshots and deltas.
///
/// A [`Decoder`] that owns its track: it reads groups, routes each frame by its position, and
/// yields the reconstructed value. When something else already owns the track, use the [`Decoder`]
/// directly.
pub struct Consumer<T> {
	track: moq_net::track::Subscriber,
	group: Option<moq_net::group::Consumer>,
	decoder: Decoder<T>,
	frames_read: usize,
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
			frames_read: 0,
		}
	}

	/// Get the next reconstructed value, or `None` once the track ends.
	pub async fn next(&mut self) -> Result<Option<T>>
	where
		T: Unpin,
	{
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

	/// Poll for the next reconstructed value, without blocking.
	///
	/// Jumps to the newest group, reads its snapshot, and applies deltas in order. All frames already
	/// buffered in the group are applied in one poll but only the resulting *latest* value is yielded:
	/// the intermediate reconstructions are stale, so a late joiner (or any consumer that has fallen
	/// behind) catches up to the head in a single step instead of replaying every superseded state.
	/// Frames must still be decoded in order (the DEFLATE window and merge patches are sequential);
	/// only the per-frame deserialize and yield are skipped. Switching to a newer group discards the
	/// older one.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<T>>> {
		// Drain to the newest group, resetting reconstruction state whenever we switch.
		let track_finished = loop {
			match self.track.poll_next_group(waiter)? {
				Poll::Ready(Some(group)) => {
					self.group = Some(group);
					// The next frame is the new group's snapshot, which also restarts the decoder's window.
					self.frames_read = 0;
				}
				Poll::Ready(None) => break true,
				Poll::Pending => break false,
			}
		};

		// Apply every frame currently buffered in the group, tracking whether any moved us forward and
		// whether the group is still open with nothing buffered yet (vs. exhausted).
		// `poll_read_frame` returns an owned `Poll`, so the borrow of `self.group` ends before the
		// match arms, leaving `apply` (and clearing the group) free to take `&mut self`.
		let mut advanced = false;
		let mut group_pending = false;
		while let Some(group) = &mut self.group {
			match group.poll_read_frame(waiter)? {
				Poll::Ready(Some(frame)) => {
					self.apply(&frame.payload)?;
					advanced = true;
				}
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

		if advanced {
			// Deserialize once, from the head of the backlog we just drained.
			return Poll::Ready(Ok(self.decoder.decode()?));
		}

		// An open group may still deliver frames even after the track finishes (it was appended before
		// the finish), so wait on it rather than ending the stream.
		if group_pending {
			return Poll::Pending;
		}

		if track_finished {
			Poll::Ready(Ok(None))
		} else {
			Poll::Pending
		}
	}

	/// Apply one frame: frame 0 of a group is a snapshot, the rest are merge patches.
	fn apply(&mut self, payload: &[u8]) -> Result<()> {
		match self.frames_read {
			0 => self.decoder.snapshot(payload)?,
			_ => self.decoder.delta(payload)?,
		}
		self.frames_read += 1;
		Ok(())
	}
}
