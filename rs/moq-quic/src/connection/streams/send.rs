//! Outgoing streams and datagrams: their handles, scheduling, and errors.

use std::io;

use bytes::Buf;
use thiserror::Error;
use tracing::trace;

use super::{StreamEvent, StreamsState, schedule::Slot};
use crate::connection::{ConnectionTag, State as ConnState, send_buffer::SendBuffer, spaces::Retransmits};
use crate::{StreamId, VarInt, frame};

#[derive(Debug)]
pub(super) struct Send {
	pub(super) max_data: u64,
	pub(super) state: SendState,
	pub(super) pending: SendBuffer,
	pub(super) priority: i64,
	pub(super) slot: Slot,
	/// Whether a frame containing a FIN bit must be transmitted, even if we don't have any new data
	pub(super) fin_pending: bool,
	/// Value of `max_data` for which a `STREAM_DATA_BLOCKED` frame was most recently queued
	///
	/// A new frame is only queued once the peer has raised the limit.
	pub(super) data_blocked_limit: Option<u64>,
	/// The reason the peer wants us to stop, if `STOP_SENDING` was received
	pub(super) stop_reason: Option<VarInt>,
	/// The lowest offset a [`Stream::acked`] call is waiting for
	pub(super) notify: Option<u64>,
	/// The application gave up the stream's ID with [`Stream::release`], so its end is not kept
	pub(super) released: bool,
}

impl Send {
	pub(super) fn new(max_data: VarInt) -> Box<Self> {
		Box::new(Self {
			max_data: max_data.into(),
			state: SendState::Ready,
			pending: SendBuffer::new(),
			priority: 0,
			slot: Slot::default(),
			fin_pending: false,
			data_blocked_limit: None,
			stop_reason: None,
			notify: None,
			released: false,
		})
	}

	/// Whether the stream has been reset
	pub(super) fn is_reset(&self) -> bool {
		matches!(self.state, SendState::ResetSent { .. })
	}

	pub(super) fn finish(&mut self) -> Result<(), Error> {
		if let Some(error_code) = self.stop_reason {
			Err(Error::Stopped(error_code))
		} else if self.state == SendState::Ready {
			self.state = SendState::DataSent { finish_acked: false };
			self.fin_pending = true;
			Ok(())
		} else {
			Err(Error::Closed)
		}
	}

	/// Queue as much of `data` as `limit` and the stream's flow control allow.
	///
	/// Each chunk is taken with [`Buf::copy_to_bytes`], which a [`Bytes`](bytes::Bytes) source
	/// splits off without copying.
	pub(super) fn write(&mut self, data: &mut impl Buf, limit: u64) -> Result<usize, Error> {
		if !self.is_writable() {
			return Err(Error::Closed);
		}
		if let Some(error_code) = self.stop_reason {
			return Err(Error::Stopped(error_code));
		}
		let budget = self.max_data - self.pending.offset();
		if budget == 0 || limit == 0 {
			return Err(Error::Blocked);
		}
		let mut limit = usize::try_from(limit.min(budget)).unwrap_or(usize::MAX);

		let mut written = 0;
		while limit > 0 {
			let take = data.chunk().len().min(limit);
			if take == 0 {
				break;
			}
			self.pending.write(data.copy_to_bytes(take));
			limit -= take;
			written += take;
		}
		Ok(written)
	}

	/// How the stream ended, if it has: a peer's stop wins over the end it interrupted.
	pub(super) fn end(&self) -> Option<End> {
		if let Some(code) = self.stop_reason {
			return Some(End::Stopped(code));
		}
		match self.state {
			SendState::ResetSent { error_code, .. } => Some(End::Reset(error_code)),
			SendState::DataSent { finish_acked: true } if self.pending.is_fully_acked() => Some(End::Finished),
			_ => None,
		}
	}

	/// Update stream state due to a reset sent by the local application
	///
	/// Data below `reliable_size` is still delivered; everything after it is abandoned. Returns
	/// the Reliable Size of the reset to send now, or `None` while a peer without RESET_STREAM_AT
	/// (`!at`) still has data below `reliable_size` to acknowledge, after which
	/// [`take_held_reset`](Self::take_held_reset) releases it.
	pub(super) fn reset(&mut self, error_code: VarInt, reliable_size: u64, at: bool) -> Option<u64> {
		use SendState::*;
		let (DataSent { .. } | Ready) = self.state else {
			return None;
		};
		debug_assert!(reliable_size <= self.pending.offset());
		let final_size = self.pending.offset();
		self.pending.truncate(reliable_size);
		self.fin_pending = false;
		let send = at || self.pending.is_fully_acked();
		self.state = ResetSent {
			final_size,
			reliable_size,
			error_code,
			held: !send,
			acked: None,
		};
		send.then_some(reliable_size)
	}

