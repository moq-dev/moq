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
		let record = encoder.encode(value)?;

		// A failed open or write drops `record` uncommitted. The log rides a single group and has no
		// keyframe to resynchronize on, so a compressed encoder refuses to continue rather than emit
		// frames the consumer cannot decode.
		track.open()?;
		track.write(record.payload())?;
		record.commit();

		Ok(())
	}

	fn finish(&mut self) -> Result<()> {
		self.track.finish()
	}
}

/// The track half of [`Inner`]: the single group carrying the whole log.
struct Track {
	inner: moq_net::track::Producer,
	// Opened on the first append and never rolled.
	group: Option<moq_net::group::Producer>,
}

impl Track {
	/// Open the log's group if it isn't already.
	fn open(&mut self) -> Result<()> {
		if self.group.is_none() {
			self.group = Some(self.inner.append_group()?);
		}
		Ok(())
	}

	/// Append one encoded record to the log's group.
	fn write(&mut self, payload: &bytes::Bytes) -> Result<()> {
		self.group
			.as_mut()
			.expect("a group is open")
			.write_frame(moq_net::Timestamp::now(), payload.clone())?;
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
