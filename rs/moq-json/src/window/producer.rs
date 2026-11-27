//! Publishing a window over a track: an [`Encoder`] plus the track it writes to.

use std::marker::PhantomData;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::Value;

use super::{Encoded, Encoder};

pub use super::Config;
use crate::Result;
use crate::refresh::Refresh;

/// Publishes a sliding window of JSON records over a track.
///
/// An [`Encoder`] that owns its track: it writes each encoded frame and rolls a group whenever the
/// encoder emits a header. When something else already owns the track, use the [`Encoder`] directly.
///
/// A group runs until a joiner wants a fresh one, which [`poll_refresh`](Self::poll_refresh) answers, or until
/// it reaches the encoder's caps. Set the track's [`refresh`](moq_net::track::Info::refresh) ratio
/// when creating it, or joins never ask.
///
/// Cheaply clonable: clones share one underlying track and window, like other MoQ producers.
///
/// [`finish`](Self::finish) or [`abort`](Self::abort) the window: dropping the last clone
/// unfinished is not supported, and once it published, its readers read
/// [`moq_net::Error::Dropped`].
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
	pub fn new(track: moq_net::track::Producer, config: Config) -> Self {
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
				state: crate::state::State::Open,
			})),
			_marker: PhantomData,
		}
	}

	/// Create a subscriber for the underlying track.
	pub fn consume(&self) -> moq_net::track::Subscriber {
		self.inner.lock().unwrap().track.inner.subscribe(None)
	}

	/// A watch-only handle to the underlying track's subscriber demand.
	///
	/// Weak, so holding it neither keeps the track open nor contends with publishing.
	pub fn demand(&self) -> moq_net::track::Demand {
		self.inner.lock().unwrap().track.inner.demand()
	}

	/// The retained checkpoint suffix, oldest first.
	///
	/// This is the complete window unless [`Config::checkpoint_records`] is set.
	pub fn window(&self) -> Vec<Value> {
		self.inner.lock().unwrap().encoder.window()
	}

	/// Absolute index of the oldest retained record, and of the next to be pushed.
	pub fn range(&self) -> std::ops::Range<u64> {
		self.inner.lock().unwrap().encoder.range()
	}

	/// Drop `count` records from the front at `at` (see [`Timed::at`](crate::Timed::at)).
	///
	/// A no-op when the window is already empty, and clamped to what it holds, so a caller can trim
	/// unconditionally.
	pub fn pop(&mut self, count: u64, at: moq_net::Timestamp) -> Result<()> {
		self.inner.lock().unwrap().pop(count, at)
	}

	/// Close the open group, leaving the next edit to open a replacement with a header.
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
	/// reads the cached group, then the new one. The header carries the newest timestamp written, where
	/// its group starts; while that is the newest group's own start, the refresh waits for a later
	/// edit, since two groups never share a start. Nothing happens before the first edit, or on a
	/// track without a ratio.
	///
	/// The producer has no runtime of its own, so the owner drives this: loop on it, or select on it
	/// beside the edits. Errors once the track is finished or closed.
	#[cfg_attr(feature = "async", kio::wrapper)]
	pub fn poll_refresh(&self, waiter: &kio::Waiter) -> std::task::Poll<Result<()>> {
		self.inner.lock().unwrap().poll_refresh(waiter)
	}

	/// Finish the track, closing any open group, and return the [`Finished`] window.
	///
	/// Clones sharing the track see it ended: their edits fail with [`moq_net::Error::Closed`]. A
	/// finish that fails aborts the track instead, and those clones then report its cause.
	pub fn finish(self) -> Result<Finished<T>> {
		self.inner.lock().unwrap().finish()?;
		Ok(Finished {
			inner: self.inner,
			_marker: PhantomData,
		})
	}

	/// Abort the track with the given error, which consumers see in place of a clean end.
	///
	/// Consumes the handle, since nothing can be published afterwards. Clones sharing the track see
	/// it closed: their next edit fails with `err`.
	pub fn abort(self, err: moq_net::Error) -> Result<()> {
		self.inner.lock().unwrap().abort(err.into());
		Ok(())
	}
}

impl<T: Serialize> Producer<T> {
	/// Append a record at `at` (see [`Timed::at`](crate::Timed::at)).
	pub fn push(&mut self, value: &T, at: moq_net::Timestamp) -> Result<()> {
		self.inner.lock().unwrap().push(value, at)
	}
}

