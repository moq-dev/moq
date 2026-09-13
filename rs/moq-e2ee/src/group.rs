//! Grouped-frame producer and consumer for one group identity.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Poll, ready};

use bytes::Bytes;

use crate::error::{Error, Result};
use crate::key::TrackKey;
use crate::limits::MAX_GROUPED_PAYLOAD;
use crate::window::GroupWindow;

/// A decrypted grouped frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
	/// Presentation timestamp.
	pub timestamp: moq_net::Timestamp,
	/// Decrypted application bytes.
	pub plaintext: Bytes,
}

/// Exclusive writer for one grouped-frame identity stream.
///
/// Frames are numbered from 0 in write order. Dropping without [`Self::finish`]
/// or [`Self::abort`] lets the net group close uncleanly.
pub struct Producer {
	inner: moq_net::group::Producer,
	key: Arc<Mutex<TrackKey>>,
	ciphertexts: Vec<Bytes>,
}

impl Producer {
	pub(crate) fn new(inner: moq_net::group::Producer, key: Arc<Mutex<TrackKey>>) -> Self {
		Self {
			inner,
			key,
			ciphertexts: Vec::new(),
		}
	}

	/// The group's sequence number.
	pub fn sequence(&self) -> u64 {
		self.inner.sequence
	}

	/// Next frame index that will be written.
	pub fn next_frame(&self) -> u32 {
		u32::try_from(self.ciphertexts.len()).unwrap_or(u32::MAX)
	}

	/// Ciphertext already produced for `frame`, if it is still retained.
	pub fn ciphertext(&self, frame: u32) -> Option<&Bytes> {
		self.ciphertexts.get(frame as usize)
	}

	/// Encrypt `plaintext` at the next frame index and write the ciphertext.
	///
	/// # Errors
	///
	/// [`Error::Identity`] if the next frame exceeds 32 bits, [`Error::Exhausted`],
	/// [`Error::Oversize`], or a net write error.
	pub fn write_frame(&mut self, timestamp: moq_net::Timestamp, plaintext: impl AsRef<[u8]>) -> Result<()> {
		let frame = u32::try_from(self.ciphertexts.len()).map_err(|_| Error::Identity)?;
		let group = self.inner.sequence;
		let plaintext = plaintext.as_ref();
		let payload =
			self.key
				.lock()
				.expect("track key")
				.protect(group, u64::from(frame), plaintext, MAX_GROUPED_PAYLOAD)?;
		self.inner.write_frame(timestamp, payload.clone())?;
		self.ciphertexts.push(payload);
		Ok(())
	}

	/// Finish the group. No more frames will be written.
	///
	/// # Errors
	///
	/// A net error if the group is already closed.
	pub fn finish(mut self) -> Result<()> {
		self.inner.finish()?;
		Ok(())
	}

	/// Abort the group with a cancel, consuming the handle.
	///
	/// # Errors
	///
	/// A net error if the group is already closed.
	pub fn abort(self) -> Result<()> {
		self.inner.abort(moq_net::Error::Cancel)?;
		Ok(())
	}
}

impl fmt::Debug for Producer {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("group::Producer")
			.field("sequence", &self.sequence())
			.field("next_frame", &self.next_frame())
			.finish()
	}
}

/// Reader for one protected group.
///
/// Authentication failure is sticky and ends the grouped track.
pub struct Consumer {
	inner: moq_net::group::Consumer,
	key: Arc<Mutex<TrackKey>>,
	window: Arc<Mutex<GroupWindow>>,
	auth_failed: Arc<AtomicBool>,
	next_frame: u32,
}

impl Consumer {
	pub(crate) fn new(
		inner: moq_net::group::Consumer,
		key: Arc<Mutex<TrackKey>>,
		window: Arc<Mutex<GroupWindow>>,
		auth_failed: Arc<AtomicBool>,
	) -> Self {
		Self {
			inner,
			key,
			window,
			auth_failed,
			next_frame: 0,
		}
	}

	/// The group's sequence number.
	pub fn sequence(&self) -> u64 {
		self.inner.sequence
	}

	/// Read the next decrypted frame, without blocking.
	///
	/// # Errors
	///
	/// [`Error::Authentication`] ends this track. [`Error::Duplicate`] if the identity
	/// is still in the window. [`Error::Oversize`] or [`Error::Exhausted`] as for open.
	pub fn poll_read_frame(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Frame>>> {
		if self.auth_failed.load(Ordering::Acquire) {
			return Poll::Ready(Err(Error::Authentication));
		}
		let Some(frame) = ready!(self.inner.poll_read_frame(waiter)?) else {
			return Poll::Ready(Ok(None));
		};
		let group = self.inner.sequence;
		let index = self.next_frame;
		if let Err(err) = self.window.lock().expect("group window").check(group, index) {
			if matches!(err, Error::Authentication) {
				self.fail_auth();
			}
			return Poll::Ready(Err(err));
		}
		let plaintext =
			match self
				.key
				.lock()
				.expect("track key")
				.open(group, u64::from(index), &frame.payload, MAX_GROUPED_PAYLOAD)
			{
				Ok(plaintext) => plaintext,
				Err(Error::Authentication) => {
					self.fail_auth();
					return Poll::Ready(Err(Error::Authentication));
				}
				Err(err) => return Poll::Ready(Err(err)),
			};
		self.next_frame = match self.next_frame.checked_add(1) {
			Some(next) => next,
			None => {
				self.fail_auth();
				return Poll::Ready(Err(Error::Identity));
			}
		};
		Poll::Ready(Ok(Some(Frame {
			timestamp: frame.timestamp,
			plaintext,
		})))
	}

	/// Read the next decrypted frame.
	///
	/// # Errors
	///
	/// Same as [`Self::poll_read_frame`].
	pub async fn read_frame(&mut self) -> Result<Option<Frame>> {
		kio::wait(|waiter| self.poll_read_frame(waiter)).await
	}

	fn fail_auth(&self) {
		self.auth_failed.store(true, Ordering::Release);
	}
}

impl fmt::Debug for Consumer {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("group::Consumer")
			.field("sequence", &self.sequence())
			.field("next_frame", &self.next_frame)
			.finish()
	}
}
