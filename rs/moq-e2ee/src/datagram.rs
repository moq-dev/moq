//! Protected datagram tracks: writers, readers, and the events a read yields.
//!
//! A datagram track is a `moq-net` [datagram track](moq_net::datagram) under the physical name of
//! its semantic name, independent of any stream track of that name. Each datagram is sealed under
//! the name's datagram key at its own sequence, with frame 0.

use std::fmt;
use std::sync::{Arc, Mutex};
use std::task::{Poll, ready};

use bytes::Bytes;

use crate::error::{Error, Result};
use crate::generation::Domain;
use crate::key::{Claim, TrackKey};
use crate::limits::MAX_DATAGRAM_PAYLOAD;
use crate::window::DatagramWindow;

/// A decrypted datagram.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Datagram {
	/// The datagram track's own sequence.
	pub sequence: u64,
	/// Presentation timestamp, or `None` when untimed.
	pub timestamp: Option<moq_net::Timestamp>,
	/// Decrypted application bytes.
	pub plaintext: Bytes,
}

/// Outcome of reading one protected datagram.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
	/// Authenticated plaintext.
	Datagram(Datagram),
	/// The datagram failed to authenticate; dropped, track continues.
	Authentication {
		/// Sequence of the rejected datagram.
		sequence: u64,
	},
	/// Sequence already opened inside the retained window; dropped, track continues.
	Duplicate {
		/// Sequence of the duplicate datagram.
		sequence: u64,
	},
	/// The sequence is above `2^53-1`; dropped, track continues.
	Identity {
		/// Sequence of the rejected datagram.
		sequence: u64,
	},
	/// The payload is shorter than the tag or longer than the datagram cap; dropped, track continues.
	Oversize {
		/// Sequence of the rejected datagram.
		sequence: u64,
	},
}

/// Exclusive producer for one physical datagram track.
///
/// Uses the datagram key and the datagram sequences of its semantic name, shared with every
/// datagram producer of that name under the generation and independent of its stream track.
/// Sequences are allocated monotonically, so a re-created or re-served name resumes above
/// every sequence allocated before; reuse and exhaustion are refused before encryption.
pub struct Producer {
	inner: moq_net::datagram::Producer,
	claim: Claim,
}

impl Producer {
	pub(crate) fn new(inner: moq_net::datagram::Producer, claim: Claim) -> Self {
		Self { inner, claim }
	}

	/// The physical track name.
	pub fn name(&self) -> &str {
		self.inner.name()
	}

	/// Encrypt `plaintext` at the next sequence and send it, returning that sequence.
	///
	/// # Errors
	///
	/// Same as [`Self::insert`], except [`Error::Reuse`].
	pub fn append(&mut self, timestamp: impl Into<Option<moq_net::Timestamp>>, plaintext: &[u8]) -> Result<u64> {
		self.write(None, timestamp.into(), plaintext)
	}

	/// Encrypt `plaintext` at an explicit sequence, which must be above every sequence this
	/// name's datagram tracks allocated under the generation, and send it.
	///
	/// Plaintext is capped at [`MAX_DATAGRAM_PLAINTEXT`](crate::MAX_DATAGRAM_PLAINTEXT).
	/// A predictable failure is refused before the sequence is allocated; once
	/// encrypted, the identity is spent even if the net write fails.
	///
	/// # Errors
	///
	/// [`Error::Reuse`] if `sequence` was already allocated, [`Error::Identity`],
	/// [`Error::Exhausted`], [`Error::Oversize`], or a net write error.
	pub fn insert(
		&mut self,
		sequence: u64,
		timestamp: impl Into<Option<moq_net::Timestamp>>,
		plaintext: &[u8],
	) -> Result<()> {
		self.write(Some(sequence), timestamp.into(), plaintext)?;
		Ok(())
	}

	fn write(&mut self, sequence: Option<u64>, timestamp: Option<moq_net::Timestamp>, plaintext: &[u8]) -> Result<u64> {
		let (sequence, payload) = self.claim.allocate(sequence, |sequence| {
			self.claim
				.key
				.lock()
				.expect("track key")
				.protect(sequence, 0, plaintext, MAX_DATAGRAM_PAYLOAD)
		})?;
		self.inner.insert(moq_net::Datagram {
			sequence,
			timestamp,
			payload,
		})?;
		Ok(sequence)
	}

	/// The highest priority among the track's subscribers, or `None` when it has none, so a
	/// publisher can skip encrypting datagrams nobody reads.
	pub fn priority(&self) -> Option<i8> {
		self.inner.priority()
	}