/// A window whose producer [finished](Producer::finish) it: it can still be read or aborted, but
/// not edited.
///
/// Dropping the last handle sharing the track releases it, so new readers can no longer find it.
///
/// ```compile_fail,E0599
/// # fn cannot_push(window: moq_json::window::Finished<u32>) {
/// window.push(&1, moq_net::Timestamp::unscaled(0)).unwrap();
/// # }
/// ```
#[must_use = "dropping the last handle releases the track, which new readers can then no longer find"]
pub struct Finished<T> {
	inner: Arc<Mutex<Inner<T>>>,
	_marker: PhantomData<fn(T)>,
}

impl<T> Finished<T> {
	/// Create a subscriber for the underlying track.
	pub fn consume(&self) -> moq_net::track::Subscriber {
		self.inner.lock().unwrap().track.inner.subscribe(None)
	}

	/// The retained checkpoint suffix, oldest first; see [`Producer::window`].
	pub fn window(&self) -> Vec<Value> {
		self.inner.lock().unwrap().encoder.window()
	}

	/// Absolute index of the oldest retained record, and of the next one; see [`Producer::range`].
	pub fn range(&self) -> std::ops::Range<u64> {
		self.inner.lock().unwrap().encoder.range()
	}

	/// Abort the track with `err`; see [`moq_net::track::Finished::abort`]. Readers still draining
	/// get the clean end instead once every group below the end finished.
	pub fn abort(self, err: moq_net::Error) -> Result<()> {
		self.inner.lock().unwrap().abort(err.into());
		Ok(())
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
	/// `None` before the first write.
	at: Option<moq_net::Timestamp>,

	/// Whether this producer still publishes, or how it ended the track.
	state: crate::state::State,
}

impl<T> Inner<T> {
	fn pop(&mut self, count: u64, at: moq_net::Timestamp) -> Result<()> {
		self.ensure_open()?;
		let at = self.track.rescale(at)?;
		let Inner { track, encoder, .. } = self;

		let Some(frame) = encoder.pop(count)? else {
			return Ok(());
		};

		// A failed write drops the frame uncommitted. The pop is discarded, and the next edit opens a
		// new group, since a consumer may hold the group the frame was meant for.
		track.write(at, &frame)?;
		frame.commit();
		if self.at.is_none_or(|newest| at > newest) {
			self.at = Some(at);
			self.refresh.advanced();
		}

		Ok(())
	}

	fn poll_refresh(&mut self, waiter: &kio::Waiter) -> std::task::Poll<Result<()>> {
		// A finished track still has subscribers draining it, so its demand alone never says stop.
		self.ensure_open()?;
		std::task::ready!(self.refresh.poll(self.track.largest, waiter))?;

		let Inner {
			track,
			encoder,
			at,
			refresh,
			..
		} = self;
		// Two groups never share a start, so a refresh at the newest group's own start waits for a
		// later write. Checked before encoding, since a header dropped unwritten resets the encoder.
		let Some(at) = *at else {
			return refresh.park(waiter);
		};
		if !crate::starts_past(&track.inner, at) {
			return refresh.park(waiter);
		}
		let frame = encoder.snapshot()?;
		track.write(at, &frame)?;
		frame.commit();
		std::task::Poll::Ready(Ok(()))
	}

	fn cut(&mut self) -> Result<()> {
		let Some(group) = self.track.group.take() else {
			return Ok(());
		};
		// The group closes either way, so reset first: a `close` error must not leave the encoder
		// appending ops to a group that is gone.
		self.encoder.reset();
		group.finish()?;
		Ok(())
	}

	fn finish(&mut self) -> Result<()> {
		match &self.state {
			crate::state::State::Open => {}
			crate::state::State::Finished => return Ok(()),
			crate::state::State::Aborted(err) => return Err(err.clone()),
		}
		// The open group goes with the track, so the encoder must not keep appending ops to it.
		self.encoder.reset();
		if let Err(err) = self.track.finish() {
			// Half-finished, the track could still take groups, so it ends with the cause instead.
			self.abort(err.clone());
			return Err(err);
		}
		self.state = crate::state::State::Finished;
		self.refresh.ended();
		Ok(())
	}

