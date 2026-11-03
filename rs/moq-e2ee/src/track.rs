//! Grouped-frame writers and readers for one physical stream track.

use std::fmt;
use std::sync::{Arc, Mutex};
use std::task::{Poll, ready};

use crate::error::Result;
use crate::generation::Domain;
use crate::group;
use crate::key::{Claim, TrackKey};
use crate::terminal::Terminal;

/// Exclusive producer for one physical stream track.
///
/// Uses the grouped-frame key and the group sequences of its semantic name, shared with every
/// producer of that name under the generation. Sequences are allocated monotonically, so a
/// re-created or re-served name resumes above every sequence allocated before; reuse and
/// exhaustion are refused before encryption.
pub struct Producer {
	inner: moq_net::track::Producer,
	claim: Claim,
}

impl Producer {
	pub(crate) fn new(inner: moq_net::track::Producer, claim: Claim) -> Self {
		Self { inner, claim }
	}

	/// The physical track name.
	pub fn name(&self) -> &str {
		self.inner.name()
	}

	/// Allocate the next sequence and start a group there.
	///
	/// # Errors
	///
	/// [`Error::Identity`](crate::Error::Identity) if the next sequence exceeds `2^53-1`, or a net error.
	pub fn append_group(&mut self) -> Result<group::Producer> {
		self.start_group(None)
	}

	/// Start a group at an explicit sequence, which must be above every sequence this name
	/// allocated under the generation.
	///
	/// # Errors
	///
	/// [`Error::Reuse`](crate::Error::Reuse) if `sequence` was already allocated, [`Error::Identity`](crate::Error::Identity) if
	/// it exceeds `2^53-1`, or a net error.
	pub fn create_group(&mut self, sequence: u64) -> Result<group::Producer> {
		self.start_group(Some(sequence))
	}

	fn start_group(&mut self, sequence: Option<u64>) -> Result<group::Producer> {
		let (sequence, ()) = self.claim.allocate(sequence, |_| Ok(()))?;
		let inner = self.inner.create_group(moq_net::group::Info { sequence })?;
		Ok(group::Producer::new(inner, Arc::clone(&self.claim.key)))
	}

	/// Finish the track after the last allocated sequence.
	///
	/// # Errors
	///
	/// A net error if the track is already closed.
	pub fn finish(self) -> Result<()> {
		self.inner.finish()?;
		Ok(())
	}

	/// Abort the track, consuming the handle.
	///
	/// # Errors
	///
	/// A net error if the track is already closed.
	pub fn abort(self) -> Result<()> {
		self.inner.abort(moq_net::Error::Cancel)?;
		Ok(())
	}
}

impl fmt::Debug for Producer {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("track::Producer")
			.field("name", &self.name())
			.field("next", &*self.claim.next.lock().expect("claim sequence"))
			.finish()
	}
}

/// A requested or reserved track its subscribers wait on until it is accepted.
///
/// Yielded by [`broadcast::Requests::poll_requested_track`](crate::broadcast::Requests::poll_requested_track)
/// or made by [`broadcast::Producer::reserve_track`](crate::broadcast::Producer::reserve_track),
/// which claims its name up front. A requested name is claimed only once accepted, so a
/// subscriber cannot make the publisher hold names it never serves. Accept it once its
/// [`demand`](Self::demand) shows a subscriber, or up front. Dropping it without
/// [`accept`](Self::accept) rejects the waiting subscribers.
pub struct Request {
	inner: moq_net::track::Request,
	generation: crate::generation::Producer,
	// From `generation::Producer::peek` or `claim`; claimed by `accept`.
	claim: Claim,
	semantic: String,
}

impl Request {
	pub(crate) fn new(
		inner: moq_net::track::Request,
		generation: crate::generation::Producer,
		claim: Claim,
		semantic: String,
	) -> Self {
		Self {
			inner,
			generation,
			claim,
			semantic,
		}
	}

	/// The semantic track name.
	pub fn name(&self) -> &str {
		&self.semantic
	}