	/// Resolves to [`Self::priority`] once it differs from `last`.
	pub fn poll_priority(&self, waiter: &kio::Waiter, last: Option<i8>) -> Poll<Option<i8>> {
		self.inner.poll_priority(waiter, last)
	}

	/// Finish the track.
	pub fn finish(self) {
		self.inner.finish();
	}

	/// Abort the track, consuming the handle.
	pub fn abort(self) {
		self.inner.abort(moq_net::Error::Cancel);
	}

	#[cfg(test)]
	pub(crate) fn invocations(&self) -> u64 {
		self.claim.key.lock().expect("track key").invocations()
	}
}

impl fmt::Debug for Producer {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("datagram::Producer")
			.field("name", &self.name())
			.field("next", &*self.claim.next.lock().expect("claim sequence"))
			.finish()
	}
}

/// A datagram track a subscriber asked for that the handler has yet to serve.
///
/// Yielded by [`broadcast::DatagramRequests::poll_requested`](crate::broadcast::DatagramRequests::poll_requested)
/// under its semantic name. The name is claimed only once accepted. Dropping it without
/// [`accept`](Self::accept) rejects the waiting subscribers.
pub struct Request {
	inner: moq_net::datagram::Request,
	generation: crate::generation::Producer,
	// From `generation::Producer::peek`; claimed by `accept`.
	claim: Claim,
	semantic: String,
}

impl Request {
	pub(crate) fn new(
		inner: moq_net::datagram::Request,
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

	/// Serve the track, claiming its name and resolving every waiting subscriber.
	pub fn accept(self, info: moq_net::datagram::Info) -> Producer {
		let claim = self.generation.adopt(&self.semantic, Domain::Datagram, self.claim);
		Producer::new(self.inner.accept(info), claim)
	}

	/// Fail every waiting subscriber with `err`. This claims nothing, and releases no earlier claim.
	pub fn reject(self, err: moq_net::Error) {
		self.inner.reject(err);
	}
}

impl fmt::Debug for Request {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("datagram::Request").field("name", &self.name()).finish()
	}
}

/// Subscriber for one physical datagram track, live from when it was created.
///
/// A datagram that fails to open is dropped as an [`Event::Identity`], [`Event::Oversize`],
/// or [`Event::Authentication`], and one already opened as an [`Event::Duplicate`]; the track
/// continues until its key is exhausted.
pub struct Consumer {
	inner: moq_net::datagram::Consumer,
	// Shared with every other subscription to this name's datagrams under the generation.
	key: Arc<Mutex<TrackKey>>,
	window: DatagramWindow,
}

impl Consumer {
	pub(crate) fn new(inner: moq_net::datagram::Consumer, key: Arc<Mutex<TrackKey>>) -> Self {
		Self {
			inner,
			key,
			window: DatagramWindow::default(),
		}
	}

	/// The physical track name.
	pub fn name(&self) -> &str {
		self.inner.name()
	}

	/// Set this subscriber's priority, higher first.
	pub fn set_priority(&mut self, priority: i8) {
		self.inner.set_priority(priority);
	}

	/// Poll for the next datagram event, or `None` once the track finished.
	///
	/// # Errors
	///
	/// [`Error::Exhausted`] once the datagram key is spent, or a net error.
	pub fn poll_recv(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Event>>> {
		let Some(datagram) = ready!(self.inner.poll_recv(waiter)?) else {
			return Poll::Ready(Ok(None));
		};
		let sequence = datagram.sequence;
		if self.window.is_duplicate(sequence) {
			return Poll::Ready(Ok(Some(Event::Duplicate { sequence })));
		}
		let opened = self
			.key
			.lock()
			.expect("track key")
			.open(sequence, 0, &datagram.payload, MAX_DATAGRAM_PAYLOAD);
		let event = match opened {
			Ok(plaintext) => {
				self.window.mark(sequence);
				Event::Datagram(Datagram {
					sequence,
					timestamp: datagram.timestamp,
					plaintext,
				})
			}
			Err(Error::Authentication) => Event::Authentication { sequence },
			Err(Error::Identity) => Event::Identity { sequence },
			Err(Error::Oversize) => Event::Oversize { sequence },
			Err(err) => return Poll::Ready(Err(err)),
		};
		Poll::Ready(Ok(Some(event)))
	}

	/// Receive the next datagram event.
	///
	/// # Errors
	///
	/// Same as [`Self::poll_recv`].
	pub async fn recv(&mut self) -> Result<Option<Event>> {
		kio::wait(|waiter| self.poll_recv(waiter)).await
	}
}

impl fmt::Debug for Consumer {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("datagram::Consumer")
			.field("name", &self.name())
			.finish()
	}
}
