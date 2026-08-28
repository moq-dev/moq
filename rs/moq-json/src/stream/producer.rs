//! Publishing an ordered log over a track: an [`Encoder`] plus the track it writes to.

use std::marker::PhantomData;
use std::sync::{Arc, Mutex};

use serde::Serialize;

use super::{Encoder, ProducerConfig};
use crate::Result;

/// Publishes an ordered log of JSON records over a track, one record per frame in a single group.
///
/// An [`Encoder`] that owns its track. When something else already owns the track, use the
/// [`Encoder`] directly.
///
/// Cheaply clonable: clones share one underlying track and publishing state, so multiple owners
/// (e.g. several producers feeding one log) append into a single ordered stream.
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
	///
	/// Still hands one back once a failed write has ended the log: the subscriber surfaces the abort
	/// on its first read, which is what tells a late reader the log is truncated.
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
				},
				encoder: Encoder::new(config),
			})),
			_marker: PhantomData,
		}
	}

	/// Append one record to the log.
	///
	/// Any failure ends the track. A log missing a record is not the lossless log a stream promises,
	/// and that holds whether the group rejected the record or it never encoded at all, so the
	/// failure is surfaced rather than papered over with a second group. The track is aborted rather
	/// than closed cleanly, so a consumer sees the failure instead of a log that merely looks
	/// complete. Every later append fails on the ended track.
	pub fn append(&mut self, value: &T) -> Result<()> {
		self.inner.lock().unwrap().append(value)
	}

	/// Finish the track, closing the group.
	pub fn finish(&mut self) -> Result<()> {
		self.inner.lock().unwrap().finish()
	}
}

/// Shared publishing state behind [`Producer`]'s `Arc<Mutex>`.
///
/// The track and the encoder are separate fields so a [`Pending`](super::Pending) record (which
/// borrows the encoder) and the write that consumes it (which borrows the track) don't contend for
/// one `&mut self`.
struct Inner<T> {
	track: Track,
	encoder: Encoder<T>,
}

impl<T: Serialize> Inner<T> {
	fn append(&mut self, value: &T) -> Result<()> {
		// Split the borrow so `record` can hold the encoder while `track` is written through.
		let Inner { track, encoder } = self;

		// Encode first, so a value that can't be serialized doesn't publish an empty group that
		// subscribers would advance into and wait on. Opening the group afterwards is safe because
		// `record` guards the window: any failure below drops it uncommitted.
		let record = match encoder.encode(value) {
			Ok(record) => record,
			Err(err) => {
				// A record that can't be encoded is as lost as one the group rejects: the log is
				// missing it either way, and carrying on would present that gap as a complete log.
				// Nothing was published, so this only has to end the track.
				track.abort(moq_net::Error::Cancel);
				return Err(err);
			}
		};

		let opened = track.open();
		let published = opened.is_ok();
		let result = match opened {
			Ok(()) => track.write(record.payload()),
			Err(err) => Err(err),
		};

		if let Err(err) = result {
			// The record never reached the wire either way, so the window is ahead of every consumer
			// and has to be reset. Without this the desync latch answers the next append before the
			// track does, masking the real reason the log stopped.
			drop(record);
			encoder.reset();

			// What differs is whether a consumer could have seen the group. A write failure means the
			// group is already live, so the record is a hole in the log, and a second group would hand
			// consumers that gap dressed up as a complete log. End the track, which is what keeps "a
			// stream is one group" a real invariant rather than the usual case.
			//
			// An `open` failure published nothing (it only runs when there is no group), so a later
			// append opens a fresh group whose decoder starts cold. That path also catches an append
			// onto a track already ended this way, which keeps reporting the error it was aborted with.
			if published {
				track.abort(err.clone());
			}

			return Err(err.into());
		}

		record.commit();
		Ok(())
	}

	fn finish(&mut self) -> Result<()> {
		Ok(self.track.finish()?)
	}
}

/// The track half of [`Inner`]: the single group carrying the whole log.
struct Track {
	inner: moq_net::track::Producer,

	/// Opened on the first append and never rolled.
	group: Option<moq_net::group::Producer>,
}

impl Track {
	/// Open the log's group if it isn't already.
	fn open(&mut self) -> std::result::Result<(), moq_net::Error> {
		if self.group.is_none() {
			self.group = Some(self.inner.append_group()?);
		}
		Ok(())
	}

	/// Append one encoded record to the log's group.
	fn write(&mut self, payload: &bytes::Bytes) -> std::result::Result<(), moq_net::Error> {
		let group = self.group.as_mut().expect("a group is open");
		group.write_frame(moq_net::Timestamp::now(), payload.clone())
	}

	/// End the track with an error, so a consumer sees the failure rather than a clean end.
	///
	/// Aborting the *track* is what a subscriber observes. Aborting only the group drops it from the
	/// cache and the consumer still reads a clean end, which is exactly what a completed log looks
	/// like, so a truncated log would be indistinguishable from a whole one.
	fn abort(&mut self, err: moq_net::Error) {
		// Abort the group with the same error first. `track::Producer::abort` deliberately leaves an
		// already-pulled `group::Consumer` independent, so dropping our handle would hand a reader
		// sitting in the group a generic `Dropped` instead of the failure that ended the log.
		if let Some(group) = self.group.take() {
			let _ = group.abort(err.clone());
		}

		// Abort through a clone, since aborting consumes a handle and the state is shared. Keeping
		// ours means `consume` still hands back a subscriber, which is how a reader learns the log
		// ended badly rather than cleanly.
		let _ = self.inner.clone().abort(err);
	}

	fn finish(&mut self) -> std::result::Result<(), moq_net::Error> {
		// Finalize both independently rather than short-circuiting on the group. Returning early
		// would leave the track open with `group` already taken, so a later append would open a
		// second group, and (with compression) write into it from a window the consumer never
		// received. That is exactly the split log ending the track exists to prevent.
		let group = match self.group.take() {
			Some(mut group) => group.finish(),
			None => Ok(()),
		};
		let track = self.inner.finish();

		group?;
		track?;
		Ok(())
	}
}
