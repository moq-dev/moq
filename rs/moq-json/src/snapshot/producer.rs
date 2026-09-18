//! Publishing a JSON value over a track: an [`Encoder`] plus the track it writes to.

use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::{Encoded, Encoder};
use crate::{Error, Result};

pub use super::Config;

/// Take the shared publishing state, recovering if a prior guard panicked while holding it.
///
/// A panic under this lock is an in-flight edit being unwound. That edit is discarded (see
/// [`Guard`]'s `Drop`) and the last value that reached the wire is still consistent, so poisoning
/// would only turn the next `modify`/`update` into a panic during cleanup.
fn take<T>(inner: &Mutex<Inner<T>>) -> MutexGuard<'_, Inner<T>> {
	inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Publishes a JSON value over a track, choosing snapshots and deltas automatically.
///
/// An [`Encoder`] that owns its track: it writes each encoded frame and rolls a group whenever the
/// encoder emits a snapshot. When something else already owns the track, use the [`Encoder`]
/// directly.
///
/// Cheaply clonable: clones share one underlying track and publishing state, like other MoQ
/// producers.
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
	/// Create a subscriber for the underlying track.
	pub fn consume(&self) -> moq_net::track::Subscriber {
		take(&self.inner).track.inner.subscribe(None)
	}

	/// Whether any consumer for the underlying track currently exists.
	///
	/// The demand signal for a producer serving on request: an unused track is
	/// cached state nobody is watching, safe to drop and recreate on the next
	/// request.
	pub fn is_used(&self) -> bool {
		take(&self.inner).track.inner.is_used()
	}
}

impl<T: Serialize> Producer<T> {
	/// Create a producer that publishes to the given track.
	pub fn new(track: moq_net::track::Producer, config: Config) -> Self {
		Self {
			inner: Arc::new(Mutex::new(Inner {
				track: Track {
					inner: track,
					group: None,
					deltas: config.delta_ratio != 0,
				},
				encoder: Encoder::new(config),
				aborted: None,
			})),
			_marker: PhantomData,
		}
	}

	/// Publish a new value, emitting a snapshot or a delta automatically.
	///
	/// Does nothing if the value is unchanged from the previous publish.
	pub fn update(&mut self, value: &T) -> Result<()> {
		take(&self.inner).update(value)
	}

	/// Lock the current value for in-place editing, publishing on drop.
	///
	/// The returned [`Guard`] derefs to the current value: everything published through this producer
	/// so far, composed, or `T::default()` if nothing has been. Editing it through [`DerefMut`] marks
	/// the guard dirty; when a dirty guard drops it publishes the result, a no-op if unchanged.
	///
	/// After a rejected frame the current value is what the producer last *tried* to publish, which
	/// consumers never received. That is deliberate. The guard exists so independent owners can each
	/// edit their own field without clobbering, and dropping a rejected owner's field would clobber it
	/// for whoever edits next, which is the failure this API exists to prevent. The owner whose write
	/// failed sees the error and can act on it; the next successful publish is a full snapshot
	/// carrying the composed value, so consumers converge on it either way.
	///
	/// This is the counterpart to a callback: hold the guard, mutate, drop. The guard holds the
	/// producer's lock for its lifetime, so independent owners are serialized: each one starts from
	/// the latest value and their changes compose instead of clobbering. Don't hold a guard across
	/// an `.await`, since that keeps the lock held while suspended.
	///
	/// Fails if the track is closed. That is the one publication failure that happens in normal
	/// operation, and the guard holds the lock that [`finish`](Self::finish) needs, so nothing is
	/// left to check after the guard drops. Anything else that stops the drop from publishing (a
	/// value that won't serialize, or one too large for a frame) aborts the track with that error:
	/// consumers see it instead of a stale value, and the next `modify` returns it here. Call
	/// [`Guard::commit`] to get the error back immediately instead. A panic while the guard is held
	/// discards the in-flight edit rather than publishing a torn value.
	pub fn modify(&mut self) -> Result<Guard<'_, T>>
	where
		T: Default + DeserializeOwned,
	{
		let inner = take(&self.inner);
		inner.open()?;

		let value = inner
			.encoder
			.value()
			.and_then(|last| serde_json::from_value(last.clone()).ok())
			.unwrap_or_default();

		Ok(Guard {
			inner,
			value,
			dirty: false,
		})
	}

