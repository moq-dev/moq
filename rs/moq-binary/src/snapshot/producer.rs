//! Publishing a binary value over a track.

use std::sync::{Arc, Mutex};

use bytes::Bytes;

use crate::Result;

/// Configuration for a [`Producer`].
///
/// Build from [`Default`] and override fields (the struct is `#[non_exhaustive]`, so new options
/// stay additive).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ProducerConfig {
	/// Compress each value as its own raw DEFLATE stream.
	///
	/// A snapshot group holds a single self-contained value, so there is no window to share: each
	/// value is compressed alone. `false` (the default) writes the bytes through untouched. A
	/// [`Consumer`](super::Consumer) must set
	/// [`ConsumerConfig::compression`](super::ConsumerConfig::compression) to match.
	pub compression: bool,
}

impl ProducerConfig {
	/// Set [`compression`](Self::compression) (a builder, since the struct is `#[non_exhaustive]`).
	pub fn with_compression(mut self, compression: bool) -> Self {
		self.compression = compression;
		self
	}
}

/// Publishes a binary value over a track, one value per group.
///
/// Each [`update`](Self::update) rolls a new group holding the whole value, so a consumer only ever
/// needs the newest group and older ones are dropped. For a log where every payload survives, use
/// [`stream`](crate::stream) instead.
///
/// Cheaply clonable: clones share one underlying track, like other MoQ producers.
#[derive(Clone)]
pub struct Producer {
	inner: Arc<Mutex<Inner>>,
}

impl Producer {
	/// Create a producer that publishes to the given track.
	pub fn new(track: moq_net::track::Producer, config: ProducerConfig) -> Self {
		Self {
			inner: Arc::new(Mutex::new(Inner {
				track,
				compression: config.compression,
			})),
		}
	}

	/// Create a subscriber for the underlying track.
	pub fn consume(&self) -> moq_net::track::Subscriber {
		self.inner.lock().unwrap().track.subscribe(None)
	}

	/// Whether any consumer for the underlying track currently exists.
	///
	/// The demand signal for a producer serving on request: an unused track is cached state nobody is
	/// watching, safe to drop and recreate on the next request.
	pub fn is_used(&self) -> bool {
		self.inner
			.lock()
			.unwrap()
			.track
			.poll_unused(&kio::Waiter::noop())
			.is_pending()
	}

	/// Publish a new value, superseding the previous one.
	///
	/// Unlike [`moq-json`](https://docs.rs/moq-json), an identical value is republished rather than
	/// skipped: comparing two opaque blobs costs a full scan, and only the caller knows whether its
	/// bytes changed.
	pub fn update(&mut self, payload: impl Into<Bytes>) -> Result<()> {
		self.inner.lock().unwrap().update(payload.into())
	}

	/// Finish the track.
	pub fn finish(&mut self) -> Result<()> {
		self.inner.lock().unwrap().finish()
	}
}

/// Shared publishing state behind [`Producer`]'s `Arc<Mutex>`.
struct Inner {
	track: moq_net::track::Producer,
	compression: bool,
}

impl Inner {
	fn update(&mut self, payload: Bytes) -> Result<()> {
		// One frame per group, so the window spans a single value and starts cold every time.
		let payload = match self.compression {
			true => {
				// Compression can take a large value under the group's frame limit, but every consumer
				// decodes with moq-flate's default output cap, so publishing past it would advertise a
				// value that always fails to read. Reject it here instead.
				if payload.len() as u64 > moq_flate::DEFAULT_MAX_FRAME_SIZE {
					return Err(moq_flate::Error::TooLarge(moq_flate::DEFAULT_MAX_FRAME_SIZE).into());
				}
				moq_flate::Encoder::new().frame(&payload)
			}
			false => payload,
		};

		// Check before opening a group. `append_group` publishes immediately, so discovering the limit
		// inside `write_frame` would leave an empty newest group behind: a snapshot consumer jumps to
		// the newest, so the previous value would be lost even though this update reported an error.
		if payload.len() as u64 > moq_net::group::MAX_CACHE_BYTES {
			return Err(moq_net::Error::FrameTooLarge.into());
		}

		let mut group = self.track.append_group()?;
		if let Err(err) = group.write_frame(moq_net::Timestamp::now(), payload) {
			// `append_group` already published this group, and a rejected frame (too large) doesn't
			// close the track. Dropping the handle does NOT close the group, so leaving it would strand
			// any subscriber that advanced into it with nothing to read and no end.
			let _ = group.finish();
			return Err(err.into());
		}

		group.finish()?;
		Ok(())
	}

	fn finish(&mut self) -> Result<()> {
		self.track.finish()?;
		Ok(())
	}
}