	/// Abort the track with `err`, which every later call reports. The first cause wins.
	fn abort(&mut self, err: crate::Error) {
		// The track carries a moq-net error; anything else has no wire code, so consumers get a
		// generic failure while the exact cause stays here.
		let reason = match &err {
			crate::Error::Net(err) => err.clone(),
			_ => moq_net::StreamError::Internal.into(),
		};
		self.encoder.reset();
		// The group first: aborting the track leaves an already-pulled group consumer independent.
		if let Some(group) = self.track.group.take() {
			let _ = group.abort(reason.clone());
		}
		// Idempotent: a track that is already closed keeps its first reason.
		let _ = self.track.inner.clone().abort(reason);
		self.state.abort(err);
		self.refresh.ended();
	}

	/// Refuse further edits once the track can't take another group.
	fn ensure_open(&self) -> Result<()> {
		self.state.check()?;
		// The same test `append_group` applies: a boundary declared ahead of the live edge still
		// admits the groups below it. The largest counts every group, even an empty one, so the
		// next group is the one after it.
		let track = &self.track.inner;
		let next = track.largest().map_or(0, |largest| largest.group.saturating_add(1));
		if track.is_closed()
			|| track
				.finished()
				.is_some_and(|end| moq_net::track::Position::group(next) >= end)
		{
			return Err(moq_net::Error::Closed.into());
		}
		Ok(())
	}
}

impl<T: Serialize> Inner<T> {
	fn push(&mut self, value: &T, at: moq_net::Timestamp) -> Result<()> {
		self.ensure_open()?;
		let at = self.track.rescale(at)?;
		let Inner { track, encoder, .. } = self;

		let frame = encoder.push(value)?;
		track.write(at, &frame)?;
		frame.commit();
		if self.at.is_none_or(|newest| at > newest) {
			self.at = Some(at);
			self.refresh.advanced();
		}

		Ok(())
	}
}

/// The track half of [`Inner`]: where an encoded frame goes and how groups are rolled.
struct Track {
	inner: moq_net::track::Producer,

	/// The group an op would be appended to, open after a header. Whatever clears it resets the
	/// encoder too, or the encoder's next op would have no group to land in.
	group: Option<moq_net::group::Producer>,

	/// The start of the newest group written, which a refresh has to pass.
	largest: Option<u64>,
}

impl Track {
	/// `timestamp` at the track's timescale, or [`moq_net::Error::TimestampMismatch`] when it is
	/// unscaled on a timed track, scaled on a scale-less one, or beyond what the scale can hold.
	///
	/// An edit checks this before encoding. Found by `write_frame` instead, the mismatch would
	/// come after a header published its group, leaving an empty newest group behind, and the
	/// dropped frame would reset the encoder, rolling a group for an edit that changed nothing.
	fn rescale(&self, timestamp: moq_net::Timestamp) -> Result<moq_net::Timestamp> {
		Ok(self.inner.info().rescale(timestamp)?)
	}

	/// Write one encoded frame at a timestamp already [rescaled](Self::rescale) to the track, rolling a
	/// group when it is a header.
	fn write(&mut self, timestamp: moq_net::Timestamp, encoded: &Encoded) -> Result<()> {
		match encoded.keyframe {
			true => self.write_header(timestamp, encoded.payload.clone()),
			false => self.write_op(timestamp, encoded.payload.clone()),
		}
	}

	/// Write the header as the first frame of a new group starting at `timestamp`, then close the
	/// previous group.
	///
	/// A start at or before the newest group's is [`moq_net::Error::TimestampMismatch`], refused
	/// before the previous group closes.
	fn write_header(&mut self, timestamp: moq_net::Timestamp, payload: bytes::Bytes) -> Result<()> {
		let mut group = self.inner.append_group(timestamp)?;
		self.largest = Some(group.start);

		// The previous group is complete; no more frames will be appended to it. One that can't
		// close (already aborted) fails the write, taking the group just published with it rather
		// than leaving it to drop as `Dropped`.
		if let Some(previous) = self.group.take()
			&& let Err(err) = previous.finish()
		{
			let _ = group.abort(err.clone());
			return Err(err.into());
		}

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
	fn write_op(&mut self, timestamp: moq_net::Timestamp, payload: bytes::Bytes) -> Result<()> {
		let group = self
			.group
			.as_mut()
			.expect("the encoder only emits an op after a header opened a group");
		group.write_frame(crate::frame_time(group, timestamp), payload)?;
		Ok(())
	}

	fn finish(&mut self) -> Result<()> {
		if let Some(group) = self.group.take() {
			group.finish()?;
		}
		// Another handle may have declared the end already, which stands. This shared state
		// keeps its own clone, whose appends the end refuses.
		if self.inner.finished().is_none() {
			drop(self.inner.clone().finish()?);
		}
		Ok(())
	}
}

// Records are indexed across the window's groups, and the next producer of this track would
// number its own from zero, so a reader handed over would take them for records it already has:
// an unfinished window that published a header ends with `Dropped` instead of being requested
// again.
impl Drop for Track {
	fn drop(&mut self) {
		if self.largest.is_none() || self.inner.finished().is_some() {
			return;
		}
		if let Some(group) = self.group.take() {
			let _ = group.abort(moq_net::Error::Dropped);
		}
		// A track already closed keeps its first reason.
		let _ = self.inner.clone().abort(moq_net::Error::Dropped);
	}
}

#[cfg(test)]
mod tests {
	use std::task::Poll;