	/// Release a held RESET_STREAM once all the data it holds for has been acknowledged, returning
	/// its Reliable Size
	pub(super) fn take_held_reset(&mut self) -> Option<u64> {
		let SendState::ResetSent {
			held, reliable_size, ..
		} = &mut self.state
		else {
			return None;
		};
		if !*held || !self.pending.is_fully_acked() {
			return None;
		}
		*held = false;
		Some(*reliable_size)
	}

	/// The Reliable Size this stream's reset delivers, if it was reset
	pub(super) fn reliable_size(&self) -> Option<u64> {
		match self.state {
			SendState::ResetSent { reliable_size, .. } => Some(reliable_size),
			_ => None,
		}
	}

	/// Abandon the data a reset still delivers, returning the Reliable Size of 0 for the reset to
	/// send now
	///
	/// For a peer that sent STOP_SENDING, which will not read it. This releases a held
	/// RESET_STREAM, and replaces a RESET_STREAM_AT whose data is still unacknowledged.
	pub(super) fn abandon_reliable(&mut self) -> Option<u64> {
		let SendState::ResetSent {
			reliable_size, held, ..
		} = &mut self.state
		else {
			return None;
		};
		if self.pending.is_fully_acked() {
			return None;
		}
		*reliable_size = 0;
		*held = false;
		self.pending.truncate(0);
		Some(0)
	}

	/// The frame announcing this stream's reset, if it was reset
	///
	/// It carries a Reliable Size only when the peer accepts RESET_STREAM_AT (`at`).
	pub(super) fn reset_frame(&self, id: StreamId, at: bool) -> Option<frame::ResetStream> {
		let SendState::ResetSent {
			final_size,
			reliable_size,
			error_code,
			..
		} = self.state
		else {
			return None;
		};
		Some(frame::ResetStream {
			id,
			error_code,
			final_offset: VarInt::try_from(final_size).expect("impossibly large offset"),
			// A zero Reliable Size is a plain RESET_STREAM, which any peer accepts
			reliable_size: (at && reliable_size > 0)
				.then(|| VarInt::try_from(reliable_size).expect("impossibly large offset")),
		})
	}

	/// Handle acknowledgement of a reset frame sent while the Reliable Size was `reliable_size`
	///
	/// Returns whether the current reset and all data below its Reliable Size have been
	/// acknowledged
	pub(super) fn reset_acked(&mut self, reliable_size: u64) -> bool {
		let SendState::ResetSent { acked, .. } = &mut self.state else {
			return false;
		};
		// Sizes only shrink, so the smallest one acknowledged is the latest
		*acked = Some(acked.map_or(reliable_size, |acked| acked.min(reliable_size)));
		self.is_reset_acked() && self.pending.is_fully_acked()
	}

	/// Whether the peer acknowledged a reset carrying the current Reliable Size, rather than one
	/// that STOP_SENDING has since shrunk
	fn is_reset_acked(&self) -> bool {
		matches!(
			self.state,
			SendState::ResetSent { reliable_size, acked, .. } if acked == Some(reliable_size)
		)
	}

	/// Handle STOP_SENDING
	///
	/// Returns true if the stream was stopped due to this frame, and false
	/// if it had been stopped before
	pub(super) fn try_stop(&mut self, error_code: VarInt) -> bool {
		if self.stop_reason.is_none() {
			self.stop_reason = Some(error_code);
			true
		} else {
			false
		}
	}

	/// Returns whether the stream has been finished or reset, and all data it still owes the peer
	/// has been acknowledged
	pub(super) fn ack(&mut self, frame: frame::StreamMeta) -> bool {
		self.pending.ack(frame.offsets);
		match &mut self.state {
			SendState::DataSent { finish_acked } => {
				*finish_acked |= frame.fin;
				*finish_acked && self.pending.is_fully_acked()
			}
			SendState::ResetSent { .. } => self.is_reset_acked() && self.pending.is_fully_acked(),
			SendState::Ready => false,
		}
	}

