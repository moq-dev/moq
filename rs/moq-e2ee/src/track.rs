//! Exclusive grouped-frame and datagram writers and readers for one physical track.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Poll, ready};

use bytes::Bytes;

use crate::credential::{Credential, Domain, PhysicalName, Pin};
use crate::datagram::{self, Event};
use crate::error::{Error, Result};
use crate::group;
use crate::key::TrackKey;
use crate::limits::{MAX_DATAGRAM_BODY, MIN_DATAGRAM_HEADER, TAG_LEN, check_u53, datagram_payload_limit};
use crate::window::{DatagramWindow, GroupWindow};

/// Exclusive producer for one physical track.
///
/// Owns grouped-frame and datagram key domains and the shared sequence namespace.
/// Not `Clone`. Sequences are allocated monotonically; reuse and exhaustion are
/// refused before encryption. Datagram ciphertext is retained for retransmission,
/// bounded to the last [`crate::DATAGRAM_DUPLICATE_WINDOW`] sequences.
pub struct Producer {
	inner: moq_net::track::Producer,
	group_key: Arc<Mutex<TrackKey>>,
	datagram_key: Arc<Mutex<TrackKey>>,
	next: u64,
	datagrams: HashMap<u64, RetainedDatagram>,
	subscribe: u64,
}

struct RetainedDatagram {
	timestamp: moq_net::Timestamp,
	payload: Bytes,
}

impl Producer {
	pub(crate) fn new(
		inner: moq_net::track::Producer,
		credential: &Credential,
		physical: PhysicalName,
	) -> Result<Self> {
		Ok(Self {
			inner,
			group_key: Arc::new(Mutex::new(TrackKey::derive(credential, &physical, Domain::Group)?)),
			datagram_key: Arc::new(Mutex::new(TrackKey::derive(credential, &physical, Domain::Datagram)?)),
			next: 0,
			datagrams: HashMap::new(),
			subscribe: 0,
		})
	}

	/// Subscribe ID used to size datagram plaintext against the moq-lite header.
	///
	/// Defaults to 0. The actual subscribe ID is assigned by the session; set this
	/// to the value that will be encoded, or leave 0 for a one-byte varint.
	pub fn set_subscribe(&mut self, subscribe: u64) {
		self.subscribe = subscribe;
	}

	/// The underlying net track name, which is the physical name.
	pub fn name(&self) -> &str {
		self.inner.name()
	}

	/// Allocate the next sequence and start a grouped-frame group there.
	///
	/// # Errors
	///
	/// [`Error::Identity`] if the next sequence exceeds `2^53-1`, or a net error.
	pub fn append_group(&mut self) -> Result<group::Producer> {
		let sequence = self.allocate(None)?;
		self.create_group_at(sequence)
	}

	/// Start a grouped-frame group at an explicit sequence.
	///
	/// # Errors
	///
	/// [`Error::Reuse`] if `sequence` was already allocated, [`Error::Identity`] if
	/// it exceeds `2^53-1`, or a net error.
	pub fn create_group(&mut self, sequence: u64) -> Result<group::Producer> {
		let sequence = self.allocate(Some(sequence))?;
		self.create_group_at(sequence)
	}

	fn create_group_at(&mut self, sequence: u64) -> Result<group::Producer> {
		let inner = self.inner.create_group(moq_net::group::Info { sequence })?;
		Ok(group::Producer::new(inner, self.group_key.clone()))
	}

	/// Encrypt `plaintext` at the next sequence and insert the datagram.
	///
	/// # Errors
	///
	/// [`Error::Reuse`], [`Error::Identity`], [`Error::Exhausted`], [`Error::Oversize`],
	/// or a net write error.
	pub fn append_datagram(&mut self, timestamp: moq_net::Timestamp, plaintext: impl AsRef<[u8]>) -> Result<u64> {
		let sequence = self.next;
		self.insert_datagram(sequence, timestamp, plaintext)?;
		Ok(sequence)
	}

	/// Encrypt `plaintext` at `sequence` and insert the datagram.
	///
	/// The identity is committed before encryption. Retransmission must use
	/// [`Self::retransmit_datagram`].
	///
	/// # Errors
	///
	/// [`Error::Reuse`] if `sequence` was already allocated, [`Error::Identity`],
	/// [`Error::Exhausted`], [`Error::Oversize`], or a net write error.
	pub fn insert_datagram(
		&mut self,
		sequence: u64,
		timestamp: moq_net::Timestamp,
		plaintext: impl AsRef<[u8]>,
	) -> Result<()> {
		let sequence = self.allocate(Some(sequence))?;
		let plaintext = plaintext.as_ref();
		let limit = datagram_payload_limit(self.subscribe, sequence, timestamp.value())?;
		let payload = self
			.datagram_key
			.lock()
			.expect("datagram key")
			.protect(sequence, 0, plaintext, limit)?;
		self.datagrams.insert(
			sequence,
			RetainedDatagram {
				timestamp,
				payload: payload.clone(),
			},
		);
		// Bound retention to the duplicate window so a long-lived datagram track
		// does not retain every ciphertext for the life of the Producer.
		let cutoff = self.next.saturating_sub(crate::DATAGRAM_DUPLICATE_WINDOW as u64);
		self.datagrams.retain(|&seq, _| seq >= cutoff);
		datagram::insert_ciphertext(&mut self.inner, sequence, timestamp, payload)?;
		Ok(())
	}

