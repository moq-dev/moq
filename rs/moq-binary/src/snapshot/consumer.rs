//! Consuming a binary value from a track.

use std::task::Poll;

use bytes::Bytes;

use crate::Result;

pub use super::Config;

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
	/// Set [`Config::compression`] to match the producer that wrote the track.
	pub fn new(track: moq_net::track::Subscriber, config: Config) -> Self {
		Self {
			track: track.ordered(),
			group: None,
			flate: None,
			compression: config.compression.is_deflate(),
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
	///
	/// A group the transport can no longer serve is discarded the same way, not reported: on a
	/// snapshot track its content is superseded by definition, so the reader waits for the
	/// replacement. Only a failure of the track itself ends the stream.
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
			match group.poll_read_frame(waiter) {
				Poll::Ready(Ok(Some(frame))) => latest = Some(self.decode(&frame.payload)?),
				// The current group is exhausted; wait for a newer one.
				Poll::Ready(Ok(None)) => {
					self.group = None;
					break;
				}
				// The transport can no longer serve the rest of this group: it was superseded and
				// reclaimed (`Old`), dropped under memory pressure (`Evicted`), or read past the
				// drift budget (`Lagged`). A snapshot reader only ever wants the newest value, so a
				// group whose content is gone is never fatal: drop it and wait for its replacement.
				// A track- or session-level failure still arrives through `poll_next_group` above.
				Poll::Ready(Err(err)) => {
					let sequence = group.sequence;
					self.group = None;
					tracing::warn!(
						track = self.track.name(),
						group = sequence,
						error = ?err,
						"snapshot group lost; waiting for a newer one"
					);
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
