//! Grouped-frame and datagram writers and readers for one physical track.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Poll, ready};

use crate::datagram::{Datagram, Event};
use crate::error::{Error, Result};
use crate::group;
use crate::key::TrackKey;
use crate::limits::{MAX_DATAGRAM_PAYLOAD, check_u53};
use crate::window::DatagramWindow;

/// Exclusive producer for one physical track.
///
/// Owns the grouped-frame and datagram keys and the shared sequence namespace.
/// Sequences are allocated monotonically; reuse and exhaustion are refused before encryption.
pub struct Producer {
	inner: moq_net::track::Producer,
	group_key: Arc<Mutex<TrackKey>>,
	datagram_key: TrackKey,
	next: u64,
}

impl Producer {
	pub(crate) fn new(inner: moq_net::track::Producer, group_key: TrackKey, datagram_key: TrackKey) -> Self {
		Self {
			inner,
			group_key: Arc::new(Mutex::new(group_key)),
			datagram_key,
			next: 0,
		}
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
		let sequence = self.next;
		self.create_group(sequence)
	}

	/// Start a group at an explicit sequence, which must be at or above the next unallocated one.
	///
	/// # Errors
	///
	/// [`Error::Reuse`] if `sequence` was already allocated, [`Error::Identity`] if
	/// it exceeds `2^53-1`, or a net error.
	pub fn create_group(&mut self, sequence: u64) -> Result<group::Producer> {
		self.allocate(sequence)?;
		let inner = self.inner.create_group(moq_net::group::Info { sequence })?;
		Ok(group::Producer::new(inner, self.group_key.clone()))
	}

	/// Encrypt `plaintext` at the next sequence and insert the datagram, returning that sequence.
	///
	/// # Errors
	///
	/// [`Error::Identity`], [`Error::Exhausted`], [`Error::Oversize`], or a net write error.
	pub fn append_datagram(&mut self, timestamp: moq_net::Timestamp, plaintext: &[u8]) -> Result<u64> {
		let sequence = self.next;
		self.insert_datagram(sequence, timestamp, plaintext)?;
		Ok(sequence)
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
	pub fn insert_datagram(&mut self, sequence: u64, timestamp: moq_net::Timestamp, plaintext: &[u8]) -> Result<()> {
		self.reserve(sequence)?;
		let payload = self
			.datagram_key
			.protect(sequence, 0, plaintext, MAX_DATAGRAM_PAYLOAD)?;
		self.next = sequence + 1;
		self.inner.insert_datagram(sequence, timestamp, payload)?;
		Ok(())
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

	/// Refuse a sequence this track already allocated or cannot represent.
	fn reserve(&self, sequence: u64) -> Result<()> {
		if sequence < self.next {
			return Err(Error::Reuse);
		}
		check_u53(sequence)
	}

	fn allocate(&mut self, sequence: u64) -> Result<()> {
		self.reserve(sequence)?;
		self.next = sequence + 1;
		Ok(())
	}

	#[cfg(test)]
	pub(crate) fn datagram_invocations(&self) -> u64 {
		self.datagram_key.invocations()
	}
}

impl fmt::Debug for Producer {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("track::Producer")
			.field("name", &self.name())
			.field("next", &self.next)
			.finish()
	}
}

/// Subscriber for one physical track.
///
/// Grouped authentication failure is sticky and ends the track. A bad datagram
/// is dropped with [`Event::Authentication`] and the track continues.
pub struct Consumer {
	inner: moq_net::track::Subscriber,
	group_key: Arc<Mutex<TrackKey>>,
	datagram_key: TrackKey,
	window: DatagramWindow,
	auth_failed: Arc<AtomicBool>,
}

impl Consumer {
	pub(crate) fn new(inner: moq_net::track::Subscriber, group_key: TrackKey, datagram_key: TrackKey) -> Self {
		Self {
			inner,
			group_key: Arc::new(Mutex::new(group_key)),
			datagram_key,
			window: DatagramWindow::default(),
			auth_failed: Arc::new(AtomicBool::new(false)),
		}
	}

	/// The physical track name.
	pub fn name(&self) -> &str {
		self.inner.name()
	}

	/// Poll for the next protected group in arrival order.
	///
	/// # Errors
	///
	/// [`Error::Authentication`] once a grouped frame has failed to open, or a net error.
	pub fn poll_recv_group(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<group::Consumer>>> {
		if self.auth_failed.load(Ordering::Acquire) {
			return Poll::Ready(Err(Error::Authentication));
		}
		let Some(inner) = ready!(self.inner.poll_recv_group(waiter)?) else {
			return Poll::Ready(Ok(None));
		};
		Poll::Ready(Ok(Some(group::Consumer::new(
			inner,
			self.group_key.clone(),
			self.auth_failed.clone(),
		))))
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
	/// A bad tag and a duplicate are events, not track-ending errors.
	///
	/// # Errors
	///
	/// [`Error::Authentication`] only if a grouped frame already failed,
	/// [`Error::Oversize`] or [`Error::Exhausted`] from opening, or a net error.
	pub fn poll_recv_datagram(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Event>>> {
		if self.auth_failed.load(Ordering::Acquire) {
			return Poll::Ready(Err(Error::Authentication));
		}
		let Some(datagram) = ready!(self.inner.poll_recv_datagram(waiter)?) else {
			return Poll::Ready(Ok(None));
		};
		let sequence = datagram.sequence;
		if self.window.is_duplicate(sequence) {
			return Poll::Ready(Ok(Some(Event::Duplicate { sequence })));
		}
		match self
			.datagram_key
			.open(sequence, 0, &datagram.payload, MAX_DATAGRAM_PAYLOAD)
		{
			Ok(plaintext) => {
				self.window.mark(sequence);
				Poll::Ready(Ok(Some(Event::Datagram(Datagram {
					sequence,
					timestamp: datagram.timestamp,
					plaintext,
				}))))
			}
			Err(Error::Authentication) => Poll::Ready(Ok(Some(Event::Authentication { sequence }))),
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

impl fmt::Debug for Consumer {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("track::Consumer")
			.field("name", &self.name())
			.field("auth_failed", &self.auth_failed.load(Ordering::Acquire))
			.finish()
	}
}