	/// Whether the peer acknowledged everything before `offset`, noting to report it if not
	pub(super) fn acked(&mut self, offset: u64) -> Result<bool, Error> {
		// An acknowledgment no longer means the peer reads the data, and a reset dropped the rest
		if self.stop_reason.is_some() || offset > self.pending.offset() {
			return Err(Error::Closed);
		}
		if offset <= self.pending.acked() {
			return Ok(true);
		}
		self.notify = Some(self.notify.map_or(offset, |notify| notify.min(offset)));
		Ok(false)
	}

	/// The acknowledged prefix to report once it reaches the offset a waiter is waiting for
	pub(super) fn take_acked(&mut self) -> Option<u64> {
		let acked = self.pending.acked();
		if self.notify.is_none_or(|notify| acked < notify) {
			return None;
		}
		self.notify = None;
		Some(acked)
	}

	/// The acknowledged prefix to report to every waiter after a reset or STOP_SENDING, which may
	/// fail any of them
	pub(super) fn take_waiting(&mut self) -> Option<u64> {
		self.notify.take()?;
		Some(self.pending.acked())
	}

	/// Handle increase to stream-level flow control limit
	///
	/// Returns whether the stream was unblocked
	pub(super) fn increase_max_data(&mut self, offset: u64) -> bool {
		if offset <= self.max_data || self.state != SendState::Ready {
			return false;
		}
		let was_blocked = self.pending.offset() == self.max_data;
		self.max_data = offset;
		was_blocked
	}

	pub(super) fn offset(&self) -> u64 {
		self.pending.offset()
	}

	pub(super) fn is_pending(&self) -> bool {
		self.pending.has_unsent_data() || self.fin_pending
	}

	pub(super) fn is_writable(&self) -> bool {
		matches!(self.state, SendState::Ready)
	}
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub(super) enum SendState {
	/// Sending new data
	Ready,
	/// Stream was finished; now sending retransmits only
	DataSent { finish_acked: bool },
	/// Reset by the application, while data below `reliable_size` is still delivered
	///
	/// The peer gets RESET_STREAM_AT, or, without it, a RESET_STREAM `held` until it has
	/// acknowledged that data.
	ResetSent {
		final_size: u64,
		reliable_size: u64,
		error_code: VarInt,
		held: bool,
		/// The smallest Reliable Size an acknowledged reset frame carried
		acked: Option<u64>,
	},
}

/// An outgoing stream, from [`Connection::send_stream`](crate::connection::Connection::send_stream)
///
/// A borrowed view by ID: the stream lives on in the connection after this is dropped, and
/// calls on an ID the stream no longer has state for fail with [`Error::Closed`].
pub struct Stream<'a> {
	pub(crate) id: StreamId,
	pub(crate) state: &'a mut StreamsState,
	pub(crate) pending: &'a mut Retransmits,
	pub(crate) conn_state: &'a ConnState,
}

impl<'a> Stream<'a> {
	/// Drive a bare `StreamsState` without a connection, for the `streams` fuzz target
	#[cfg(any(test, feature = "fuzz"))]
	pub(crate) fn new(
		id: StreamId,
		state: &'a mut StreamsState,
		pending: &'a mut Retransmits,
		conn_state: &'a ConnState,
	) -> Self {
		Self {
			id,
			state,
			pending,
			conn_state,
		}
	}

	/// Queue as much of `data` as the stream and connection accept now, returning how many bytes
	///
	/// Each chunk is taken with [`Buf::copy_to_bytes`], which a [`Bytes`](bytes::Bytes) source
	/// splits off without copying. Fails with [`Error::Blocked`] when nothing fits; a
	/// [`StreamEvent::Writable`] says when to retry.
	pub fn write(&mut self, data: &mut impl Buf) -> Result<usize, Error> {
		if self.conn_state.is_closed() {
			trace!(%self.id, "write blocked; connection draining");
			return Err(Error::Blocked);
		}

		let limit = self.state.write_limit();
		let max_data = self.state.max_send_data(self.id);
		let stream = StreamsState::send_mut(&mut self.state.send, self.id, max_data)?;
		let was_pending = stream.is_pending();
		let written = match stream.write(data, limit) {
			Ok(written) => written,
			Err(Error::Blocked) => {
				if limit == 0 {
					trace!(
						stream = %self.id, max_data = self.state.max_data, data_sent = self.state.data_sent,
						"write blocked by connection-level flow control or send window"
					);
					self.state.connection_blocked.insert(self.id);
					// Only report the peer's limit, not our own send window.
					if self.state.data_sent == self.state.max_data
						&& self.state.data_blocked_limit != Some(self.state.max_data)
					{
						self.state.data_blocked_limit = Some(self.state.max_data);
						self.pending.data_blocked = true;
					}
				} else if stream.data_blocked_limit != Some(stream.max_data) {
					stream.data_blocked_limit = Some(stream.max_data);
					self.pending.stream_data_blocked.insert(self.id);
				}
				return Err(Error::Blocked);
			}
			Err(e) => return Err(e),
		};
		self.state.data_sent += written as u64;
		self.state.buffered_data += written as u64;
		trace!(stream = %self.id, "wrote {} bytes", written);
		if !was_pending {
			self.state.pending.push(self.id, &mut stream.slot, stream.priority);
		}
		Ok(written)
	}

