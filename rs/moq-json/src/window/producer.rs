//! Publishing a window over a track: an [`Encoder`] plus the track it writes to.

use std::marker::PhantomData;
use std::sync::{Arc, Mutex};

use moq_net::Timed;
use serde::Serialize;
use serde_json::Value;

use super::{Encoded, Encoder, ProducerConfig};
use crate::Result;
use crate::refresh::Refresh;

/// Publishes a sliding window of JSON records over a track.
///
/// An [`Encoder`] that owns its track: it writes each encoded frame and rolls a group whenever the
/// encoder emits a header. When something else already owns the track, use the [`Encoder`] directly.
///
/// A group runs until a joiner wants a fresh one, which [`refresh`](Self::refresh) answers, or until
/// it reaches the encoder's caps. Set the track's [`refresh`](moq_net::track::Info::refresh) ratio
/// when creating it, or joins never ask.
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
				refresh: Refresh::new(&track),
				track: Track {
					inner: track,
					group: None,
					largest: None,
				},
				encoder: Encoder::new(config),
				at: None,
				finished: false,
			})),
			_marker: PhantomData,
		}
	}

	/// Create a subscriber for the underlying track.
	pub fn consume(&self) -> moq_net::track::Subscriber {
		self.inner.lock().unwrap().track.inner.subscribe(None)
	}

	/// The retained checkpoint suffix, oldest first.
	///
	/// This is the complete window unless [`ProducerConfig::checkpoint_records`] is set.
	pub fn window(&self) -> Vec<Value> {
		self.inner.lock().unwrap().encoder.window()
	}

	/// Absolute index of the oldest retained record, and of the next to be pushed.
	pub fn range(&self) -> std::ops::Range<u64> {
		self.inner.lock().unwrap().encoder.range()
	}

	/// Drop records from the front at `count`'s timestamp, or untimed when absent.
	///
	/// A no-op when the window is already empty, and clamped to what it holds, so a caller can trim
	/// unconditionally.
	pub fn pop(&mut self, count: impl Into<Timed<u64>>) -> Result<()> {
		self.inner.lock().unwrap().pop(count.into())
	}

	/// Finish the open group, leaving the next edit to open a replacement with a header.
	///
	/// Idempotent: cutting when no group is open does nothing. A caller that stores complete groups
	/// cuts after its edits, so every edit so far sits in a group no later frame can extend.
	pub fn cut(&mut self) -> Result<()> {
		self.inner.lock().unwrap().cut()
	}

	/// Wait for a joiner to want a fresh group, then restate the window in a header opening one.
	///
	/// A join wants one once the open group's ops outweigh the track's
	/// [`refresh`](moq_net::track::Info::refresh) ratio times its header (see
	/// [`Demand::wanted`](moq_net::track::Demand::wanted)). It is a soft trigger: that joiner still
	/// reads the cached group, then the new one. The header carries the newest timestamp written. Nothing happens before
	/// the first edit, or on a track without a ratio.
	///
	/// The producer has no runtime of its own, so the owner drives this: loop on it, or select on it
	/// beside the edits. Errors once the track is finished or closed.
	pub async fn refresh(&self) -> Result<()> {
		kio::wait(|waiter| self.poll_refresh(waiter)).await
	}

	/// Poll for [`refresh`](Self::refresh), for an owner driving the producer synchronously.
	pub fn poll_refresh(&self, waiter: &kio::Waiter) -> std::task::Poll<Result<()>> {
		self.inner.lock().unwrap().poll_refresh(waiter)
	}

	/// Finish the track, closing any open group.
	///
	/// Borrows rather than consumes, matching snapshot and stream, so the handle stays
	/// usable afterwards (reads, a second finish). Writes after this fail with
	/// [`moq_net::Error::Closed`].
	pub fn finish(&mut self) -> Result<()> {
		self.inner.lock().unwrap().finish()
	}

	/// Abort the track with the given error, which consumers see in place of a clean end.
	///
	/// Consumes the handle, since nothing can be published afterwards. Clones sharing the track see
	/// it closed: their next edit fails with [`moq_net::Error::Closed`].
	pub fn abort(self, err: moq_net::Error) -> Result<()> {
		self.inner.lock().unwrap().abort(err);
		Ok(())
	}
}

impl<T: Serialize> Producer<T> {
	/// Append a record at its timestamp, or untimed when absent.
	pub fn push<'a>(&mut self, value: impl Into<Timed<&'a T>>) -> Result<()>
	where
		T: 'a,
	{
		self.inner.lock().unwrap().push(value.into())
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

	/// Watches for a joiner wanting a group past the largest written.
	refresh: Refresh,

	/// The newest timestamp written, which a refresh restates the window at. An edit may carry an
	/// older timestamp than its group's first frame, and a new group may not start before that one.
	at: Option<moq_net::Timestamp>,

	finished: bool,
}

