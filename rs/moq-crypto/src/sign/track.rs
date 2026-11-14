//! Signing producer and verifying subscriber for one track.

use std::fmt;
use std::sync::{Arc, Mutex};
use std::task::{Poll, ready};

use crate::sign::error::Result;
use crate::sign::group;
use crate::sign::scope::Scope;
use crate::sign::{PublicKey, SecretKey};

/// Producer for one signed track.
///
/// Group starts only rise under an instance, even across a replacement of the track, so no two
/// groups are signed at the same coordinates. A relay holding an aborted group and a new group at
/// its start could otherwise splice their frames into a group that was never published.
pub struct Producer {
	inner: moq_net::track::Producer,
	key: SecretKey,
	scope: Arc<Scope>,
	// The lowest start this instance has not signed on the track, shared with its replacements.
	next: Arc<Mutex<u64>>,
}

impl Producer {
	pub(crate) fn new(
		inner: moq_net::track::Producer,
		key: SecretKey,
		scope: Arc<Scope>,
		next: Arc<Mutex<u64>>,
	) -> Self {
		Self {
			inner,
			key,
			scope,
			next,
		}
	}

	/// The track name.
	pub fn name(&self) -> &str {
		self.inner.name()
	}

	/// Start the next group at `start`, in the track's timescale, as
	/// [`moq_net::track::Producer::append_group`] does, above any start this instance signed on
	/// the track.
	///
	/// # Errors
	///
	/// [`moq_net::Error::Duplicate`] if this instance already signed the start or a later one,
	/// even in a group since aborted or evicted, or a net error if the track refuses it.
	pub fn append_group(&self, start: u64) -> Result<group::Producer> {
		self.claim(start, |start| self.inner.append_group(start))
	}

	/// Start a group at `start`, in the track's timescale, as
	/// [`moq_net::track::Producer::create_group`] does, above any start this instance signed on
	/// the track.
	///
	/// # Errors
	///
	/// [`moq_net::Error::Duplicate`] if this instance already signed the start or a later one,
	/// even in a group since aborted or evicted, or a net error if the track refuses it.
	pub fn create_group(&self, start: u64) -> Result<group::Producer> {
		self.claim(start, |start| self.inner.create_group(moq_net::group::Info { start }))
	}

	fn claim(
		&self,
		start: u64,
		create: impl FnOnce(u64) -> moq_net::Result<moq_net::group::Producer>,
	) -> Result<group::Producer> {
		let mut next = self.next.lock().expect("signed starts");
		if start < *next {
			return Err(moq_net::Error::Duplicate.into());
		}
		let after = start
			.checked_add(1)
			.ok_or(moq_net::Error::from(moq_net::BoundsExceeded))?;
		let inner = create(start)?;
		*next = after;
		Ok(self.group(inner))
	}

	fn group(&self, inner: moq_net::group::Producer) -> group::Producer {
		group::Producer::new(
			inner,
			self.inner.info().clone(),
			self.key.clone(),
			Arc::clone(&self.scope),
		)
	}

	/// Finish the track after the last group.
	///
	/// # Errors
	///
	/// A net error if the track is already closed.
	pub fn finish(self) -> Result<()> {
		drop(self.inner.finish()?);
		Ok(())
	}

	/// Abort the track with `err`, consuming the handle.
	///
	/// # Errors
	///
	/// A net error if the track is already closed.
	pub fn abort(self, err: moq_net::Error) -> Result<()> {
		self.inner.abort(err)?;
		Ok(())
	}
}

impl fmt::Debug for Producer {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("track::Producer").field("name", &self.name()).finish()
	}
}

/// The pollable state of a [`broadcast::Consumer::subscribe`](crate::sign::broadcast::Consumer::subscribe);
/// awaited via the [`kio::Pending`] wrapper, whose `DerefMut` exposes [`Self::update`].
pub struct Subscribing {
	inner: kio::Pending<moq_net::track::Subscribing>,
	key: PublicKey,
	// Bound to the name subscribed to, never to the name a relay hands back.
	scope: Arc<Scope>,
}

impl Subscribing {
	pub(crate) fn new(inner: kio::Pending<moq_net::track::Subscribing>, key: PublicKey, scope: Arc<Scope>) -> Self {
		Self { inner, key, scope }
	}

	/// Poll until the track is found, yielding a verifying [`Consumer`].
	///
	/// # Errors
	///
	/// A net error if the track is aborted or not found.
	pub fn poll_ok(&self, waiter: &kio::Waiter) -> Poll<Result<Consumer>> {
		let inner = ready!(self.inner.poll_ok(waiter))?;
		Poll::Ready(Ok(Consumer {
			inner,
			key: self.key,
			scope: Arc::clone(&self.scope),
		}))
	}

	/// Change the subscription preferences before (or after) it resolves.
	///
	/// # Errors
	///
	/// [`moq_net::Error::Closed`] if the track already ended.
	pub fn update(&mut self, subscription: moq_net::track::Subscription) -> Result<()> {
		Ok(self.inner.update(subscription)?)
	}
}

impl kio::Task for Subscribing {
	type Output = Result<Consumer>;

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Self::Output> {
		self.poll_ok(waiter)
	}
}

/// Subscriber for one signed track.
///
/// Each group verifies on its own: a forged frame ends only the group that carried it.
pub struct Consumer {
	inner: moq_net::track::Subscriber,
	key: PublicKey,
	scope: Arc<Scope>,
}

impl Consumer {
	/// The track name.
	pub fn name(&self) -> &str {
		self.inner.name()
	}

	/// The next group in arrival order, or `None` once the track finished.
	///
	/// # Errors
	///
	/// A net error if the track fails.
	#[cfg_attr(feature = "async", kio::wrapper)]
	pub fn poll_recv_group(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<group::Consumer>>> {
		let Some(inner) = ready!(self.inner.poll_recv_group(waiter)?) else {
			return Poll::Ready(Ok(None));
		};
		Poll::Ready(Ok(Some(group::Consumer::new(inner, self.key, Arc::clone(&self.scope)))))
	}
}

impl fmt::Debug for Consumer {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("track::Consumer").field("name", &self.name()).finish()
	}
}
