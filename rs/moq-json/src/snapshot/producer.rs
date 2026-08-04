//! Publishing a JSON value over a track: an [`Encoder`] plus the track it writes to.

use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::{Encoded, Encoder, ProducerConfig};
use crate::Result;

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
		self.inner.lock().unwrap().track.inner.subscribe(None)
	}
}

impl<T: Serialize> Producer<T> {
	/// Create a producer that publishes to the given track.
	pub fn new(track: moq_net::track::Producer, config: ProducerConfig) -> Self {
		Self {
			inner: Arc::new(Mutex::new(Inner {
				track: Track {
					inner: track,
					group: None,
					deltas: config.delta_ratio != 0,
				},
				encoder: Encoder::new(config),
			})),
			_marker: PhantomData,
		}
	}

	/// Publish a new value, emitting a snapshot or a delta automatically.
	///
	/// Does nothing if the value is unchanged from the previous publish.
	pub fn update(&mut self, value: &T) -> Result<()> {
		self.inner.lock().unwrap().update(value)
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
	/// Publishing on drop can fail (a closed track, a value that won't serialize) and only logs a
	/// warning. Call [`Guard::commit`] instead to handle the error.
	pub fn lock(&mut self) -> Guard<'_, T>
	where
		T: Default + DeserializeOwned,
	{
		let inner = self.inner.lock().unwrap();
		let value = inner
			.encoder
			.value()
			.and_then(|last| serde_json::from_value(last.clone()).ok())
			.unwrap_or_default();

		Guard {
			inner,
			value,
			dirty: false,
		}
	}

	/// Finish the track, closing any open group.
	pub fn finish(&mut self) -> Result<()> {
		self.inner.lock().unwrap().finish()
	}
}

/// An RAII editing guard returned by [`Producer::lock`].
///
/// Holds the producer's lock for its lifetime and derefs to the current value. Mutating it through
/// [`DerefMut`] marks it dirty, and dropping a dirty guard publishes the edited value.
///
/// Publishing on drop swallows any error into a warning, so prefer [`commit`](Self::commit) when the
/// caller can act on a failure.
pub struct Guard<'a, T: Serialize> {
	inner: MutexGuard<'a, Inner<T>>,
	value: T,
	dirty: bool,
}

impl<T: Serialize> Guard<'_, T> {
	/// Publish the edited value, returning any error.
	///
	/// Consumes the guard, so the subsequent drop publishes nothing. A no-op if the value was never
	/// mutated.
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
		if let Err(err) = self.publish() {
			tracing::warn!(%err, "failed to publish JSON value on guard drop");
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
}

impl<T: Serialize> Inner<T> {
	fn update(&mut self, value: &T) -> Result<()> {
		// Split the borrow so `frame` can hold the encoder while `track` is written through.
		let Inner { track, encoder } = self;

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
	/// Write one encoded frame, rolling a group when it's a snapshot.
	fn write(&mut self, encoded: &Encoded) -> Result<()> {
		match encoded.keyframe {
			true => self.write_snapshot(encoded.payload.clone()),
			false => self.write_delta(encoded.payload.clone()),
		}
	}

	/// Close the open group and write a snapshot as the first frame of a new one.
	fn write_snapshot(&mut self, payload: bytes::Bytes) -> Result<()> {
		// The previous group is complete; no more frames will be appended to it.
		if let Some(mut group) = self.group.take() {
			group.finish()?;
		}

		let mut group = self.inner.append_group()?;
		group.write_frame(moq_net::Timestamp::now(), payload)?;

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
		if let Some(mut group) = self.group.take() {
			group.finish()?;
		}
		self.inner.finish()?;
		Ok(())
	}
}
