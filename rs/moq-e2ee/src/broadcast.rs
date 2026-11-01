//! Protected broadcasts addressed by semantic track names.
//!
//! The application names tracks the way it would in plaintext (`catalog.json`, `video`). These
//! wrappers derive the physical name of each, so only physical names reach `moq-net` and the
//! relays behind it.

use std::task::{Poll, ready};

use crate::error::{Error, Result};
use crate::{Name, generation, track};

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
	/// A name this generation already produced, even on another broadcast, resumes above
	/// every sequence it allocated.
	///
	/// # Errors
	///
	/// [`Error::Identity`] if `semantic` exceeds 65535 bytes, or a net error if the broadcast
	/// already holds it.
	pub fn create_track(
		&self,
		semantic: &str,
		info: impl Into<Option<moq_net::track::Info>>,
	) -> Result<track::Producer> {
		let name = self.generation.name(semantic)?;
		let track = self.inner.create_track(name.as_str(), info)?;
		self.generation.produce(track, semantic)
	}

	/// Reserve the track for `semantic` under its physical name, to accept once it is wanted.
	///
	/// Subscribers wait on the name until the [`track::Request`] is accepted or rejected.
	/// A track served on demand is reserved this way, or answered through [`Self::requests`].
	///
	/// # Errors
	///
	/// Same as [`Self::create_track`].
	pub fn reserve_track(&self, semantic: &str) -> Result<track::Request> {
		let name = self.generation.name(semantic)?;
		let request = self.inner.reserve_track(name.as_str())?;
		match self.generation.claim(semantic) {
			Ok(keys) => Ok(track::Request::new(
				request,
				self.generation.clone(),
				keys,
				semantic.to_string(),
			)),
			Err(err) => {
				request.reject(moq_net::Error::Cancel);
				Err(err)
			}
		}
	}

	/// Create a handler for requests to tracks nobody created or reserved, named by their
	/// semantic names.
	pub fn requests(&self) -> Requests {
		Requests {
			inner: self.inner.requests(),
			generation: self.generation.clone(),
		}
	}
}

/// Handles on-demand track creation for a protected broadcast.
///
/// Yields each requested track under its semantic name, as
/// [`moq_net::broadcast::Requests`] does for a plaintext broadcast. A requested name that this
/// generation did not issue, such as one a relay forged or another epoch's, is answered as
/// [`moq_net::Error::NotFound`] after at most one open, and keeps no state. A name it did issue
/// is claimed only when the handler accepts its request.
#[derive(Clone)]
pub struct Requests {
	inner: moq_net::broadcast::Requests,
	generation: generation::Producer,
}

impl Requests {
	/// The broadcast's static metadata, fixed when it was created.
	pub fn info(&self) -> &moq_net::broadcast::Info {
		self.inner.info()
	}

	/// Poll for the next consumer-requested track, without blocking.
	///
	/// # Errors
	///
	/// [`moq_net::Error::Closed`] once the broadcast has ended.
	pub fn poll_requested_track(&mut self, waiter: &kio::Waiter) -> Poll<Result<track::Request>> {
		loop {
			let request = ready!(self.inner.poll_requested_track(waiter))?;
			match self.open(request.name()) {
				Ok((keys, semantic)) => {
					let generation = self.generation.clone();
					return Poll::Ready(Ok(track::Request::new(request, generation, keys, semantic)));
				}
				Err(_) => request.reject(moq_net::Error::NotFound),
			}
		}
	}

	/// Open a requested physical name to its semantic name and keys, claiming nothing: the
	/// request claims the name only once the handler accepts it.
	fn open(&self, name: &str) -> Result<(track::Keys, String)> {
		// Syntax first, so a name that cannot be one costs no open.
		let name: Name = name.parse()?;
		let semantic = self.generation.open(&name)?;
		Ok((self.generation.peek(&semantic)?, semantic))
	}

	/// Block until a consumer requests a track, returning a [`track::Request`] to serve.
	///
	/// # Errors
	///
	/// Same as [`Self::poll_requested_track`].
	pub async fn requested_track(&mut self) -> Result<track::Request> {
		kio::wait(|waiter| self.poll_requested_track(waiter)).await
	}