	/// Finish the open group, so the deltas already written stop being provisional.
	///
	/// No replacement group opens until the next [`update`](Self::update), which emits a full
	/// snapshot as its first frame even when the value is unchanged. A consumer joining at that group
	/// therefore reads the whole value without the deltas that preceded it.
	///
	/// Idempotent: cutting when no group is open does nothing, so a caller can cut on its own
	/// schedule without tracking what has been published since the last one. Inert when deltas are
	/// disabled, where every frame already gets its own group.
	pub fn cut(&mut self) -> Result<()> {
		take(&self.inner).cut()
	}

	/// Finish the track, closing any open group.
	pub fn finish(&mut self) -> Result<()> {
		take(&self.inner).finish()
	}

	/// Abort the track with the given error, which consumers see in place of any further value.
	///
	/// Consumes the handle, since nothing can be published afterwards. Clones sharing the track see
	/// it closed: their next [`modify`](Self::modify) or [`update`](Self::update) fails.
	pub fn abort(self, err: moq_net::Error) -> Result<()> {
		let mut inner = take(&self.inner);
		inner.abort(err.into());
		Ok(())
	}
}

/// An RAII editing guard returned by [`Producer::modify`].
///
/// Holds the producer's lock for its lifetime and derefs to the current value. Mutating it through
/// [`DerefMut`] marks it dirty, and dropping a dirty guard publishes the edited value.
///
/// Publishing on drop cannot return an error, so a failure aborts the track instead: consumers see
/// the error and the next [`Producer::modify`] returns it. Call [`commit`](Self::commit) when the
/// caller can act on the failure itself. A panic while the guard is held skips publication, so a
/// torn edit is discarded instead of reaching the wire.
pub struct Guard<'a, T: Serialize> {
	inner: MutexGuard<'a, Inner<T>>,
	value: T,
	dirty: bool,
}

impl<T: Serialize> Guard<'_, T> {
	/// Publish the edited value, returning any error.
	///
	/// Consumes the guard, so the subsequent drop publishes nothing. A no-op if the value was never
	/// mutated. Unlike a drop, a failure here leaves the track open: the caller has the error and
	/// decides what to do with it.
	pub fn commit(mut self) -> Result<()> {
		self.publish()
	}

	/// Publish a dirty value once, clearing the dirty flag so it isn't published again.
	fn publish(&mut self) -> Result<()> {
		if !self.dirty {
			return Ok(());
		}
		self.dirty = false;

		// We already hold the lock, so publish through the held guard rather than re-locking.
		self.inner.update(&self.value)
	}
}

impl<T: Serialize> Deref for Guard<'_, T> {
	type Target = T;

	fn deref(&self) -> &T {
		&self.value
	}
}

impl<T: Serialize> DerefMut for Guard<'_, T> {
	fn deref_mut(&mut self) -> &mut T {
		self.dirty = true;
		&mut self.value
	}
}

impl<T: Serialize> Drop for Guard<'_, T> {
	fn drop(&mut self) {
		if std::thread::panicking() {
			// The in-flight copy may be torn; keep the last value that reached the wire.
			return;
		}
		if let Err(err) = self.publish() {
			tracing::error!(%err, "failed to publish JSON value on guard drop, aborting the track");
			self.inner.abort(err);
		}
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

	/// Why this producer aborted the track, so [`Producer::modify`] can report the cause rather than
	/// the closed track it left behind.
	aborted: Option<Error>,
}

impl<T> Inner<T> {
	/// Refuse further edits once the track can't take another group.
	fn open(&self) -> Result<()> {
		if let Some(err) = &self.aborted {
			return Err(err.clone());
		}

		// The same test `append_group` applies: a boundary declared ahead of the live edge still
		// admits the groups below it.
		let track = &self.track.inner;
		let next = track.latest().map_or(0, |latest| latest.saturating_add(1));
		if track.is_closed() || track.final_sequence().is_some_and(|fin| next >= fin) {
			return Err(moq_net::Error::Closed.into());
		}
		Ok(())
	}

