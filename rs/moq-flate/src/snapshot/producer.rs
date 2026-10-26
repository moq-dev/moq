//! Publishing an opaque value over a track.

use std::sync::{Arc, Mutex};

use bytes::Bytes;
use moq_net::Timed;

use crate::Result;

pub use super::Config;

/// Publishes an opaque value over a track, one value per group.
///
/// Each [`update`](Self::update) rolls a new group holding the whole value, so a consumer only ever
/// needs the newest group to reconstruct the value. A reader can retain earlier states within its
/// drift budget. For a log where every payload survives, use
/// [`stream`](crate::stream) instead.
///
/// Cheaply clonable: clones share one underlying track, like other MoQ producers.
#[derive(Clone)]
pub struct Producer {
	inner: Arc<Mutex<Inner>>,
}

impl Producer {
	/// Create a producer that publishes to the given track.
	pub fn new(track: moq_net::track::Producer, config: Config) -> Self {
		Self {
			inner: Arc::new(Mutex::new(Inner {
				track,
				compression: config.compression.is_deflate(),
			})),
		}
	}

	/// Create a subscriber for the underlying track.
	pub fn consume(&self) -> moq_net::track::Subscriber {
		self.inner.lock().unwrap().track.subscribe(None)
	}

	/// A watch-only handle to the underlying track's subscriber demand.
	///
	/// Weak, so holding it neither keeps the track open nor contends with publishing.
	pub fn demand(&self) -> moq_net::track::Demand {
		self.inner.lock().unwrap().track.demand()
	}

	/// Publish a new value, superseding the previous one, and return the frame's encoded size.
	///
	/// Unlike [`moq-json`](https://docs.rs/moq-json), an identical value is republished rather than
	/// skipped: comparing two opaque blobs costs a full scan, and only the caller knows whether its
	/// bytes changed.
	pub fn update(&mut self, payload: impl Into<Timed<Bytes>>) -> Result<usize> {
		self.inner.lock().unwrap().update(payload.into())
	}

	/// Finish the track.
	pub fn finish(&mut self) -> Result<()> {
		self.inner.lock().unwrap().finish()
	}

	/// Abort the track with the given error, which consumers see in place of any further value.
	///
	/// Consumes the handle, since nothing can be published afterwards. Clones sharing the track see
	/// it closed: their next [`update`](Self::update) fails.
	pub fn abort(self, err: moq_net::Error) -> Result<()> {
		// Through a clone, since aborting consumes a handle and the state is shared. Idempotent: a
		// track that is already closed keeps its first reason.
		let _ = self.inner.lock().unwrap().track.clone().abort(err);
		Ok(())
	}
}

/// Shared publishing state behind [`Producer`]'s `Arc<Mutex>`.
struct Inner {
	track: moq_net::track::Producer,
	compression: bool,
}

impl Inner {
	fn update(&mut self, payload: Timed<Bytes>) -> Result<usize> {
		let timestamp = payload.at;
		let payload = payload.value;

		// Check before opening a group. `append_group` publishes immediately, so discovering the limit
		// inside `write_frame` would leave an empty newest group behind: a snapshot consumer jumps to
		// the newest, so the previous value would be lost even though this update reported an error.
		// A compressed value is held to the same budget by its inflated size, since that is what every
		// consumer's decoder allows: compression saves bandwidth, never what a reader holds.
		if payload.len() as u64 > moq_net::group::MAX_CACHE_BYTES {
			return Err(moq_net::Error::FrameTooLarge.into());
		}

		// One frame per group, so the window spans a single value and starts cold every time.
		let payload = match self.compression {
			true => crate::Encoder::new().frame(&payload),
			false => payload,
		};
		if payload.len() as u64 > moq_net::group::MAX_CACHE_BYTES {
			return Err(moq_net::Error::FrameTooLarge.into());
		}
		// The same goes for a timestamp that doesn't match the track.
		if timestamp.is_some() != self.track.info().timescale.is_some() {
			return Err(moq_net::Error::TimestampMismatch.into());
		}

		let size = payload.len();
		let mut group = self.track.append_group()?;
		if let Err(err) = group.write_frame(timestamp, payload) {
			// `append_group` already published this group, and a rejected frame (too large) doesn't
			// close the track. Dropping the handle does NOT close the group, so leaving it would strand
			// any subscriber that advanced into it with nothing to read and no end.
			let _ = group.finish();
			return Err(err.into());
		}

		group.finish()?;
		Ok(size)
	}

	fn finish(&mut self) -> Result<()> {
		self.track.finish()?;
		Ok(())
	}
}
