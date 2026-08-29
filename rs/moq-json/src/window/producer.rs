//! Publishing a window over a track: an [`Encoder`] plus the track it writes to.

use std::marker::PhantomData;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::Value;

use super::{Encoded, Encoder, ProducerConfig};
use crate::Result;

/// Publishes a sliding window of JSON records over a track.
///
/// An [`Encoder`] that owns its track: it writes each encoded frame and rolls a group whenever the
/// encoder emits a reset. When something else already owns the track, use the [`Encoder`] directly.
///
/// Cheaply clonable: clones share one underlying track and window, like other MoQ producers.
pub struct Producer<T> {
	inner: Arc<Mutex<Inner<T>>>,
	_marker: PhantomData<fn(T)>,
}

impl<T> Clone for Producer<T> {
	fn clone(&self) -> Self {
		Self {
			inner: self.inner.clone(),
			_marker: PhantomData,
		}
	}
}

impl<T> Producer<T> {
	/// Create a producer that publishes to the given track.
	pub fn new(track: moq_net::track::Producer, config: ProducerConfig) -> Self {
		Self {
			inner: Arc::new(Mutex::new(Inner {
				track: Track {
					inner: track,
					group: None,
				},
				encoder: Encoder::new(config),
			})),
			_marker: PhantomData,
		}
	}

	/// Create a subscriber for the underlying track.
	pub fn consume(&self) -> moq_net::track::Subscriber {
		self.inner.lock().unwrap().track.inner.subscribe(None)
	}

	/// The retained window, oldest first.
	pub fn window(&self) -> Vec<Value> {
		self.inner.lock().unwrap().encoder.window().cloned().collect()
	}

	/// Absolute index of the oldest retained record, and of the next to be pushed.
	pub fn range(&self) -> std::ops::Range<u64> {
		self.inner.lock().unwrap().encoder.range()
	}

	/// Drop `count` records from the front of the window.
	///
	/// A no-op when the window is already empty, and clamped to what it holds, so a caller can trim
	/// unconditionally.
	pub fn pop(&mut self, count: u64) -> Result<()> {
		self.inner.lock().unwrap().pop(count)
	}

	/// Finish the track, closing any open group.
	pub fn finish(&mut self) -> Result<()> {
		self.inner.lock().unwrap().finish()
	}
}

impl<T: Serialize> Producer<T> {
	/// Append one record to the back of the window.
	pub fn push(&mut self, value: &T) -> Result<()> {
		self.inner.lock().unwrap().push(value)
	}
}

/// Shared publishing state behind [`Producer`]'s `Arc<Mutex>`.
///
/// The track and the encoder are separate fields so a [`Pending`](super::Pending) frame (which
/// borrows the encoder) and the write that consumes it (which borrows the track) don't contend for
/// one `&mut self`.
struct Inner<T> {
	track: Track,
	encoder: Encoder<T>,
}

impl<T> Inner<T> {
	fn pop(&mut self, count: u64) -> Result<()> {
		let Inner { track, encoder } = self;

		let Some(frame) = encoder.pop(count)? else {
			return Ok(());
		};

		// A failed write drops the frame uncommitted, which resets the encoder so the next edit
		// restates the whole window. The pop itself stands: the record really is gone from the
		// publisher's window, and only the consumer's knowledge of that is lost.
		track.write(&frame)?;
		frame.commit();

		Ok(())
	}

	fn finish(&mut self) -> Result<()> {
		// The open group goes with the track, so the encoder must not keep emitting ops into it. Any
		// further edit fails on the closed track, but it has to fail as a track error rather than by
		// writing an op with no group to hold it.
		self.encoder.reset();
		self.track.finish()
	}
}

impl<T: Serialize> Inner<T> {
	fn push(&mut self, value: &T) -> Result<()> {
		let Inner { track, encoder } = self;

		let frame = encoder.push(value)?;
		track.write(&frame)?;
		frame.commit();

		Ok(())
	}
}

/// The track half of [`Inner`]: where an encoded frame goes and how groups are rolled.
struct Track {
	inner: moq_net::track::Producer,

	/// The group an op would be appended to, open between resets.
	group: Option<moq_net::group::Producer>,
}

impl Track {
	/// Write one encoded frame, rolling a group when it's a reset.
	fn write(&mut self, encoded: &Encoded) -> Result<()> {
		match encoded.keyframe {
			true => self.write_reset(encoded.payload.clone()),
			false => self.write_op(encoded.payload.clone()),
		}
	}

	/// Close the open group and write a reset as the first frame of a new one.
	fn write_reset(&mut self, payload: bytes::Bytes) -> Result<()> {
		// The previous group is complete; no more frames will be appended to it.
		if let Some(mut group) = self.group.take() {
			group.finish()?;
		}

		let mut group = self.inner.append_group()?;
		if let Err(err) = group.write_frame(moq_net::Timestamp::now(), payload) {
			// `append_group` already published this group, and a rejected frame (too large) doesn't
			// close the track. Dropping the handle does NOT close the group, so leaving it would strand
			// any subscriber that advanced into it with nothing to read and no end.
			let _ = group.finish();
			return Err(err.into());
		}

		self.group = Some(group);
		Ok(())
	}

	/// Append an op to the group the last reset opened.
	fn write_op(&mut self, payload: bytes::Bytes) -> Result<()> {
		self.group
			.as_mut()
			.expect("the encoder only emits an op after a reset opened a group")
			.write_frame(moq_net::Timestamp::now(), payload)?;
		Ok(())
	}

	fn finish(&mut self) -> Result<()> {
		if let Some(mut group) = self.group.take() {
			group.finish()?;
		}
		self.inner.finish()?;
		Ok(())
	}
}