	/// Abort the track with a publication failure nobody could return.
	fn abort(&mut self, err: Error) {
		// The track carries a moq-net error; anything else (a value that won't serialize) has no
		// wire code, so consumers get a generic failure while the exact cause stays here.
		let reason = match &err {
			Error::Net(err) => err.clone(),
			_ => moq_net::StreamError::Internal.into(),
		};
		self.encoder.reset();
		self.track.group = None;
		// Idempotent: a track that is already closed keeps its first reason.
		let _ = self.track.inner.clone().abort(reason);
		self.aborted = Some(err);
	}

	/// Finish the open group, leaving the next update to open a replacement with a full snapshot.
	fn cut(&mut self) -> Result<()> {
		if self.track.group.is_none() {
			return Ok(());
		}

		// The group closes either way below, so reset first: a `finish` error must not leave the
		// encoder emitting deltas against a snapshot whose group is gone.
		self.encoder.reset();
		self.track.cut()
	}
}

impl<T: Serialize> Inner<T> {
	fn update(&mut self, value: &T) -> Result<()> {
		// Split the borrow so `frame` can hold the encoder while `track` is written through.
		let Inner { track, encoder, .. } = self;

		let Some(frame) = encoder.update(value)? else {
			return Ok(());
		};

		// A failed write drops `frame` uncommitted, which resets the encoder so the next update
		// resynchronizes with a fresh snapshot. Most failures kill the track outright, but a rejected
		// frame (too large) doesn't, and a delta against a snapshot no consumer ever saw is unreadable.
		track.write(&frame)?;
		frame.commit();

		Ok(())
	}

	fn finish(&mut self) -> Result<()> {
		// The open group goes with the track, so the encoder must not keep emitting deltas into it.
		// Any further update fails on the closed track, but it has to fail as an error rather than by
		// writing a delta with no group to hold it.
		self.encoder.reset();
		self.track.finish()
	}
}

/// The track half of [`Inner`]: where an encoded frame goes and how groups are rolled.
struct Track {
	inner: moq_net::track::Producer,

	/// The group a delta would be appended to, open only while deltas are enabled.
	group: Option<moq_net::group::Producer>,

	/// Whether the encoder can emit deltas at all. With them off every frame is a snapshot, so a
	/// group is closed the moment it's written and never held open.
	deltas: bool,
}

impl Track {
	/// Finish the open group without opening a replacement.
	fn cut(&mut self) -> Result<()> {
		if let Some(group) = self.group.take() {
			group.finish()?;
		}
		Ok(())
	}

	/// Write one encoded frame, rolling a group when it's a snapshot.
	fn write(&mut self, encoded: &Encoded) -> Result<()> {
		// Check before touching a group. `write_snapshot` closes the previous group and publishes a
		// new one before the frame is written, so discovering the limit inside `write_frame` would
		// leave an empty newest group behind: a snapshot consumer jumps to the newest, so the previous
		// value would be lost even though this update reported an error.
		if encoded.payload.len() as u64 > moq_net::group::MAX_CACHE_BYTES {
			return Err(moq_net::Error::FrameTooLarge.into());
		}

		match encoded.keyframe {
			true => self.write_snapshot(encoded.payload.clone()),
			false => self.write_delta(encoded.payload.clone()),
		}
	}

	/// Close the open group and write a snapshot as the first frame of a new one.
	fn write_snapshot(&mut self, payload: bytes::Bytes) -> Result<()> {
		// The previous group is complete; no more frames will be appended to it.
		if let Some(group) = self.group.take() {
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

		match self.deltas {
			// Keep the group open so future deltas can be appended to it.
			true => self.group = Some(group),
			// One frame per group, identical to a plain JSON track.
			false => group.finish()?,
		}

		Ok(())
	}

	/// Append a delta to the group the last snapshot opened.
	fn write_delta(&mut self, payload: bytes::Bytes) -> Result<()> {
		self.group
			.as_mut()
			.expect("the encoder only emits a delta after a snapshot opened a group")
			.write_frame(moq_net::Timestamp::now(), payload)?;
		Ok(())
	}

	fn finish(&mut self) -> Result<()> {
		if let Some(group) = self.group.take() {
			group.finish()?;
		}
		self.inner.finish()?;
		Ok(())
	}
}
