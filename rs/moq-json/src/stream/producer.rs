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
		self.inner.lock().unwrap().track.subscribe(None)
	}
}

impl<T: Serialize> Producer<T> {
	/// Create a producer that publishes to the given track.
	pub fn new(track: moq_net::track::Producer, config: ProducerConfig) -> Self {
		Self {
			inner: Arc::new(Mutex::new(Inner {
				track,
				group: None,
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
struct Inner<T> {
	track: moq_net::track::Producer,
	// The single group carrying the whole log, opened on the first append.
	group: Option<moq_net::group::Producer>,
	encoder: Encoder<T>,
}

impl<T: Serialize> Inner<T> {
	fn append(&mut self, value: &T) -> Result<()> {
		// Open the group before encoding. Encoding folds the record into the DEFLATE window, so a
		// failure here would leave the window carrying a record that never reached the wire and every
		// later frame would decode against context the consumer doesn't have.
		if self.group.is_none() {
			self.group = Some(self.track.append_group()?);
		}

		let payload = self.encoder.encode(value)?;
		self.group
			.as_mut()
			.expect("a group is open")
			.write_frame(moq_net::Timestamp::now(), payload)?;

		Ok(())
	}

	fn finish(&mut self) -> Result<()> {
		if let Some(mut group) = self.group.take() {
			group.finish()?;
		}
		self.track.finish()?;
		Ok(())
	}
}
