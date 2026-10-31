//! Signing writer and verifying reader for one group.

use std::fmt;
use std::sync::Arc;
use std::task::{Poll, ready};

use bytes::Bytes;

use crate::error::{Error, Result};
use crate::key::{PublicKey, SecretKey};
use crate::scope::Scope;

/// A grouped frame whose signature verified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
	/// Presentation timestamp at the track's timescale, or `None` when untimed. Signed.
	pub timestamp: Option<moq_net::Timestamp>,
	/// The application bytes, without the signature.
	pub payload: Bytes,
}

/// Exclusive writer for one group, signing each frame at the index it lands on.
pub struct Producer {
	inner: moq_net::group::Producer,
	key: SecretKey,
	scope: Arc<Scope>,
}

impl Producer {
	pub(crate) fn new(inner: moq_net::group::Producer, key: SecretKey, scope: Arc<Scope>) -> Self {
		Self { inner, key, scope }
	}

	/// The group's sequence number.
	pub fn sequence(&self) -> u64 {
		self.inner.sequence
	}

	/// Sign `payload` at the next frame index and write it with the signature appended.
	///
	/// # Errors
	///
	/// A net error if the group is closed, the timestamp does not match the track, or the
	/// signed frame exceeds the frame size limit.
	pub fn write_frame(&mut self, timestamp: impl Into<Option<moq_net::Timestamp>>, payload: &[u8]) -> Result<()> {
		// Signed at the track's timescale, the value the net group stores and every hop carries.
		// A frame whose timedness disagrees with the track is left for the write to refuse.
		let timestamp = match (timestamp.into(), self.inner.timescale()) {
			(Some(at), Some(timescale)) => Some(at.convert(timescale).map_err(|_| moq_net::Error::TimestampMismatch)?),
			(timestamp, _) => timestamp,
		};
		let frame = self.inner.frame_count() as u64;
		let body = self
			.scope
			.sign(&self.key, self.inner.sequence, frame, timestamp, payload);
		self.inner.write_frame(timestamp, body)?;
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

	/// Abort the group with `err`, consuming the handle.
	///
	/// # Errors
	///
	/// A net error if the group is already closed.
	pub fn abort(self, err: moq_net::Error) -> Result<()> {
		self.inner.abort(err)?;
		Ok(())
	}
}

impl fmt::Debug for Producer {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("group::Producer")
			.field("sequence", &self.sequence())
			.field("frames", &self.inner.frame_count())
			.finish()
	}
}

/// Reader for one signed group.
///
/// A frame that fails to verify ends the group with [`Error::Signature`]: none of its bytes
/// reach the caller, and the underlying reader is released. The track keeps delivering groups.
pub struct Consumer {
	// `None` once a frame failed to verify.
	inner: Option<moq_net::group::Consumer>,
	sequence: u64,
	key: PublicKey,
	scope: Arc<Scope>,
}

impl Consumer {
	pub(crate) fn new(inner: moq_net::group::Consumer, key: PublicKey, scope: Arc<Scope>) -> Self {
		Self {
			sequence: inner.sequence,
			inner: Some(inner),
			key,
			scope,
		}
	}

	/// The group's sequence number.
	pub fn sequence(&self) -> u64 {
		self.sequence
	}

	/// Read the next verified frame, without blocking.
	///
	/// # Errors
	///
	/// [`Error::Signature`] once a frame has failed to verify, or a net error from the group.
	pub fn poll_read_frame(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Frame>>> {
		let Some(inner) = self.inner.as_mut() else {
			return Poll::Ready(Err(Error::Signature));
		};
		let Some(frame) = ready!(inner.poll_read_frame(waiter)?) else {
			return Poll::Ready(Ok(None));
		};
		// The cursor already advanced past the frame, so a group resumed above frame 0 still
		// verifies at the index the publisher signed.
		let index = inner.index() - 1;
		match self
			.scope
			.verify(&self.key, self.sequence, index, frame.timestamp, frame.payload)
		{
			Ok(payload) => Poll::Ready(Ok(Some(Frame {
				timestamp: frame.timestamp,
				payload,
			}))),
			Err(err) => {
				self.inner = None;
				Poll::Ready(Err(err))
			}
		}
	}

	/// Read the next verified frame.
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