	/// How the stream ended, or `None` while it is still going
	///
	/// The peer's stop, a reset, or a finish every byte of which the peer acknowledged. Kept
	/// until [`Self::release`], so it can be asked after the stream's state is gone.
	pub fn end(&self) -> Result<Option<End>, Error> {
		match self.state.send.get(&self.id) {
			Some(Some(stream)) if stream.released => Err(Error::Closed),
			Some(Some(stream)) => Ok(stream.end()),
			Some(None) => Ok(None),
			None => self.state.ends.get(&self.id).map(|&end| Some(end)).ok_or(Error::Closed),
		}
	}

	/// Give up the stream's ID: its [`end`](Self::end) is no longer kept
	///
	/// The stream itself carries on, so finish or reset it first. Every later call on the ID
	/// fails with [`Error::Closed`]. Without it, the end of every finished or reset stream is
	/// kept for the connection's life.
	pub fn release(self) {
		if self.state.ends.remove(&self.id).is_some() {
			return;
		}
		let max_data = self.state.max_send_data(self.id);
		if let Ok(stream) = StreamsState::send_mut(&mut self.state.send, self.id, max_data) {
			stream.released = true;
		}
	}

	/// Finish the stream, signalling that no more data will be sent
	///
	/// If this fails, the stream never reaches [`End::Finished`].
	pub fn finish(&mut self) -> Result<(), Error> {
		let max_data = self.state.max_send_data(self.id);
		let stream = StreamsState::send_mut(&mut self.state.send, self.id, max_data)?;
		let was_pending = stream.is_pending();
		stream.finish()?;
		self.state.connection_blocked.swap_remove(&self.id);
		if !was_pending {
			self.state.pending.push(self.id, &mut stream.slot, stream.priority);
		}

		Ok(())
	}

	/// Abandon transmitting data on the stream
	///
	/// Fails with [`Error::Closed`] if the stream was already reset or is gone.
	pub fn reset(&mut self, error_code: VarInt) -> Result<(), Error> {
		self.reset_to(0, error_code)
	}

	/// The offset the next write starts at, which is how many bytes were written so far
	///
	/// After a reset, the bytes the reset still delivers. Zero before the first write, or once
	/// the stream is gone.
	pub fn offset(&self) -> Offset {
		let value = match self.state.send.get(&self.id) {
			Some(Some(stream)) => stream.offset(),
			_ => 0,
		};
		Offset {
			id: self.id,
			tag: self.state.tag,
			value,
		}
	}

	/// Whether the peer acknowledged every byte before `offset`
	///
	/// A `false` notes to report a [`StreamEvent::Acked`] once the answer changes, so poll again
	/// then. The stream holds only its lowest waiting offset, so every waiter on it polls again
	/// on that event. Like [`End::Finished`], an acknowledgment only means the peer's QUIC stack
	/// holds the data, not that its application read it.
	///
	/// Fails with [`Error::Closed`] once the peer will never acknowledge it, or no longer reads
	/// it: a reset dropped the data before `offset`, the peer sent STOP_SENDING, or the
	/// connection closed.
	///
	/// # Panics
	/// - when `offset` came from another stream
	pub fn acked(&mut self, offset: Offset) -> Result<bool, Error> {
		let offset = self.own(offset);
		if self.conn_state.is_closed() {
			return Err(Error::Closed);
		}
		match self.state.send.get_mut(&self.id) {
			Some(Some(stream)) if !stream.released => stream.acked(offset),
			// Nothing was written, so an offset of it is 0.
			Some(None) => Ok(true),
			Some(Some(_)) => Err(Error::Closed),
			None => match self.state.ends.get(&self.id) {
				Some(End::Finished) => Ok(true),
				_ => Err(Error::Closed),
			},
		}
	}

