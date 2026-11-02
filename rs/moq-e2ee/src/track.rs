//! Grouped-frame and datagram writers and readers for one physical track.

use std::fmt;
use std::sync::{Arc, Mutex};
use std::task::{Poll, ready};

use crate::datagram::{Datagram, Event};
use crate::error::{Error, Result};
use crate::group;
use crate::key::TrackKey;
use crate::limits::{MAX_DATAGRAM_PAYLOAD, check_u53};
use crate::terminal::Terminal;
use crate::window::DatagramWindow;

/// Exclusive producer for one physical track.
///
/// Uses the grouped-frame and datagram keys and the sequence namespace of its semantic name,
/// shared with every producer of that name under the generation. Sequences are allocated
/// monotonically, so a re-created or re-served name resumes above every sequence allocated
/// before; reuse and exhaustion are refused before encryption.
pub struct Producer {
	inner: moq_net::track::Producer,
	keys: Keys,
}

/// A semantic name's grouped-frame and datagram keys and its next unallocated sequence;
/// clones share their accounting.
#[derive(Clone, Debug)]
pub(crate) struct Keys {
	pub group: Arc<Mutex<TrackKey>>,
	pub datagram: Arc<Mutex<TrackKey>>,
	// The high-water mark plus one, kept for the generation's life so no sequence repeats.
	// Locked before either key.
	pub next: Arc<Mutex<u64>>,
}

impl Keys {
	pub(crate) fn new(group: TrackKey, datagram: TrackKey) -> Self {
		Self {
			group: Arc::new(Mutex::new(group)),
			datagram: Arc::new(Mutex::new(datagram)),
			next: Arc::default(),
		}
	}
}

impl Producer {
	pub(crate) fn new(inner: moq_net::track::Producer, keys: Keys) -> Self {
		Self { inner, keys }
	}

	/// The physical track name.
	pub fn name(&self) -> &str {
		self.inner.name()
	}

	/// Allocate the next sequence and start a group there.
	///
	/// # Errors
	///
	/// [`Error::Identity`] if the next sequence exceeds `2^53-1`, or a net error.
	pub fn append_group(&mut self) -> Result<group::Producer> {
		self.start_group(None)
	}

	/// Start a group at an explicit sequence, which must be above every sequence this name
	/// allocated under the generation.
	///
	/// # Errors
	///
	/// [`Error::Reuse`] if `sequence` was already allocated, [`Error::Identity`] if
	/// it exceeds `2^53-1`, or a net error.
	pub fn create_group(&mut self, sequence: u64) -> Result<group::Producer> {
		self.start_group(Some(sequence))
	}

	fn start_group(&mut self, sequence: Option<u64>) -> Result<group::Producer> {
		let sequence = {
			let mut next = self.keys.next.lock().expect("track sequence");
			let sequence = reserve(*next, sequence)?;
			*next = sequence + 1;
			sequence
		};
		let inner = self.inner.create_group(moq_net::group::Info { sequence })?;
		Ok(group::Producer::new(inner, Arc::clone(&self.keys.group)))
	}

	/// Encrypt `plaintext` at the next sequence and insert the datagram, returning that sequence.
	///
	/// # Errors
	///
	/// [`Error::Identity`], [`Error::Exhausted`], [`Error::Oversize`], or a net write error.
	pub fn append_datagram(
		&mut self,
		timestamp: impl Into<Option<moq_net::Timestamp>>,
		plaintext: &[u8],
	) -> Result<u64> {
		self.write_datagram(None, timestamp.into(), plaintext)
	}

	/// Encrypt `plaintext` at an explicit sequence and insert the datagram.
	///
	/// Plaintext is capped at [`MAX_DATAGRAM_PLAINTEXT`](crate::MAX_DATAGRAM_PLAINTEXT).
	/// A predictable failure is refused before the sequence is allocated; once
	/// encrypted, the identity is spent even if the net write fails.
	///
	/// # Errors
	///
	/// [`Error::Reuse`] if `sequence` was already allocated, [`Error::Identity`],
	/// [`Error::Exhausted`], [`Error::Oversize`], or a net write error.
	pub fn insert_datagram(
		&mut self,
		sequence: u64,
		timestamp: impl Into<Option<moq_net::Timestamp>>,
		plaintext: &[u8],
	) -> Result<()> {
		self.write_datagram(Some(sequence), timestamp.into(), plaintext)?;
		Ok(())
	}

	fn write_datagram(
		&mut self,
		sequence: Option<u64>,
		timestamp: Option<moq_net::Timestamp>,
		plaintext: &[u8],
	) -> Result<u64> {
		// Held across encryption, so another producer of this name cannot take the sequence.
		let (sequence, payload) = {
			let mut next = self.keys.next.lock().expect("track sequence");
			let sequence = reserve(*next, sequence)?;
			let payload =
				self.keys
					.datagram
					.lock()
					.expect("track key")
					.protect(sequence, 0, plaintext, MAX_DATAGRAM_PAYLOAD)?;
			*next = sequence + 1;
			(sequence, payload)
		};
		self.inner.insert_datagram(sequence, timestamp, payload)?;
		Ok(sequence)
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

	#[cfg(test)]
	pub(crate) fn datagram_invocations(&self) -> u64 {
		self.keys.datagram.lock().expect("track key").invocations()
	}
}

impl fmt::Debug for Producer {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("track::Producer")
			.field("name", &self.name())
			.field("next", &*self.keys.next.lock().expect("track sequence"))
			.finish()
	}
}