	/// Watch subscriber demand without keeping the track alive.
	pub fn demand(&self) -> moq_net::track::Demand {
		self.inner.demand()
	}

	/// Serve the track, claiming its name and resolving every waiting subscriber.
	pub fn accept(self, info: impl Into<Option<moq_net::track::Info>>) -> Producer {
		let claim = self.generation.adopt(&self.semantic, Domain::Group, self.claim);
		Producer::new(self.inner.accept(info), claim)
	}

	/// Fail every waiting subscriber with `err`. This claims nothing, and releases no earlier claim.
	pub fn reject(self, err: moq_net::Error) {
		self.inner.reject(err);
	}
}

impl fmt::Debug for Request {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("track::Request").field("name", &self.name()).finish()
	}
}

/// The pollable state of a [`broadcast::Consumer::subscribe`](crate::broadcast::Consumer::subscribe);
/// awaited via the [`kio::Pending`] wrapper, whose `DerefMut` exposes [`Self::update`].
pub struct Subscribing {
	inner: kio::Pending<moq_net::track::Subscribing>,
	generation: crate::generation::Consumer,
	// Keys derive from the name subscribed to, never from the name a relay hands back.
	semantic: String,
}

impl Subscribing {
	pub(crate) fn new(
		inner: kio::Pending<moq_net::track::Subscribing>,
		generation: crate::generation::Consumer,
		semantic: String,
	) -> Self {
		Self {
			inner,
			generation,
			semantic,
		}
	}

	/// Poll until the track is found, yielding a decrypting [`Consumer`].
	///
	/// # Errors
	///
	/// A net error if the track is aborted or not found.
	pub fn poll_ok(&self, waiter: &kio::Waiter) -> Poll<Result<Consumer>> {
		let subscriber = ready!(self.inner.poll_ok(waiter))?;
		Poll::Ready(self.generation.consume(subscriber, &self.semantic))
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

/// Subscriber for one physical stream track.
///
/// Grouped authentication failure ends the track and wakes pending reads with
/// [`Error::Authentication`](crate::Error::Authentication).
pub struct Consumer {
	name: String,
	// Shared with every other subscription to this name under the generation.
	key: Arc<Mutex<TrackKey>>,
	terminal: Arc<Terminal>,
}

impl Consumer {
	pub(crate) fn new(inner: moq_net::track::Subscriber, key: Arc<Mutex<TrackKey>>) -> Self {
		let name = inner.name().to_string();
		Self {
			name,
			key,
			terminal: Terminal::new(Some(inner)),
		}
	}

	/// The physical track name.
	pub fn name(&self) -> &str {
		&self.name
	}

	/// Poll for the next protected group in arrival order.
	///
	/// # Errors
	///
	/// [`Error::Authentication`](crate::Error::Authentication) once a grouped frame has failed to open, or a net error.
	pub fn poll_recv_group(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<group::Consumer>>> {
		let terminal = Arc::clone(&self.terminal);
		let key = Arc::clone(&self.key);
		terminal.poll_subscription(waiter, |inner, waiter| {
			let Some(inner) = ready!(inner.poll_recv_group(waiter)?) else {
				return Poll::Ready(Ok(None));
			};
			Poll::Ready(Ok(Some(group::Consumer::new(
				inner,
				Arc::clone(&key),
				Arc::clone(&terminal),
			))))
		})
	}

	/// Receive the next protected group in arrival order.
	///
	/// # Errors
	///
	/// Same as [`Self::poll_recv_group`].
	pub async fn recv_group(&mut self) -> Result<Option<group::Consumer>> {
		kio::wait(|waiter| self.poll_recv_group(waiter)).await
	}
}

// Groups share the terminal, so without this a held group would keep the
// subscription, and publisher demand, alive after the track is dropped.
impl Drop for Consumer {
	fn drop(&mut self) {
		self.terminal.unsubscribe();
	}
}

impl fmt::Debug for Consumer {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("track::Consumer")
			.field("name", &self.name())
			.field("auth_failed", &self.terminal.is_failed())
			.finish()
	}
}