	/// Abandon the stream, but still deliver the bytes before `offset`
	///
	/// Nothing past `offset` is sent or retransmitted. A peer that accepts RESET_STREAM_AT gets
	/// it at once, and reads the data before the error. Otherwise the data keeps being
	/// retransmitted, and a plain RESET_STREAM follows once the peer has acknowledged all of it, a
	/// round trip later. An acknowledgment only means the peer's QUIC stack holds the data:
	/// RFC 9000 lets it discard data its application has not read yet on RESET_STREAM, so this
	/// only reliably delivers data the peer parses on arrival, like a WebTransport stream header.
	/// A peer that asked us to stop sending gets the reset at once, without the data.
	///
	/// Fails with [`Error::Closed`] if the stream was already reset or is gone.
	///
	/// # Panics
	/// - when `offset` came from another stream
	pub fn reset_at(&mut self, offset: Offset, error_code: VarInt) -> Result<(), Error> {
		let offset = self.own(offset);
		self.reset_to(offset, error_code)
	}

	/// The value of an offset minted for this stream.
	fn own(&self, offset: Offset) -> u64 {
		assert!(
			offset.id == self.id && offset.tag == self.state.tag,
			"offset used on another stream"
		);
		offset.value
	}

	fn reset_to(&mut self, offset: u64, error_code: VarInt) -> Result<(), Error> {
		let at = self.state.peer_reset_stream_at;
		let max_data = self.state.max_send_data(self.id);
		let stream = StreamsState::send_mut(&mut self.state.send, self.id, max_data)?;
		if stream.is_reset() {
			// Redundant reset call
			return Err(Error::Closed);
		}
		// An offset is minted from what was written, which only grows until a reset.
		debug_assert!(offset <= stream.offset(), "reset past the data written");
		// The peer that sent STOP_SENDING will not read the prefix
		let reliable_size = match stream.stop_reason {
			Some(_) => 0,
			None => offset,
		};

		// Restore the portion of the send window consumed by the data that we aren't about to
		// send. We leave flow control alone because the peer's responsible for issuing additional
		// credit based on the final offset communicated in the RESET_STREAM frame we send.
		let buffered = stream.pending.buffered();
		let send = stream.reset(error_code, reliable_size, at);
		self.state.connection_blocked.swap_remove(&self.id);
		self.state.buffered_data -= buffered - stream.pending.buffered();
		if let Some(offset) = stream.take_waiting() {
			self.state.events.push_back(StreamEvent::Acked { id: self.id, offset });
		}
		// Only data below the Reliable Size is left to send
		if !stream.is_pending() {
			self.state.pending.remove(&mut stream.slot);
		}
		if let Some(reliable_size) = send {
			self.pending.reset_stream.push((self.id, reliable_size));
		}

		Ok(())
	}

	/// Set the priority within the stream's send group, or session-wide when ungrouped
	///
	/// Higher values go first; equal values share bytes round robin. A queued stream moves at
	/// once, behind the streams already queued at its new priority. With
	/// [`transport::Config::send_fairness`](crate::transport::Config::send_fairness) off, a stream
	/// its group is partway through keeps going first until it runs out of data.
	pub fn set_priority(&mut self, priority: i64) -> Result<(), Error> {
		let max_data = self.state.max_send_data(self.id);
		let stream = StreamsState::send_mut(&mut self.state.send, self.id, max_data)?;
		stream.priority = priority;
		self.state.pending.set_stream_priority(&mut stream.slot, priority);
		Ok(())
	}

	/// The priority of the stream
	pub fn priority(&self) -> Result<i64, Error> {
		match self.state.send.get(&self.id) {
			Some(Some(stream)) if stream.released => Err(Error::Closed),
			Some(stream) => Ok(stream.as_ref().map(|s| s.priority).unwrap_or_default()),
			None => Err(Error::Closed),
		}
	}

	/// Move the stream into a send group, or with `None` back to the ungrouped streams
	///
	/// Ungrouped streams are sent before any send group. A queued stream keeps its place.
	///
	/// # Panics
	/// - when `group` was opened on another connection
	pub fn set_group(&mut self, group: Option<&Group>) -> Result<(), Error> {
		let group = group.map(|g| g.index(self.state.tag));
		let max_data = self.state.max_send_data(self.id);
		let stream = StreamsState::send_mut(&mut self.state.send, self.id, max_data)?;
		self.state
			.pending
			.join(self.id, &mut stream.slot, group, stream.priority);
		Ok(())
	}
}