	use serde_json::{Value, json};

	use super::*;

	/// A reader of a window dropped unfinished reads `Dropped`, not the next producer's records.
	#[test]
	fn a_dropped_window_ends_rather_than_restarting() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let _requests = broadcast.requests();
		let track = broadcast.create_track("test", moq_net::track::Info::default()).unwrap();
		let mut groups = track.subscribe(None);
		let mut producer = Producer::<Value>::new(track, Config::default());
		producer
			.push(&json!({ "n": 1 }), moq_net::Timestamp::unscaled(1))
			.unwrap();

		let waiter = kio::Waiter::noop();
		let Poll::Ready(Ok(Some(mut group))) = groups.poll_recv_group(&waiter) else {
			panic!("expected the window's group");
		};
		drop(producer);
		let ended = loop {
			match group.poll_read_frame(&waiter) {
				Poll::Ready(Ok(Some(_))) => continue,
				other => break other,
			}
		};
		assert!(matches!(ended, Poll::Ready(Err(moq_net::Error::Dropped))), "{ended:?}");
		let next = groups.poll_recv_group(&waiter);
		assert!(matches!(next, Poll::Ready(Err(moq_net::Error::Dropped))));
	}

	/// A previous group already aborted fails the next header, which aborts the group it just
	/// published with the same error rather than dropping it as `Dropped`.
	#[test]
	fn a_previous_group_that_cannot_close_aborts_the_next() {
		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", moq_net::track::Info::default())
			.unwrap();
		// Oldest first, as published.
		let mut groups = track.subscribe(
			moq_net::track::Subscription::default()
				.with_ranges([moq_net::track::Range::default().with_order(moq_net::track::Order::Ascending)]),
		);
		let mut producer = Producer::<Value>::new(track, Config::default());
		producer
			.push(&json!({ "n": 1 }), moq_net::Timestamp::unscaled(1))
			.unwrap();

		let mut inner = producer.inner.lock().unwrap();
		// Swap in a group its own track's abort already failed, keeping the handle.
		let other = moq_net::broadcast::Info::new()
			.produce()
			.create_track("other", moq_net::track::Info::default())
			.unwrap();
		let aborted = other.append_group(moq_net::Timestamp::unscaled(1)).unwrap();
		other.abort(moq_net::Error::GroupTooLarge).unwrap();
		let open = inner.track.group.replace(aborted).expect("an open group");
		let _finished = open.finish().unwrap();
		let err = inner
			.track
			.write_header(moq_net::Timestamp::unscaled(2), bytes::Bytes::from_static(b"{}"))
			.unwrap_err();
		assert!(matches!(err, crate::Error::Net(moq_net::Error::GroupTooLarge)), "{err}");
		assert!(inner.track.group.is_none());
		drop(inner);

		let waiter = kio::Waiter::noop();
		let mut read = 0;
		while let Poll::Ready(Ok(Some(mut group))) = groups.poll_recv_group(&waiter) {
			let ended = loop {
				match group.poll_read_frame(&waiter) {
					Poll::Ready(Ok(Some(_))) => continue,
					other => break other,
				}
			};
			read += 1;
			// The first group finished; the one the failed write published carries the abort.
			match read {
				1 => assert!(matches!(ended, Poll::Ready(Ok(None))), "{ended:?}"),
				_ => assert!(
					matches!(ended, Poll::Ready(Err(moq_net::Error::GroupTooLarge))),
					"{ended:?}"
				),
			}
		}
		assert_eq!(read, 2);
	}
}