	/// Write the retained ciphertext for `sequence` again without encrypting.
	///
	/// # Errors
	///
	/// [`Error::Reuse`] if that identity was never produced or was evicted outside
	/// the retention window, or a net write error.
	pub fn retransmit_datagram(&mut self, sequence: u64) -> Result<()> {
		let retained = self.datagrams.get(&sequence).ok_or(Error::Reuse)?;
		datagram::insert_ciphertext(&mut self.inner, sequence, retained.timestamp, retained.payload.clone())?;
		Ok(())
	}

	/// Ciphertext already produced for datagram `sequence`, if retained.
	pub fn datagram_ciphertext(&self, sequence: u64) -> Option<&Bytes> {
		self.datagrams.get(&sequence).map(|d| &d.payload)
	}

	#[cfg(test)]
	pub(crate) fn datagram_invocations(&self) -> u64 {
		self.datagram_key.lock().expect("datagram key").invocations()
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

	fn allocate(&mut self, requested: Option<u64>) -> Result<u64> {
		let sequence = requested.unwrap_or(self.next);
		if sequence < self.next {
			return Err(Error::Reuse);
		}
		check_u53(sequence)?;
		self.next = sequence.checked_add(1).ok_or(Error::Identity)?;
		check_u53(self.next.saturating_sub(1))?;
		Ok(sequence)
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
	datagram_key: Arc<Mutex<TrackKey>>,
	groups: Arc<Mutex<GroupWindow>>,
	datagrams: DatagramWindow,
	auth_failed: Arc<AtomicBool>,
}

impl Consumer {
	/// Wrap a net subscriber. The track name is the physical name.
	///
	/// # Errors
	///
	/// [`Error::PinnedMismatch`] if `pin` does not match the credential,
	/// [`Error::Identity`] if the track name is not a physical name.
	pub fn new(credential: &Credential, track: moq_net::track::Subscriber, pin: Option<&Pin>) -> Result<Self> {
		if let Some(pin) = pin {
			credential.check_pin(pin)?;
		}
		let physical = PhysicalName::parse(track.name())?;
		Ok(Self {
			inner: track,
			group_key: Arc::new(Mutex::new(TrackKey::derive(credential, &physical, Domain::Group)?)),
			datagram_key: Arc::new(Mutex::new(TrackKey::derive(credential, &physical, Domain::Datagram)?)),
			groups: Arc::new(Mutex::new(GroupWindow::default())),
			datagrams: DatagramWindow::default(),
			auth_failed: Arc::new(AtomicBool::new(false)),
		})
	}

	/// Poll for the next protected group in arrival order.
	///
	/// # Errors
	///
	/// [`Error::Authentication`] once a grouped frame has failed open. Net errors
	/// from the underlying subscriber.
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
			self.groups.clone(),
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
	/// Authentication failure and duplicates are events, not track-ending errors.
	///
	/// # Errors
	///
	/// [`Error::Authentication`] only if a grouped frame already failed. Net errors
	/// from the underlying subscriber. [`Error::Oversize`] / [`Error::Exhausted`]
	/// from opening.
	pub fn poll_recv_datagram(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Event>>> {
		if self.auth_failed.load(Ordering::Acquire) {
			return Poll::Ready(Err(Error::Authentication));
		}
		let Some(datagram) = ready!(self.inner.poll_recv_datagram(waiter)?) else {
			return Poll::Ready(Ok(None));
		};
		let sequence = datagram.sequence;
		if self.datagrams.is_duplicate(sequence) {
			return Poll::Ready(Ok(Some(Event::Duplicate { sequence })));
		}
		let limit = MAX_DATAGRAM_BODY.saturating_sub(MIN_DATAGRAM_HEADER);
		if datagram.payload.len() < TAG_LEN || datagram.payload.len() > limit {
			return Poll::Ready(Err(Error::Oversize));
		}
		match self
			.datagram_key
			.lock()
			.expect("datagram key")
			.open(sequence, 0, &datagram.payload, limit)
		{
			Ok(plaintext) => {
				// Mark only after a successful open so a forged datagram that fails
				// AEAD does not burn the identity; the real retransmission still opens.
				self.datagrams.mark(sequence);
				Poll::Ready(Ok(Some(Event::Datagram(datagram::Datagram {
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
			.field("auth_failed", &self.auth_failed.load(Ordering::Acquire))
			.finish()
	}
}