/// A position in an outgoing stream, from [`Stream::offset`]
///
/// Only the stream it came from takes it, so it never points past the data written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Offset {
	id: StreamId,
	tag: ConnectionTag,
	value: u64,
}

impl Offset {
	/// The same stream's offset `value` bytes in, at or before this one, for tests and the fuzz
	/// target.
	#[cfg(any(test, feature = "fuzz"))]
	pub(crate) fn rewind(self, value: u64) -> Self {
		assert!(value <= self.value, "rewound past {}", self.value);
		Self { value, ..self }
	}
}

impl From<Offset> for u64 {
	fn from(offset: Offset) -> Self {
		offset.value
	}
}

/// How an outgoing stream ended, from [`Stream::end`]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
	/// Finished, and the peer acknowledged every byte
	Finished,
	/// The peer asked us to stop sending, with its code
	Stopped(VarInt),
	/// Reset by us, with our code
	Reset(VarInt),
}

/// Scheduling of outgoing streams between send groups
///
/// A higher priority group preempts every lower one, and groups of equal priority get an
/// equal share of bytes no matter how many streams each has queued. Streams outside any group
/// go before all of it. A group's datagrams, from [`Datagrams::send`](crate::Datagrams::send),
/// share its bytes in the same order.
pub struct Groups<'a> {
	pub(crate) state: &'a mut StreamsState,
}

impl Groups<'_> {
	/// Open a send group at `priority`
	pub fn open(&mut self, priority: i64) -> Group {
		Group {
			index: self.state.pending.open_group(priority),
			tag: self.state.tag,
		}
	}

	/// Release a send group; its streams keep their place until they finish
	///
	/// # Panics
	/// - when `group` was opened on another connection
	pub fn close(&mut self, group: Group) {
		let index = group.index(self.state.tag);
		self.state.pending.close_group(index);
	}

	/// Change a group's priority, moving any data it has queued along with it
	///
	/// # Panics
	/// - when `group` was opened on another connection
	pub fn set_priority(&mut self, group: &Group, priority: i64) {
		let index = group.index(self.state.tag);
		self.state.pending.set_group_priority(index, priority);
	}

	/// The group's priority
	///
	/// # Panics
	/// - when `group` was opened on another connection
	pub fn priority(&self, group: &Group) -> i64 {
		self.state.pending.group_priority(group.index(self.state.tag))
	}
}

/// How an outgoing stream or datagram is scheduled, like W3C WebTransport's send options
///
/// The default is ungrouped at priority 0.
#[derive(Debug, Default, Clone, Copy)]
pub struct Options<'a> {
	/// The send group to join, or `None` for the ungrouped streams that go before every group
	pub group: Option<&'a Group>,
	/// The priority within the group: higher goes first, and a datagram goes ahead of a
	/// stream of equal priority
	pub priority: i64,
}

/// A send group, from [`Groups::open`]
///
/// Release it with [`Groups::close`] on the connection that opened it.
#[derive(Debug, PartialEq, Eq)]
#[must_use = "a send group lives until it is closed"]
pub struct Group {
	index: usize,
	tag: ConnectionTag,
}

impl Group {
	pub(crate) fn index(&self, tag: ConnectionTag) -> usize {
		assert_eq!(self.tag, tag, "send group used on another connection");
		self.index
	}
}

/// Why a stream write or datagram failed
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
	/// Flow control or the send buffer has no room; a [`StreamEvent::Writable`] says when to
	/// retry
	#[error("blocked")]
	Blocked,
	/// The peer asked us to stop sending, with its code
	#[error("stopped by peer: code {0}")]
	Stopped(VarInt),
	/// The datagram is larger than the path or the peer's limit allows
	#[error("datagram too large")]
	TooLarge,
	/// Datagrams are disabled locally or unsupported by the peer
	#[error("datagrams unsupported")]
	Unsupported,
	/// The stream was finished, reset, or released, or was never opened
	#[error("closed stream")]
	Closed,
}

impl From<Error> for io::Error {
	fn from(err: Error) -> Self {
		let kind = match err {
			Error::Blocked => io::ErrorKind::WouldBlock,
			Error::Stopped(_) => io::ErrorKind::ConnectionReset,
			Error::TooLarge => io::ErrorKind::InvalidInput,
			Error::Unsupported => io::ErrorKind::Unsupported,
			Error::Closed => io::ErrorKind::NotConnected,
		};
		Self::new(kind, err)
	}
}