	/// Create a consumer that can subscribe to tracks in this broadcast.
	pub fn consume(&self) -> Consumer {
		Consumer::new(self.inner.consume(), self.generation.consumer().clone())
	}

	/// Answer the broadcast's track listings under semantic names, for a handler that knows the
	/// tracks it would serve. See [`moq_net::broadcast::Lister`].
	///
	/// # Errors
	///
	/// [`moq_net::Error::Duplicate`] while another lister exists.
	pub fn lister(&self) -> Result<Lister> {
		Ok(Lister {
			inner: self.inner.lister()?,
			generation: self.generation.clone(),
		})
	}

	/// Block until the broadcast ends.
	pub async fn closed(&self) {
		kio::wait(|waiter| self.poll_closed(waiter)).await
	}

	/// Poll until the broadcast ends.
	pub fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.inner.poll_closed(waiter)
	}

	/// Return true if this is the same broadcast instance.
	pub fn is_clone(&self, other: &Self) -> bool {
		self.inner.is_clone(&other.inner)
	}
}

/// A handler's answer to a protected broadcast's listings, taking semantic names and listing
/// their physical names. See [`moq_net::broadcast::Lister`].
pub struct Lister {
	inner: moq_net::broadcast::Lister,
	generation: generation::Producer,
}

impl Lister {
	/// Ready once anyone lists the broadcast.
	///
	/// # Errors
	///
	/// [`moq_net::Error::Closed`] once the broadcast ended.
	pub fn poll_wanted(&self, waiter: &kio::Waiter) -> Poll<Result<()>> {
		self.inner.poll_wanted(waiter).map_err(Error::Net)
	}

	/// Ready once nobody lists the broadcast.
	///
	/// # Errors
	///
	/// [`moq_net::Error::Closed`] once the broadcast ended.
	pub fn poll_unwanted(&self, waiter: &kio::Waiter) -> Poll<Result<()>> {
		self.inner.poll_unwanted(waiter).map_err(Error::Net)
	}

	/// List the track for `semantic`.
	///
	/// # Errors
	///
	/// [`Error::Identity`] if `semantic` exceeds 65535 bytes, or [`moq_net::Error::Duplicate`]
	/// if this answer already lists it.
	pub fn add(&self, semantic: &str, info: moq_net::track::Info) -> Result<()> {
		let name = self.generation.name(semantic)?;
		Ok(self.inner.add(name.as_str(), info)?)
	}

	/// Stop listing the track for `semantic`, which left unfinished.
	///
	/// # Errors
	///
	/// [`Error::Identity`] if `semantic` exceeds 65535 bytes, or [`moq_net::Error::NotFound`]
	/// if this answer does not list it.
	pub fn remove(&self, semantic: &str) -> Result<()> {
		let name = self.generation.name(semantic)?;
		Ok(self.inner.remove(name.as_str())?)
	}

	/// Stop listing the track for `semantic`, which finished.
	///
	/// # Errors
	///
	/// Same as [`Self::remove`].
	pub fn finish(&self, semantic: &str) -> Result<()> {
		let name = self.generation.name(semantic)?;
		Ok(self.inner.finish(name.as_str())?)
	}

	/// The answer holds everything this hop knows now; later changes are live.
	pub fn synced(&self) {
		self.inner.synced();
	}

	/// Withdraw the answer: every track it listed, and its sync.
	pub fn clear(&self) {
		self.inner.clear();
	}

	/// End every current reader as if the broadcast ended, and withdraw the answer.
	pub fn end(&self) {
		self.inner.end();
	}

	/// Fail every current reader with `err` and withdraw the answer.
	pub fn abort(&self, err: moq_net::Error) {
		self.inner.abort(err);
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
			semantic.to_string(),
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

impl std::fmt::Debug for Requests {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("broadcast::Requests")
			.field("generation", &self.generation)
			.finish()
	}
}

impl std::fmt::Debug for Lister {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("broadcast::Lister")
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
