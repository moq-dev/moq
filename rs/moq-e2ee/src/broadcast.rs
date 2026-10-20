//! Protected broadcasts addressed by semantic track names.
//!
//! The application names tracks the way it would in plaintext (`catalog.json`, `video`). These
//! wrappers derive the physical name of each, so only physical names reach `moq-net` and the
//! relays behind it.

use crate::error::{Error, Result};
use crate::{generation, track};

/// The publishing side of a protected broadcast.
///
/// Wraps the `moq-net` broadcast at [`Credential::path`](crate::Credential::path). Announce that
/// broadcast with [`generation().epoch()`](generation::Producer::epoch) on its route once the
/// tracks a subscriber needs first exist.
#[derive(Clone)]
pub struct Producer {
	inner: moq_net::broadcast::Producer,
	generation: generation::Producer,
}

impl Producer {
	/// Protect `inner` under `generation`.
	pub fn new(inner: moq_net::broadcast::Producer, generation: generation::Producer) -> Self {
		Self { inner, generation }
	}

	/// The generation this broadcast's tracks are protected under.
	pub fn generation(&self) -> &generation::Producer {
		&self.generation
	}

	/// Create the track for `semantic` under its physical name.
	///
	/// # Errors
	///
	/// [`Error::Identity`] if `semantic` exceeds 65535 bytes, [`Error::Reuse`] if this
	/// generation already produced the name (even on another broadcast), or a net error if the
	/// broadcast already holds it.
	pub fn create_track(
		&self,
		semantic: &str,
		info: impl Into<Option<moq_net::track::Info>>,
	) -> Result<track::Producer> {
		let name = self.generation.name(semantic)?;
		let track = self.inner.create_track(name.as_str(), info)?;
		self.generation.produce(track)
	}

	/// Reserve the track for `semantic` under its physical name, to accept once it is wanted.
	///
	/// Subscribers wait on the name until the [`track::Request`] is accepted or rejected.
	/// A track served on demand is reserved this way: physical names cannot be mapped back,
	/// so a request for a name nobody created or reserved fails as not found.
	///
	/// # Errors
	///
	/// Same as [`Self::create_track`].
	pub fn reserve_track(&self, semantic: &str) -> Result<track::Request> {
		let name = self.generation.name(semantic)?;
		let request = self.inner.reserve_track(name.as_str())?;
		match self.generation.claim(name.as_str()) {
			Ok(keys) => Ok(track::Request::new(request, keys)),
			Err(err) => {
				request.reject(moq_net::Error::Cancel);
				Err(err)
			}
		}
	}
}

/// The subscribing side of a protected broadcast.
///
/// Wraps the `moq-net` broadcast found at [`Credential::path`](crate::Credential::path), bound to
/// the epoch on its route.
#[derive(Clone)]
pub struct Consumer {
	inner: moq_net::broadcast::Consumer,
	generation: generation::Consumer,
}

impl Consumer {
	/// Read `inner` under `generation`.
	pub fn new(inner: moq_net::broadcast::Consumer, generation: generation::Consumer) -> Self {
		Self { inner, generation }
	}

	/// The generation this broadcast's tracks are read under.
	pub fn generation(&self) -> &generation::Consumer {
		&self.generation
	}

	/// Subscribe to the track for `semantic` under its physical name.
	///
	/// # Errors
	///
	/// [`Error::Identity`] if `semantic` exceeds 65535 bytes, or a net error if the broadcast
	/// has ended or serves no such track, as for any missing track.
	pub fn subscribe(
		&self,
		semantic: &str,
		subscription: impl Into<Option<moq_net::track::Subscription>>,
	) -> Result<kio::Pending<track::Subscribing>> {
		let name = self.generation.name(semantic)?;
		let track = self.inner.track(name.as_str()).map_err(Error::Net)?;
		Ok(kio::Pending::new(track::Subscribing::new(
			track.subscribe(subscription),
			self.generation.clone(),
		)))
	}
}

impl std::fmt::Debug for Producer {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("broadcast::Producer")
			.field("generation", &self.generation)
			.finish()
	}
}

impl std::fmt::Debug for Consumer {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("broadcast::Consumer")
			.field("generation", &self.generation)
			.finish()
	}
}