/// The sequence to allocate, `next` unless one is given, refusing one already allocated or
/// one that cannot be represented.
fn reserve(next: u64, sequence: Option<u64>) -> Result<u64> {
	let sequence = sequence.unwrap_or(next);
	if sequence < next {
		return Err(Error::Reuse);
	}
	check_u53(sequence)?;
	Ok(sequence)
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
	keys: Keys,
	semantic: String,
}

impl Request {
	pub(crate) fn new(
		inner: moq_net::track::Request,
		generation: crate::generation::Producer,
		keys: Keys,
		semantic: String,
	) -> Self {
		Self {
			inner,
			generation,
			keys,
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
		let keys = self.generation.adopt(&self.semantic, self.keys);
		Producer::new(self.inner.accept(info), keys)
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

/// Subscriber for one physical track.
///
/// Grouped authentication failure ends the track and wakes pending reads with
/// [`Error::Authentication`]. A datagram that fails to open is dropped as an
/// [`Event::Identity`], [`Event::Oversize`], or [`Event::Authentication`] until that key is exhausted.
pub struct Consumer {
	name: String,
	// Shared with every other subscription to this name under the generation.
	keys: Keys,
	window: DatagramWindow,
	terminal: Arc<Terminal>,
}

impl Consumer {
	pub(crate) fn new(inner: moq_net::track::Subscriber, keys: Keys) -> Self {
		let name = inner.name().to_string();
		Self {
			name,
			keys,
			window: DatagramWindow::default(),
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
	/// [`Error::Authentication`] once a grouped frame has failed to open, or a net error.
	pub fn poll_recv_group(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<group::Consumer>>> {
		let terminal = Arc::clone(&self.terminal);
		let group_key = Arc::clone(&self.keys.group);
		terminal.poll_subscription(waiter, |inner, waiter| {
			let Some(inner) = ready!(inner.poll_recv_group(waiter)?) else {
				return Poll::Ready(Ok(None));
			};
			Poll::Ready(Ok(Some(group::Consumer::new(
				inner,
				Arc::clone(&group_key),
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

	/// Poll for the next datagram event.
	///
	/// A datagram that fails to open (a sequence out of range, a bad size, or a forged tag) and
	/// a duplicate are events, not track-ending errors.
	///
	/// # Errors
	///
	/// [`Error::Authentication`] only if a grouped frame already failed,
	/// [`Error::Exhausted`] from opening, or a net error.
	pub fn poll_recv_datagram(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Event>>> {
		let terminal = Arc::clone(&self.terminal);
		terminal.poll_subscription(waiter, |inner, waiter| self.open_datagram(inner, waiter))
	}

	/// Open one datagram. A bad datagram stays an event; exhausting the datagram key does not.
	fn open_datagram(
		&mut self,
		inner: &mut moq_net::track::Subscriber,
		waiter: &kio::Waiter,
	) -> Poll<Result<Option<Event>>> {
		let Some(datagram) = ready!(inner.poll_recv_datagram(waiter)?) else {
			return Poll::Ready(Ok(None));
		};
		let sequence = datagram.sequence;
		if self.window.is_duplicate(sequence) {
			return Poll::Ready(Ok(Some(Event::Duplicate { sequence })));
		}
		let opened =
			self.keys
				.datagram
				.lock()
				.expect("track key")
				.open(sequence, 0, &datagram.payload, MAX_DATAGRAM_PAYLOAD);
		match opened {
			Ok(plaintext) => {
				self.window.mark(sequence);
				Poll::Ready(Ok(Some(Event::Datagram(Datagram {
					sequence,
					timestamp: datagram.timestamp,
					plaintext,
				}))))
			}
			// A bad datagram is not grouped failure: the track, its demand, and any
			// parked grouped read stay up. `exhausted` is this key's own limit.
			Err(Error::Authentication) => Poll::Ready(Ok(Some(Event::Authentication { sequence }))),
			Err(Error::Identity) => Poll::Ready(Ok(Some(Event::Identity { sequence }))),
			Err(Error::Oversize) => Poll::Ready(Ok(Some(Event::Oversize { sequence }))),
			Err(err) => Poll::Ready(Err(err)),
		}
	}

	/// Receive the next datagram event.
	///
	/// # Errors
	///
	/// Same as [`Self::poll_recv_datagram`].
	pub async fn recv_datagram(&mut self) -> Result<Option<Event>> {
		kio::wait(|waiter| self.poll_recv_datagram(waiter)).await
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
