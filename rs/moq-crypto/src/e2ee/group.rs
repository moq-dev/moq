//! Grouped-frame producer and consumer for one group identity.

use std::fmt;
use std::sync::{Arc, Mutex};
use std::task::{Poll, ready};

use bytes::Bytes;

use crate::e2ee::error::{Error, Result};
use crate::e2ee::key::TrackKey;
use crate::e2ee::limits::MAX_GROUPED_PAYLOAD;
use crate::e2ee::terminal::Terminal;

/// A decrypted grouped frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
	/// Presentation timestamp, or `None` when untimed.
	pub timestamp: moq_net::Timestamp,
	/// Decrypted application bytes.
	pub plaintext: Bytes,
}

/// Exclusive writer for one group; frames are numbered from 0 in write order.
///
/// Dropping without [`Self::close`] or [`Self::abort`] lets the net group close uncleanly.
pub struct Producer {
	// The error that aborted the group once a write failed after encryption.
	inner: std::result::Result<moq_net::group::Producer, Error>,
	// The parent track's info, which rescales each timestamp before it is encrypted.
	track: moq_net::track::Info,
	start: u64,
	key: Arc<Mutex<TrackKey>>,
	next_frame: u32,
}

impl Producer {
	pub(crate) fn new(inner: moq_net::group::Producer, track: moq_net::track::Info, key: Arc<Mutex<TrackKey>>) -> Self {
		Self {
			start: inner.start,
			inner: Ok(inner),
			track,
			key,
			next_frame: 0,
		}
	}

	/// Where the group starts, in the track's timescale: its key on the track.
	pub fn start(&self) -> u64 {
		self.start
	}

	/// Next frame index that will be written.
	pub fn next_frame(&self) -> u32 {
		self.next_frame
	}

	/// Encrypt `plaintext` at the next frame index and write the ciphertext.
	///
	/// The identity is spent before the net write, so a failed write never repeats a
	/// nonce with different bytes. Predictable failures, an oversize plaintext or a
	/// timestamp the track cannot represent, are refused before encryption. Once encrypted,
	/// a failed net write aborts the group, since its transport index could no longer match
	/// the nonce; every later call returns that error.
	///
	/// # Errors
	///
	/// [`Error::Identity`] if the next frame exceeds 32 bits, [`Error::Exhausted`],
	/// [`Error::Oversize`], or a net write error.
	pub fn write_frame(&mut self, timestamp: moq_net::Timestamp, plaintext: &[u8]) -> Result<()> {
		let inner = self.inner.as_mut().map_err(|err| err.clone())?;
		let frame = self.next_frame;
		if frame == u32::MAX {
			return Err(Error::Identity);
		}
		// Checked before the frame identity is spent.
		let timestamp = self.track.rescale_in(timestamp, self.start)?;
		let payload = self.key.lock().expect("track key").protect(
			self.start,
			u64::from(frame),
			plaintext,
			MAX_GROUPED_PAYLOAD,
		)?;
		self.next_frame = frame + 1;
		if let Err(err) = inner.write_frame(timestamp, payload) {
			if let Ok(inner) = std::mem::replace(&mut self.inner, Err(Error::Net(err.clone()))) {
				// The net group may already have ended itself, as it does when it grows too large.
				let _ = inner.abort(err.clone());
			}
			return Err(Error::Net(err));
		}
		Ok(())
	}

	/// Close the group without a known end. No more frames will be written.
	///
	/// # Errors
	///
	/// A net error if the group is already closed, or the error a failed write aborted it with.
	pub fn close(self) -> Result<()> {
		self.inner?.close()?;
		Ok(())
	}

	/// Abort the group with a cancel, consuming the handle.
	///
	/// # Errors
	///
	/// A net error if the group is already closed, or the error a failed write aborted it with.
	pub fn abort(self) -> Result<()> {
		self.inner?.abort(moq_net::Error::Cancel)?;
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
			.field("start", &self.start())
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
	start: u64,
	key: Arc<Mutex<TrackKey>>,
	terminal: Arc<Terminal>,
}

impl Consumer {
	pub(crate) fn new(inner: moq_net::group::Consumer, key: Arc<Mutex<TrackKey>>, terminal: Arc<Terminal>) -> Self {
		let start = inner.start;
		Self {
			inner: Some(inner),
			start,
			key,
			terminal,
		}
	}

	/// Drop the net reader so this group no longer counts as demanded.
	fn release(&mut self) {
		self.inner.take();
	}

	/// Where the group starts, in the track's timescale: its key on the track.
	pub fn start(&self) -> u64 {
		self.start
	}

	/// The next decrypted frame, or `None` once the group finished.
	///
	/// # Errors
	///
	/// [`Error::Authentication`] ends this track. [`Error::Oversize`] or
	/// [`Error::Exhausted`] as for open, and net errors from the underlying group.
	#[cfg_attr(feature = "async", kio::wrapper)]
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
					.open(self.start, index, &frame.payload, MAX_GROUPED_PAYLOAD)
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
}

impl fmt::Debug for Consumer {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("group::Consumer")
			.field("start", &self.start())
			.field("index", &self.inner.as_ref().map(|inner| inner.index()))
			.finish()
	}
}
