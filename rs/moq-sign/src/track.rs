//! Signing producer and verifying subscriber for one track.

use std::fmt;
use std::sync::{Arc, Mutex};
use std::task::{Poll, ready};

use crate::error::Result;
use crate::group;
use crate::key::{PublicKey, SecretKey};
use crate::scope::Scope;

/// Producer for one signed track.
///
/// Group sequences only rise under an epoch, even across a replacement of the track, so no two
/// groups are signed at the same coordinates. A relay holding an aborted group and a new group at
/// its sequence could otherwise splice their frames into a group that was never published.
pub struct Producer {
	inner: moq_net::track::Producer,
	key: SecretKey,
	scope: Arc<Scope>,
	// The lowest sequence this instance has not signed on the track, shared with its replacements.
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

	/// Start a group one past the highest sequence this instance signed on the track.
	///
	/// # Errors
	///
	/// A net error if the track is closed.
	pub fn append_group(&self) -> Result<group::Producer> {
		self.claim(None)
	}

	/// Start a group at an explicit sequence, above any this instance signed on the track.
	///
	/// # Errors
	///
	/// [`moq_net::Error::Duplicate`] if this instance already signed the sequence or a later one,
	/// even in a group since aborted or evicted, or a net error if the track is closed.
	pub fn create_group(&self, sequence: u64) -> Result<group::Producer> {
		self.claim(Some(sequence))
	}

	fn claim(&self, sequence: Option<u64>) -> Result<group::Producer> {
		let mut next = self.next.lock().expect("signed sequences");
		let sequence = sequence.unwrap_or(*next);
		if sequence < *next {
			return Err(moq_net::Error::Duplicate.into());
		}
		let after = sequence
			.checked_add(1)
			.ok_or(moq_net::Error::from(moq_net::BoundsExceeded))?;
		let inner = self.inner.create_group(moq_net::group::Info { sequence })?;
		*next = after;
		Ok(self.group(inner))
	}

	fn group(&self, inner: moq_net::group::Producer) -> group::Producer {
		group::Producer::new(inner, self.key.clone(), Arc::clone(&self.scope))
	}

	/// Finish the track after the last group.
	///
	/// # Errors
	///
	/// A net error if the track is already closed.
	pub fn finish(self) -> Result<()> {
		self.inner.finish()?;
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

/// The pollable state of a [`broadcast::Consumer::subscribe`](crate::broadcast::Consumer::subscribe);
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

	/// Poll for the next group in arrival order.
	///
	/// # Errors
	///
	/// A net error if the track fails.
	pub fn poll_recv_group(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<group::Consumer>>> {
		let Some(inner) = ready!(self.inner.poll_recv_group(waiter)?) else {
			return Poll::Ready(Ok(None));
		};
		Poll::Ready(Ok(Some(group::Consumer::new(inner, self.key, Arc::clone(&self.scope)))))
	}

	/// Receive the next group in arrival order.
	///
	/// # Errors
	///
	/// Same as [`Self::poll_recv_group`].
	pub async fn recv_group(&mut self) -> Result<Option<group::Consumer>> {
		kio::wait(|waiter| self.poll_recv_group(waiter)).await
	}
}

impl fmt::Debug for Consumer {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("track::Consumer").field("name", &self.name()).finish()
	}
}
