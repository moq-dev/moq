//! Receive-side ownership: a cancelled handler explicitly aborts its unfinished group.
//!
//! Frames need no guard here: `frame::Producer`'s own `Drop` already aborts the group when a
//! frame dies before its declared size is written, so a partial frame is never silent.

use std::ops::{Deref, DerefMut};

use crate::{Error, frame, group};

/// A received group that aborts if its handler is cancelled before finishing or handing it off.
pub(crate) struct Group {
	writer: Option<group::Producer>,
	pub key: u64,
	handle: group::Handle,
	armed: bool,
}

impl Group {
	/// Observe a group while its writer belongs to an in-flight frame.
	pub fn handle(&self) -> &group::Handle {
		&self.handle
	}

	/// The index the next frame gets, counting one a frame still being written holds.
	pub fn frame_count(&self) -> usize {
		self.handle.frame_count()
	}

	/// Whether frames arrive as compressed slices, readable while a frame holds the writer.
	pub fn compressed(&self) -> bool {
		self.handle.compressed()
	}

	/// Whether this is a metadata copy, whose frames arrive as their headers alone.
	pub fn is_meta(&self) -> bool {
		self.handle.is_meta()
	}

	/// Write a metadata copy's next frame header, as [`group::Producer::write_header`].
	#[cfg(feature = "async")]
	pub fn write_header(&mut self, header: frame::Header) -> Result<(), Error> {
		self.writer
			.as_mut()
			.expect("group writer is available between frames")
			.write_header(header)
	}

	/// The relay state of the group, readable while a frame holds the writer.
	#[cfg(feature = "async")]
	pub fn wire(&self) -> Option<group::Wire> {
		self.handle.wire()
	}

	/// Set the relay state, as [`group::Handle::set_wire`], while a frame may hold the writer.
	#[cfg(feature = "async")]
	pub fn set_wire(&self, wire: group::Wire) -> Result<(), Error> {
		self.handle.set_wire(wire)
	}

	/// Mark a gap, as [`group::Handle::mark_gap`], while a frame may hold the writer.
	#[cfg(feature = "async")]
	pub fn mark_gap(&self, id: u64, gap: Option<u64>) -> Result<(), Error> {
		self.handle.mark_gap(id, gap)
	}

	/// Set the END_OF_GROUP object, as [`group::Handle::set_end_object`], while a frame may hold the writer.
	#[cfg(feature = "async")]
	pub fn set_end_object(&self, fields: crate::model::object::Fields) -> Result<(), Error> {
		self.handle.set_end_object(fields)
	}

	/// Move the writer and cancellation cleanup into its next frame.
	pub fn create_object(&mut self, frame: frame::Header) -> Result<frame::Producer, Error> {
		self.writer
			.take()
			.expect("group writer is available between frames")
			.create_object(frame)
			.map(frame::Producer::cancel_on_drop)
	}

	/// Poll until the writer may admit the frame `header` opens, its payload and metadata,
	/// within `window` bytes past its fastest reader, so admitting it sheds no byte that
	/// reader has yet to take; see [`group::Producer::poll_paced`].
	pub fn poll_admit(&self, waiter: &kio::Waiter, header: &frame::Header, window: u64) -> std::task::Poll<()> {
		let metadata = header.fields.as_ref().map_or(0, |fields| fields.heap());
		self.writer
			.as_ref()
			.expect("group writer is available between frames")
			.poll_paced(waiter, self.payload(header).saturating_add(metadata), window)
	}

	/// The payload bytes the frame `header` opens stores: none on a metadata copy, whatever
	/// size it declares.
	pub fn payload(&self, header: &frame::Header) -> u64 {
		match self.is_meta() {
			true => 0,
			false => header.info.size,
		}
	}

	/// How many more bytes the writer may take, a frame's included, before
	/// [`Self::poll_admit`] holds it at `window`, or `None` while nothing would.
	pub fn room(&self, window: u64) -> Option<u64> {
		self.writer
			.as_ref()
			.expect("group writer is available between frames")
			.room(0, window)
	}

	/// Restore the writer returned by a completed frame.
	pub fn restore(&mut self, writer: group::Producer) {
		debug_assert!(self.writer.is_none());
		self.writer = Some(writer);
	}

	/// Own a group for the lifetime of its receive handler.
	pub fn new(producer: group::Producer) -> Self {
		Self {
			key: producer.start,
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

	/// Finish receiving a group ended without saying where it ends, as a bare FIN does,
	/// without running cancellation cleanup.
	pub fn finish(mut self) -> Result<(), Error> {
		self.writer.take().expect("group owned until consumed").finish()?;
		self.armed = false;
		Ok(())
	}

	/// Finish receiving a whole group that ends at `end`, as GROUP_END says, without running
	/// cancellation cleanup. An `end` the group refuses (see [`group::Producer::finish_at`]) is
	/// [`Error::MalformedTrack`], which aborts the group; one already aborted stays so.
	pub fn finish_at(mut self, end: u64) -> Result<(), Error> {
		if self.check_end(end).is_err() {
			let _ = self.abort(Error::MalformedTrack);
			return Err(Error::MalformedTrack);
		}
		let _ = self.writer.take().expect("group owned until consumed").finish_at(end);
		self.armed = false;
		Ok(())
	}

	/// Finish receiving as the stream ended: whole at a GROUP_END's `end`, else without one.
	/// Only an `end` the group refuses fails (see [`Self::finish_at`]).
	pub fn end(self, end: Option<u64>) -> Result<(), Error> {
		match end {
			Some(end) => self.finish_at(end),
			None => {
				let _ = self.finish();
				Ok(())
			}
		}
	}

	/// Close a lite-07 group that ended with bare FIN (no GROUP_END) under a mid-group
	/// Position End cap, marking it back-cut so a later switch-back fetches the remaining
	/// frames rather than reusing the truncated slice.
	pub fn end_slice(self) -> Result<(), Error> {
		self.handle.bound_back();
		self.finish()
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
		let producer = group::Info { start: 0 }.produce();
		let mut consumer = producer.consume();
		let mut receiving = Group::new(producer);
		let frame = receiving
			.create_object(
				crate::frame::Info {
					size: 2,
					timestamp: crate::Timestamp::ZERO,
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
		let producer = group::Info { start: 0 }.produce();
		let mut consumer = producer.consume();
		let mut receiving = Group::new(producer);
		let frame = receiving
			.create_object(
				crate::frame::Info {
					size: 2,
					timestamp: crate::Timestamp::ZERO,
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
