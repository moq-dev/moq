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
	///
	/// A payload that cannot be written ends the track: a log missing a record is not the lossless
	/// log this mode promises, so the failure is surfaced rather than papered over with a second
	/// group. The group is aborted rather than closed cleanly, so a consumer sees the failure
	/// instead of a log that merely looks complete. Every later append fails on the closed track.
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

	/// Opened on the first append and never rolled.
	group: Option<moq_net::group::Producer>,

	/// The DEFLATE encoder, one window for the whole group, `Some` while compressing.
	flate: Option<moq_flate::Encoder>,
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

		// The payload never reached the wire, so the log has a hole in it, which is not the lossless
		// log this mode promises. Continuing into a second group would hand consumers a gap dressed up
		// as a complete log, so end the track and let the caller start a new one. This is also what
		// keeps "a stream is one group" a real invariant rather than the usual case.
		//
		// Abort the group rather than finishing it: a clean close would drain to `None`, which is
		// exactly what a completed log looks like, so a consumer could not tell a truncated log from
		// a whole one. Aborting surfaces the failure to the reader the same way the error surfaces it
		// to the caller. The track is then finished so nothing opens a second group; dropping the
		// group handle would not close it, stranding a subscriber that had advanced into it.
		if let Some(group) = self.group.take() {
			let _ = group.abort(err.clone());
		}
		let _ = self.track.finish();

		Err(err.into())
	}

	fn finish(&mut self) -> Result<()> {
		// Finalize both independently rather than short-circuiting on the group. Returning early
		// would leave the track open with `group` already taken, so a later append would open a
		// second group, and (with compression) write into it from a window the consumer never
		// received. That is exactly the split log ending the track exists to prevent.
		let group = match self.group.take() {
			Some(mut group) => group.finish(),
			None => Ok(()),
		};
		let track = self.track.finish();

		group?;
		track?;
		Ok(())
	}
}
