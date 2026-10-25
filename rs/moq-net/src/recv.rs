//! Receive-side ownership: a cancelled handler explicitly aborts its unfinished group.
//!
//! Frames need no guard here: `frame::Producer`'s own `Drop` already aborts the group when a
//! frame dies before its declared size is written, so a partial frame is never silent.

use std::ops::{Deref, DerefMut};

use crate::{Error, frame, group};

/// A received group that aborts if its handler is cancelled before finishing or handing it off.
pub(crate) struct Group {
	writer: Option<group::Producer>,
	pub sequence: u64,
	handle: group::Handle,
	armed: bool,
}

impl Group {
	/// Observe a group while its writer belongs to an in-flight frame.
	pub fn handle(&self) -> &group::Handle {
		&self.handle
	}

	/// Move the writer and cancellation cleanup into its next frame.
	pub fn create_object(&mut self, frame: frame::Header) -> Result<frame::Producer, Error> {
		self.writer
			.take()
			.expect("group writer is available between frames")
			.create_object(frame)
			.map(frame::Producer::cancel_on_drop)
	}

	/// Restore the writer returned by a completed frame.
	pub fn restore(&mut self, writer: group::Producer) {
		debug_assert!(self.writer.is_none());
		self.writer = Some(writer);
	}

	/// Own a group for the lifetime of its receive handler.
	pub fn new(producer: group::Producer) -> Self {
		Self {
			sequence: producer.sequence,
			handle: producer.handle(),
			writer: Some(producer),
			armed: true,
		}
	}

	/// Hand ownership to the next receive stage without closing the group.
	pub fn into_inner(mut self) -> group::Producer {
		self.armed = false;
		self.writer.take().expect("group owned until consumed")
	}

	/// Finish receiving without running cancellation cleanup.
	pub fn finish(mut self) -> Result<(), Error> {
		self.deref_mut().finish()?;
		self.writer.take();
		self.armed = false;
		Ok(())
	}

	/// Abort with the receive error instead of cancellation.
	pub fn abort(mut self, err: Error) -> Result<(), Error> {
		self.armed = false;
		self.handle.abort(err)
	}
}

impl Deref for Group {
	type Target = group::Producer;

	fn deref(&self) -> &Self::Target {
		self.writer.as_ref().expect("group owned until consumed")
	}
}

impl DerefMut for Group {
	fn deref_mut(&mut self) -> &mut Self::Target {
		self.writer.as_mut().expect("group owned until consumed")
	}
}

impl Drop for Group {
	fn drop(&mut self) {
		if self.armed {
			let _ = self.handle.abort(Error::Cancel);
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn receive_frame_cancellation_keeps_a_terminal() {
		let producer = group::Info { sequence: 0 }.produce();
		let mut consumer = producer.consume();
		let mut receiving = Group::new(producer);
		let frame = receiving
			.create_object(
				crate::frame::Info {
					size: 2,
					timestamp: Some(crate::Timestamp::ZERO),
				}
				.into(),
			)
			.unwrap();
		drop(frame);
		drop(receiving);
		assert!(matches!(
			consumer.poll_read_frame(&kio::Waiter::noop()),
			std::task::Poll::Ready(Err(Error::Cancel))
		));
	}
	#[test]
	fn receive_frame_cancellation_preserves_an_explicit_abort() {
		let producer = group::Info { sequence: 0 }.produce();
		let mut consumer = producer.consume();
		let mut receiving = Group::new(producer);
		let frame = receiving
			.create_object(
				crate::frame::Info {
					size: 2,
					timestamp: Some(crate::Timestamp::ZERO),
				}
				.into(),
			)
			.unwrap();
		receiving.abort(Error::App(42)).unwrap();
		drop(frame);
		assert!(matches!(
			consumer.poll_read_frame(&kio::Waiter::noop()),
			std::task::Poll::Ready(Err(Error::App(42)))
		));
	}
}
