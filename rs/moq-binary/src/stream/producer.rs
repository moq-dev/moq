//! Publishing an ordered log of binary payloads over a track.

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
	/// Compress the group as one sync-flushed DEFLATE stream, so each payload reuses the earlier
	/// ones as context.
	///
	/// `false` (the default) writes the bytes through untouched. A [`Consumer`](super::Consumer)
	/// must set [`ConsumerConfig::compression`](super::ConsumerConfig::compression) to match.
	pub compression: bool,
}

impl ProducerConfig {
	/// Set [`compression`](Self::compression) (a builder, since the struct is `#[non_exhaustive]`).
	pub fn with_compression(mut self, compression: bool) -> Self {
		self.compression = compression;
		self
	}
}

/// Publishes an ordered log of binary payloads over a track, one payload per frame in a single
/// group.
///
/// Cheaply clonable: clones share one underlying track and publishing state, so multiple owners
/// append into a single ordered log.
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
				group: None,
				flate: config.compression.then(moq_flate::Encoder::new),
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

	/// Append one payload to the log.
	pub fn append(&mut self, payload: impl Into<Bytes>) -> Result<()> {
		self.inner.lock().unwrap().append(payload.into())
	}

	/// Finish the track, closing the group.
	pub fn finish(&mut self) -> Result<()> {
		self.inner.lock().unwrap().finish()
	}
}

/// Shared publishing state behind [`Producer`]'s `Arc<Mutex>`.
struct Inner {
	track: moq_net::track::Producer,

	/// Opened on the first append and never rolled, except to recover from a failed write.
	group: Option<moq_net::group::Producer>,

	/// The DEFLATE encoder, one window for the whole group, `Some` while compressing.
	flate: Option<moq_flate::Encoder>,
	compression: bool,
}

impl Inner {
	fn append(&mut self, payload: Bytes) -> Result<()> {
		// Open the group before compressing: a failure here must not leave the window ahead of a
		// consumer that never received the frame.
		if self.group.is_none() {
			self.group = Some(self.track.append_group()?);
		}

		let payload = match self.flate.as_mut() {
			Some(flate) => flate.frame(&payload),
			None => payload,
		};

		let group = self.group.as_mut().expect("a group is open");
		let Err(err) = group.write_frame(moq_net::Timestamp::now(), payload) else {
			return Ok(());
		};

		// The frame never reached the wire, so a compressed window is now ahead of every consumer and
		// nothing later in this group could be decoded. A log has no keyframe to resynchronize on, so
		// recovery is a fresh group with a cold window, which the next append opens.
		//
		// The group is already published and dropping the handle does not close it, so a subscriber
		// that advanced into it would wait there with nothing to read. Close it explicitly.
		if let Some(mut group) = self.group.take() {
			let _ = group.finish();
		}
		self.flate = self.compression.then(moq_flate::Encoder::new);

		Err(err.into())
	}

	fn finish(&mut self) -> Result<()> {
		if let Some(mut group) = self.group.take() {
			group.finish()?;
		}
		self.track.finish()?;
		Ok(())
	}
}
