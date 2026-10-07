//! Grouped-frame producer and consumer for one group identity.

use std::fmt;
use std::sync::{Arc, Mutex};
use std::task::{Poll, ready};

use bytes::Bytes;

use crate::error::{Error, Result};
use crate::key::TrackKey;
use crate::limits::MAX_GROUPED_PAYLOAD;
use crate::terminal::Terminal;

/// A decrypted grouped frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
	/// Presentation timestamp.
	pub timestamp: moq_net::Timestamp,
	/// Decrypted application bytes.
	pub plaintext: Bytes,
}

/// Exclusive writer for one group; frames are numbered from 0 in write order.
///
/// Dropping without [`Self::finish`] or [`Self::abort`] lets the net group close uncleanly.
pub struct Producer {
	inner: moq_net::group::Producer,
	key: Arc<Mutex<TrackKey>>,
	next_frame: u32,
}

impl Producer {
	pub(crate) fn new(inner: moq_net::group::Producer, key: Arc<Mutex<TrackKey>>) -> Self {
		Self {
			inner,
			key,
			next_frame: 0,
		}
	}

	/// The group's sequence number.
	pub fn sequence(&self) -> u64 {
		self.inner.sequence
	}

	/// Next frame index that will be written.
	pub fn next_frame(&self) -> u32 {
		self.next_frame
	}

	/// Encrypt `plaintext` at the next frame index and write the ciphertext.
	///
	/// The identity is spent before the net write, so a failed write never repeats a
	/// nonce with different bytes. Predictable failures, an oversize plaintext or a
	/// timestamp the track cannot represent, are refused before encryption.
	///
	/// # Errors
	///
	/// [`Error::Identity`] if the next frame exceeds 32 bits, [`Error::Exhausted`],
	/// [`Error::Oversize`], or a net write error.
	pub fn write_frame(&mut self, timestamp: moq_net::Timestamp, plaintext: &[u8]) -> Result<()> {
		let frame = self.next_frame;
		if frame == u32::MAX {
			return Err(Error::Identity);
		}
		timestamp
			.convert(self.inner.timescale())
			.map_err(|_| Error::Net(moq_net::Error::TimestampMismatch))?;
		let payload = self.key.lock().expect("track key").protect(
			self.inner.sequence,
			u64::from(frame),
			plaintext,
			MAX_GROUPED_PAYLOAD,
		)?;
		self.next_frame = frame + 1;
		self.inner.write_frame(timestamp, payload)?;
		Ok(())
	}

	/// Finish the group. No more frames will be written.
	///
	/// # Errors
	///
	/// A net error if the group is already closed.
	pub fn finish(self) -> Result<()> {
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

	#[cfg(test)]
	pub(crate) fn invocations(&self) -> u64 {
		self.key.lock().expect("track key").invocations()
	}
}

impl fmt::Debug for Producer {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("group::Producer")
			.field("sequence", &self.sequence())
			.field("next_frame", &self.next_frame)
			.finish()
	}
}

/// Reader for one protected group.
///
/// A grouped authentication failure ends the track: pending reads wake with
/// [`Error::Authentication`] and the underlying subscription is released.
pub struct Consumer {
	inner: Option<moq_net::group::Consumer>,
	sequence: u64,
	key: Arc<Mutex<TrackKey>>,
	terminal: Arc<Terminal>,
}

impl Consumer {
	pub(crate) fn new(inner: moq_net::group::Consumer, key: Arc<Mutex<TrackKey>>, terminal: Arc<Terminal>) -> Self {
		let sequence = inner.sequence;
		Self {
			inner: Some(inner),
			sequence,
			key,
			terminal,
		}
	}

	/// Drop the net reader so this group no longer counts as demanded.
	fn release(&mut self) {
		self.inner.take();
	}

	/// The group's sequence number.
	pub fn sequence(&self) -> u64 {
		self.sequence
	}

	/// Read the next decrypted frame, without blocking.
	///
	/// # Errors
	///
	/// [`Error::Authentication`] ends this track. [`Error::Oversize`] or
	/// [`Error::Exhausted`] as for open, and net errors from the underlying group.
	pub fn poll_read_frame(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Frame>>> {
		if self.terminal.poll_failed(waiter) {
			self.release();
			return Poll::Ready(Err(Error::Authentication));
		}
		let opened = {
			let Some(inner) = self.inner.as_mut() else {
				return Poll::Ready(Err(Error::Authentication));
			};
			let Some(frame) = ready!(inner.poll_read_frame(waiter)?) else {
				return Poll::Ready(Ok(None));
			};
			// The transport cursor already advanced past the frame just read; its index is
			// the nonce half, so a group resumed above frame 0 still authenticates.
			let result = inner.index().checked_sub(1).ok_or(Error::Identity).and_then(|index| {
				self.key
					.lock()
					.expect("track key")
					.open(self.sequence, index, &frame.payload, MAX_GROUPED_PAYLOAD)
			});
			(frame.timestamp, result)
		};
		match opened {
			(timestamp, Ok(plaintext)) => Poll::Ready(Ok(Some(Frame { timestamp, plaintext }))),
			(_, Err(err)) => {
				if matches!(err, Error::Authentication | Error::Identity) {
					self.terminal.fail();
					self.release();
				}
				Poll::Ready(Err(err))
			}
		}
	}

	/// Read the next decrypted frame.
	///
	/// # Errors
	///
	/// Same as [`Self::poll_read_frame`].
	pub async fn read_frame(&mut self) -> Result<Option<Frame>> {
		kio::wait(|waiter| self.poll_read_frame(waiter)).await
	}
}

impl fmt::Debug for Consumer {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("group::Consumer")
			.field("sequence", &self.sequence())
			.field("index", &self.inner.as_ref().map(|inner| inner.index()))
			.finish()
	}
}
