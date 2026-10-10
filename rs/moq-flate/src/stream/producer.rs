//! Publishing an ordered log of opaque payloads over a track.

use std::sync::{Arc, Mutex};
use std::task::Poll;

use bytes::Bytes;
use moq_net::Timed;

use crate::Result;

pub use super::Config;

/// Publishes an ordered log of opaque payloads over a track, one payload per frame in a single
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
	pub fn new(track: moq_net::track::Producer, config: Config) -> Self {
		Self {
			inner: Arc::new(Mutex::new(Inner {
				track,
				group: None,
				flate: config.compression.is_deflate().then(crate::Encoder::new),
				frames: 0,
				bytes: 0,
			})),
		}
	}

	/// Create a subscriber for the underlying track.
	///
	/// Still hands one back once a failed write has ended the log: the subscriber surfaces the abort
	/// on its first read, which is what tells a late reader the log is truncated.
	pub fn consume(&self) -> moq_net::track::Subscriber {
		self.inner.lock().unwrap().track.subscribe(None)
	}

	/// A watch-only handle to the underlying track's subscriber demand.
	///
	/// Weak, so holding it neither keeps the track open nor contends with publishing.
	pub fn demand(&self) -> moq_net::track::Demand {
		self.inner.lock().unwrap().track.demand()
	}

	/// Append one payload to the log.
	///
	/// A payload that might not fit in what is left of the group's budget is refused with
	/// [`moq_net::Error::GroupTooLarge`] before anything is written, leaving the log intact. The
	/// budget covers the whole log, so once it is spent every append is refused; a publisher with
	/// more to say opens a new track.
	///
	/// Any other payload that cannot be written ends the track: a log missing a record is not the
	/// lossless log this mode promises, so the failure is surfaced rather than papered over with a
	/// second group. The group is aborted rather than closed cleanly, so a consumer sees the failure
	/// instead of a log that merely looks complete. Every later append fails on the closed track.
	///
	/// Returns the frame's encoded size.
	pub fn append(&mut self, payload: impl Into<Timed<Bytes>>) -> Result<usize> {
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
	flate: Option<crate::Encoder>,

	/// Frames and payload bytes written to the group, checked against moq-net's group budget before
	/// each payload is encoded.
	frames: usize,
	bytes: u64,
}

// The budget check also stands in for the decoder's cap: a payload that fits the group is one every
// consumer can inflate.
const _: () = assert!(moq_net::group::MAX_CACHE_BYTES <= crate::DEFAULT_MAX_FRAME_SIZE);

impl Inner {
	fn append(&mut self, payload: Timed<Bytes>) -> Result<usize> {
		let timestamp = payload.at.unwrap_or_else(moq_net::Timestamp::now);
		let payload = payload.value;

		// A closed track refuses every append, so say so before the budget: `GroupTooLarge` promises
		// the log is still writable.
		if let Poll::Ready(err) = self.track.poll_closed(&kio::Waiter::noop()) {
			return Err(err.into());
		}

		// Check before compressing: encoding advances the window, so a payload refused afterwards
		// would leave the encoder ahead of every reader. The worst case is checked rather than the
		// actual size for the same reason. Also checked before the group is opened, so a refused
		// first payload publishes nothing.
		let size = payload.len() as u64;
		let bound = if self.flate.is_some() {
			crate::Encoder::bound(size)
		} else {
			size
		};
		if self.frames >= moq_net::group::MAX_GROUP_FRAMES
			|| self.bytes.saturating_add(bound) > moq_net::group::MAX_CACHE_BYTES
		{
			return Err(moq_net::Error::GroupTooLarge.into());
		}

		// Open the group before compressing: a failure here must not leave the window ahead of a
		// consumer that never received the frame.
		if self.group.is_none() {
			self.group = Some(self.track.append_group()?);
		}

		let payload = match self.flate.as_mut() {
			Some(flate) => flate.frame(&payload),
			None => payload,
		};

		let size = payload.len();
		let group = self.group.as_mut().expect("a group is open");
		let Err(err) = group.write_frame(timestamp, payload) else {
			self.frames += 1;
			self.bytes += size as u64;
			return Ok(size);
		};

		// The payload never reached the wire, so the log has a hole in it, which is not the lossless
		// log this mode promises. Continuing into a second group would hand consumers a gap dressed up
		// as a complete log, so end the track and let the caller start a new one. This is also what
		// keeps "a stream is one group" a real invariant rather than the usual case.
		//
		// Abort the track rather than finishing it: a clean close drains a consumer to `None`, which
		// is exactly what a completed log looks like, so a truncated log would be indistinguishable
		// from a whole one. Aborting the *track* is what a subscriber observes; aborting only the
		// group drops it from the cache and the consumer still reads a clean end.
		self.abort(err.clone());

		Err(err.into())
	}

	/// End the track with an error, so a consumer sees the failure rather than a clean end.
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
		let _ = self.track.clone().abort(err);
	}

	fn finish(&mut self) -> Result<()> {
		// Finalize both independently rather than short-circuiting on the group. Returning early
		// would leave the track open with `group` already taken, so a later append would open a
		// second group, and (with compression) write into it from a window the consumer never
		// received. That is exactly the split log ending the track exists to prevent.
		let group = match self.group.take() {
			Some(group) => group.finish(),
			None => Ok(()),
		};
		let track = self.track.finish();

		group?;
		track?;
		Ok(())
	}
}
