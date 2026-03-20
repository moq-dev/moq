use std::task::Poll;

use super::container::Error;
use super::{ContainerFormat, OrderedConsumer, OrderedFrame};

/// A frame returned by [`OrderedMuxer::read()`] with its track name.
pub struct MuxedFrame {
	/// The track name this frame belongs to.
	pub name: String,
	/// The frame data.
	pub frame: OrderedFrame,
}

/// Merges multiple track consumers into a single timestamp-ordered stream.
///
/// Given N consumers (one per track), yields frames in ascending timestamp order
/// across all tracks. This enables proper interleaving for multi-track fMP4 output.
pub struct OrderedMuxer<F: ContainerFormat> {
	tracks: Vec<MuxerTrack<F>>,
}

struct MuxerTrack<F: ContainerFormat> {
	name: String,
	consumer: OrderedConsumer<F>,
	pending: Option<OrderedFrame>,
	finished: bool,
}

impl<F: ContainerFormat> OrderedMuxer<F> {
	/// Create a new muxer from a list of (name, consumer) pairs.
	pub fn new(tracks: Vec<(String, OrderedConsumer<F>)>) -> Self {
		Self {
			tracks: tracks
				.into_iter()
				.map(|(name, consumer)| MuxerTrack {
					name,
					consumer,
					pending: None,
					finished: false,
				})
				.collect(),
		}
	}

	/// Read the next frame in timestamp order across all tracks.
	///
	/// Returns `None` when all tracks have ended.
	pub async fn read(&mut self) -> Result<Option<MuxedFrame>, Error> {
		conducer::wait(|waiter| self.poll_read(waiter)).await
	}

	/// Poll-based implementation.
	pub fn poll_read(&mut self, waiter: &conducer::Waiter) -> Poll<Result<Option<MuxedFrame>, Error>> {
		// Fill empty pending slots
		for track in &mut self.tracks {
			if track.pending.is_none() && !track.finished {
				match track.consumer.poll_read(waiter) {
					Poll::Ready(Ok(Some(frame))) => {
						track.pending = Some(frame);
					}
					Poll::Ready(Ok(None)) => {
						track.finished = true;
					}
					Poll::Ready(Err(e)) => {
						track.finished = true;
						tracing::warn!(track = %track.name, error = ?e, "track error, marking finished");
					}
					Poll::Pending => {}
				}
			}
		}

		// Find minimum timestamp across pending frames
		let mut min_idx = None;
		let mut min_ts = None;

		for (i, track) in self.tracks.iter().enumerate() {
			if let Some(frame) = &track.pending {
				let ts: std::time::Duration = frame.timestamp.into();
				if min_ts.is_none() || ts < min_ts.unwrap() {
					min_ts = Some(ts);
					min_idx = Some(i);
				}
			}
		}

		// Return the frame with the smallest timestamp
		if let Some(idx) = min_idx {
			let track = &mut self.tracks[idx];
			let frame = track.pending.take().unwrap();
			let name = track.name.clone();
			return Poll::Ready(Ok(Some(MuxedFrame { name, frame })));
		}

		// All finished + no pending → None
		if self.tracks.iter().all(|t| t.finished) {
			return Poll::Ready(Ok(None));
		}

		// Still waiting for data
		Poll::Pending
	}
}