impl<T> Inner<T> {
	fn pop(&mut self, count: Timed<u64>) -> Result<()> {
		self.ensure_open()?;
		let Inner { track, encoder, .. } = self;

		let Some(frame) = encoder.pop(count.value)? else {
			return Ok(());
		};

		// A failed write drops the frame uncommitted. The pop is discarded, and the next edit opens a
		// new group, since a consumer may hold the group the frame was meant for.
		track.write(count.at, &frame)?;
		frame.commit();
		self.at = self.at.max(count.at);

		Ok(())
	}

	fn poll_refresh(&mut self, waiter: &kio::Waiter) -> std::task::Poll<Result<()>> {
		// A finished track still has subscribers draining it, so its demand alone never says stop.
		self.ensure_open()?;
		std::task::ready!(self.refresh.poll(self.track.largest, waiter))?;

		let Inner { track, encoder, at, .. } = self;
		let frame = encoder.snapshot()?;
		track.write(*at, &frame)?;
		frame.commit();
		std::task::Poll::Ready(Ok(()))
	}

	fn cut(&mut self) -> Result<()> {
		let Some(mut group) = self.track.group.take() else {
			return Ok(());
		};
		// The group closes either way, so reset first: a `finish` error must not leave the encoder
		// appending ops to a group that is gone.
		self.encoder.reset();
		group.finish()?;
		Ok(())
	}

	fn finish(&mut self) -> Result<()> {
		if self.finished {
			return Ok(());
		}
		self.finished = true;
		self.track.finish()
	}

	fn abort(&mut self, err: moq_net::Error) {
		self.finished = true;
		// The group first: aborting the track leaves an already-pulled group consumer independent.
		if let Some(group) = self.track.group.take() {
			let _ = group.abort(err.clone());
		}
		// Idempotent: a track that is already closed keeps its first reason.
		let _ = self.track.inner.clone().abort(err);
	}

	fn ensure_open(&self) -> Result<()> {
		if self.finished {
			return Err(moq_net::Error::Closed.into());
		}
		Ok(())
	}
}

impl<T: Serialize> Inner<T> {
	fn push(&mut self, value: Timed<&T>) -> Result<()> {
		self.ensure_open()?;
		let Inner { track, encoder, .. } = self;

		let frame = encoder.push(value.value)?;
		track.write(value.at, &frame)?;
		frame.commit();
		self.at = self.at.max(value.at);

		Ok(())
	}
}

/// The track half of [`Inner`]: where an encoded frame goes and how groups are rolled.
struct Track {
	inner: moq_net::track::Producer,

	/// The group an op would be appended to, open after a header.
	group: Option<moq_net::group::Producer>,

	/// The sequence of the newest group written, which a refresh has to pass.
	largest: Option<u64>,
}

impl Track {
	/// Write one encoded frame, rolling a group when it is a header.
	fn write(&mut self, timestamp: Option<moq_net::Timestamp>, encoded: &Encoded) -> Result<()> {
		// Check before touching a group: a header publishes a new group before its frame is
		// written, so a mismatch found by `write_frame` would leave an empty newest group behind.
		if timestamp.is_some() != self.inner.info().timescale.is_some() {
			return Err(moq_net::Error::TimestampMismatch.into());
		}

		match encoded.keyframe {
			true => self.write_header(timestamp, encoded.payload.clone()),
			false => self.write_op(timestamp, encoded.payload.clone()),
		}
	}

	/// Close the open group and write the header as the first frame of a new one.
	fn write_header(&mut self, timestamp: Option<moq_net::Timestamp>, payload: bytes::Bytes) -> Result<()> {
		// The previous group is complete; no more frames will be appended to it.
		if let Some(mut group) = self.group.take() {
			group.finish()?;
		}

		let mut group = self.inner.append_group()?;
		self.largest = Some(group.sequence);
		if let Err(err) = group.write_frame(timestamp, payload) {
			// `append_group` already published this group, and a rejected frame (too large) doesn't
			// close the track. Dropping the handle does NOT close the group, so leaving it would strand
			// any subscriber that advanced into it with nothing to read and no end.
			let _ = group.finish();
			return Err(err.into());
		}

		self.group = Some(group);
		Ok(())
	}

	/// Append an op to the group the last header opened.
	fn write_op(&mut self, timestamp: Option<moq_net::Timestamp>, payload: bytes::Bytes) -> Result<()> {
		self.group
			.as_mut()
			.expect("the encoder only emits an op after a header opened a group")
			.write_frame(timestamp, payload)?;
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
