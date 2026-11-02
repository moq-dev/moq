//! Incoming streams: their handles, leases, and errors.

use std::collections::hash_map::{self, Entry};
use std::{io, mem, task::Poll};

use thiserror::Error;
use tracing::debug;

use super::lease::Leased;
use super::state::get_or_insert_recv;
use super::{Retransmits, ShouldTransmit, StreamId, StreamsState};
use crate::connection::assembler::Assembler;
pub use crate::connection::assembler::Chunk;
use crate::connection::streams::state::StreamRecv;
use crate::connection::waits::Wait;
use crate::{TransportError, VarInt, frame};

pub use super::lease::Lease;

/// An incoming stream, from [`Connection::recv_stream`](crate::connection::Connection::recv_stream)
///
/// A borrowed view by ID: the stream lives on in the connection after this is dropped, and
/// calls on an ID the stream no longer has state for fail with [`Error::Closed`].
pub struct Stream<'a> {
	pub(crate) id: StreamId,
	pub(crate) state: &'a mut StreamsState,
	pub(crate) pending: &'a mut Retransmits,
}

impl Stream<'_> {
	/// Read the stream's next contiguous bytes without copying.
	pub fn read(&mut self) -> Result<Chunks<'_>, Error> {
		Chunks::new(self.id, self.state, self.pending)
	}

	/// Stop accepting data on the given receive stream
	///
	/// Discards unread and leased data, returning its credit, and notifies the peer to stop
	/// transmitting. Once stopped, further calls on the stream fail with [`Error::Closed`].
	pub fn stop(&mut self, error_code: VarInt) -> Result<(), Error> {
		let hash_map::Entry::Occupied(mut entry) = self.state.recv.entry(self.id) else {
			return Err(Error::Closed);
		};
		let stream = get_or_insert_recv(entry.get_mut(), self.state.initial_stream_receive_window);

		// An eviction already discarded the buffer, leaving the peer and the bytes the reader
		// took to this stop.
		let (read_credits, stop_sending) = match stream.lease.evicted {
			true => (0, stream.final_offset_unknown()),
			false => {
				let (credits, stop_sending) = stream.stop()?;
				(credits, stop_sending.should_transmit())
			}
		};
		let mut lease = std::mem::take(&mut stream.lease);
		if stop_sending {
			self.pending.stop_sending.push(frame::StopSending {
				id: self.id,
				error_code,
			});
		}

		// We need to keep stopped streams around until they're finished or reset so we can update
		// connection-level flow control to account for discarded data. Otherwise, we can discard
		// state immediately.
		if !stream.final_offset_unknown() {
			let recv = entry.remove().expect("must have recv when stopping");
			// The lease was taken out above, so this returns no credit.
			let _ = self.state.stream_recv_freed(self.id, recv);
			self.state.queue_max_stream_id(self.pending);
		}

		let leased = self.state.discard_lease(self.id, &mut lease);
		let transmit = self.state.add_read_credits(read_credits);
		self.pending.max_data |= leased.should_transmit() || transmit.should_transmit();
		// The credit returned may only have paid a shrunk window's debt.
		self.unstick();
		// A reader parked on the stream learns it is closed.
		self.state.waits.wake(self.id, Wait::Read);

		Ok(())
	}

	/// Check whether this stream has been reset by the peer, returning the reset error code if so
	///
	/// After returning `Ok(Some(_))` once, stream state will be discarded and all future calls will
	/// fail with [`Error::Closed`].
	pub fn received_reset(&mut self) -> Result<Option<VarInt>, Error> {
		let hash_map::Entry::Occupied(entry) = self.state.recv.entry(self.id) else {
			return Err(Error::Closed);
		};
		let Some(s) = entry.get().as_ref().and_then(|s| s.as_open_recv()) else {
			return Ok(None);
		};
		if s.stopped {
			return Err(Error::Closed);
		}
		let Some(code) = s.reset_code() else {
			return Ok(None);
		};

		// Clean up state after application observes the reset, since there's no reason for the
		// application to attempt to read or stop the stream once it knows it's reset. A lease still
		// holding credit returns it here, since nothing is left to release or stop it.
		let (_, recv) = entry.remove_entry();
		let transmit = self
			.state
			.stream_recv_freed(self.id, recv.expect("must have recv on reset"));
		self.pending.max_data |= transmit.should_transmit();
		self.state.queue_max_stream_id(self.pending);
		// The credit returned may only have paid a shrunk window's debt.
		self.unstick();

		Ok(Some(code))
	}
}

#[derive(Debug, Default)]
pub(super) struct Recv {
	// NB: when adding or removing fields, remember to update `reinit`.
	state: RecvState,
	pub(super) assembler: Assembler,
	sent_max_stream_data: u64,
	pub(super) end: u64,
	/// The end of the data STREAM frames carried, which on an ordered transport is where the
	/// next one starts. Unlike `end`, a reset's final size does not raise it.
	#[cfg(feature = "mux")]
	pub(super) data_end: u64,
	pub(super) stopped: bool,
	/// What a reader leased, which lives as long as the stream does.
	pub(super) lease: Leased,
}

impl Recv {
	pub(super) fn new(initial_max_data: u64) -> Box<Self> {
		Box::new(Self {
			state: RecvState::default(),
			assembler: Assembler::new(),
			sent_max_stream_data: initial_max_data,
			end: 0,
			#[cfg(feature = "mux")]
			data_end: 0,
			stopped: false,
			lease: Leased::default(),
		})
	}

	/// Reset to the initial state
	pub(super) fn reinit(&mut self, initial_max_data: u64) {
		self.state = RecvState::default();
		self.assembler.reinit();
		self.sent_max_stream_data = initial_max_data;
		self.end = 0;
		#[cfg(feature = "mux")]
		{
			self.data_end = 0;
		}
		self.stopped = false;
		self.lease = Leased::default();
	}

	/// Process a STREAM frame
	///
	/// Return value is `(number_of_new_bytes_ingested, stream_is_closed)`
	pub(super) fn ingest(
		&mut self,
		mut frame: frame::Stream,
		payload_len: usize,
		received: u64,
		max_data: u64,
	) -> Result<(u64, bool), TransportError> {
		let end = frame.offset + frame.data.len() as u64;
		if end >= 2u64.pow(62) {
			return Err(TransportError::FLOW_CONTROL_ERROR("maximum stream offset too large"));
		}

		if let Some(final_offset) = self.final_offset()
			&& (end > final_offset || (frame.fin && end != final_offset))
		{
			debug!(end, final_offset, "final size error");
			return Err(TransportError::FINAL_SIZE_ERROR(""));
		}
		// A first FIN can't end the stream below data already received (RFC 9000 section 4.5)
		if frame.fin && end < self.end {
			return Err(TransportError::FINAL_SIZE_ERROR("lower than high water mark"));
		}

		let new_bytes = self.credit_consumed_by(end, received, max_data)?;

		// A stopped stream keeps no data, but an evicted one waits for its stop knowing whether
		// the peer is done.
		if frame.fin
			&& let RecvState::Recv { size } = &mut self.state
		{
			*size = Some(end);
		}

		self.end = self.end.max(end);
		#[cfg(feature = "mux")]
		{
			self.data_end = self.data_end.max(end);
		}
		// A reset stream only still delivers the data below its Reliable Size
		if let RecvState::ResetRecvd { reliable_size, .. } = self.state {
			let len = reliable_size.saturating_sub(frame.offset).min(frame.data.len() as u64);
			frame.data.truncate(len as usize);
		}
		// Don't bother storing data or releasing stream-level flow control credit if the stream's
		// already stopped
		if !self.stopped && !frame.data.is_empty() {
			self.assembler
				.insert(frame.offset, frame.data, payload_len)
				.map_err(|_| TransportError::INTERNAL_ERROR("too many gaps in stream buffer"))?;
		}

		Ok((new_bytes, frame.fin && self.stopped))
	}

	pub(super) fn stop(&mut self) -> Result<(u64, ShouldTransmit), Error> {
		if self.stopped {
			return Err(Error::Closed);
		}

		let read_credits = self.unread_credit();
		// This may send a spurious STOP_SENDING if we've already received all data, but it's a bit
		// fiddly to distinguish that from the case where we've received a FIN but are missing some
		// data that the peer might still be trying to retransmit, in which case a STOP_SENDING is
		// still useful. A stream read to its end, as a lease leaves it, sends none.
		let stop_sending = self.is_receiving() && !self.is_read();
		self.stopped = true;
		self.assembler.clear();
		Ok((read_credits, ShouldTransmit(stop_sending)))
	}

	/// The connection credit held for data the application has not read: everything below the
	/// end, or after a reset, what is still owed below its Reliable Size.
	pub(super) fn unread_credit(&self) -> u64 {
		match self.state {
			RecvState::Recv { .. } => self.end - self.assembler.bytes_read(),
			RecvState::ResetRecvd { reliable_size, .. } => self.assembler.unread_before(reliable_size),
		}
	}

	/// Whether the application has read everything the stream will deliver.
	pub(super) fn is_read(&self) -> bool {
		self.final_offset().is_some() && self.unread_credit() == 0
	}

	/// Returns the window that should be advertised in a `MAX_STREAM_DATA` frame
	///
	/// The method returns a tuple which consists of the window that should be
	/// announced, as well as a boolean parameter which indicates if a new
	/// transmission of the value is recommended. If the boolean value is
	/// `false` the new window should only be transmitted if a previous transmission
	/// had failed.
	pub(super) fn max_stream_data(&mut self, stream_receive_window: u64) -> (u64, ShouldTransmit) {
		// A window shrunk since the last announcement cannot take back credit already granted.
		let max_stream_data = (self.assembler.bytes_read() + stream_receive_window).max(self.sent_max_stream_data);

		// Only announce a window update if it's significant enough
		// to make it worthwhile sending a MAX_STREAM_DATA frame.
		// We use here a fraction of the configured stream receive window to make
		// the decision, and accommodate for streams using bigger windows requiring
		// less updates. A fixed size would also work - but it would need to be
		// smaller than `stream_receive_window` in order to make sure the stream
		// does not get stuck.
		let diff = max_stream_data - self.sent_max_stream_data;
		let transmit = self.can_send_flow_control() && diff >= (stream_receive_window / 8);
		(max_stream_data, ShouldTransmit(transmit))
	}

	/// Records that a `MAX_STREAM_DATA` announcing a certain window was sent
	///
	/// This will suppress enqueuing further `MAX_STREAM_DATA` frames unless
	/// either the previous transmission was not acknowledged or the window
	/// further increased.
	pub(super) fn record_sent_max_stream_data(&mut self, sent_value: u64) {
		if sent_value > self.sent_max_stream_data {
			self.sent_max_stream_data = sent_value;
		}
	}

	/// Whether the total amount of data that the peer will send on this stream is unknown
	///
	/// True until we've received either a reset or the final frame.
	///
	/// Implies that the sender might benefit from stream-level flow control updates, and we might
	/// need to issue connection-level flow control updates due to flow control budget use by this
	/// stream in the future, even if it's been stopped.
	pub(super) fn final_offset_unknown(&self) -> bool {
		matches!(self.state, RecvState::Recv { size: None })
	}

	/// Whether stream-level flow control updates should be sent for this stream
	pub(super) fn can_send_flow_control(&self) -> bool {
		// Stream-level flow control is redundant if the sender has already sent the whole stream,
		// and moot if we no longer want data on this stream.
		self.final_offset_unknown() && !self.stopped
	}

	/// Whether data is still being accepted from the peer
	pub(super) fn is_receiving(&self) -> bool {
		match self.state {
			RecvState::Recv { .. } => true,
			RecvState::ResetRecvd { reliable_size, .. } => reliable_size > 0,
		}
	}

	fn final_offset(&self) -> Option<u64> {
		match self.state {
			RecvState::Recv { size } => size,
			RecvState::ResetRecvd { size, .. } => Some(size),
		}
	}

	/// Process a RESET_STREAM, or a RESET_STREAM_AT that still delivers `reliable_size` bytes
	///
	/// Returns `None` iff the reset was redundant, otherwise the bytes newly counted against
	/// connection flow control and the connection credit the abandoned data frees.
	pub(super) fn reset(
		&mut self,
		error_code: VarInt,
		final_offset: VarInt,
		reliable_size: u64,
		received: u64,
		max_data: u64,
	) -> Result<Option<(u64, u64)>, TransportError> {
		let final_offset = u64::from(final_offset);
		// Validate final_offset
		if let Some(offset) = self.final_offset() {
			if offset != final_offset {
				return Err(TransportError::FINAL_SIZE_ERROR("inconsistent value"));
			}
		} else if self.end > final_offset {
			return Err(TransportError::FINAL_SIZE_ERROR("lower than high water mark"));
		}
		let new_bytes = self.credit_consumed_by(final_offset, received, max_data)?;

		// The data the application could still read before this reset
		let owed = match self.state {
			RecvState::ResetRecvd {
				error_code: prev,
				reliable_size: owed,
				..
			} => {
				if prev != error_code {
					return Err(TransportError::STREAM_STATE_ERROR("reset error code changed"));
				}
				// A reordered, larger Reliable Size is ignored
				if reliable_size >= owed {
					return Ok(None);
				}
				owed
			}
			RecvState::Recv { .. } => final_offset,
		};
		self.end = final_offset;
		self.state = RecvState::ResetRecvd {
			size: final_offset,
			error_code,
			reliable_size,
		};
		// Credit the abandoned data now, and drop what we buffered of it. Reads credit the rest.
		let freed = self.assembler.unread_before(owed) - self.assembler.unread_before(reliable_size);
		self.assembler.truncate(reliable_size);
		Ok(Some((new_bytes, freed)))
	}

	/// The reset error code, once the data below the Reliable Size has been read
	pub(super) fn reset_code(&self) -> Option<VarInt> {
		match self.state {
			RecvState::ResetRecvd {
				error_code,
				reliable_size,
				..
			} if self.assembler.unread_before(reliable_size) == 0 => Some(error_code),
			_ => None,
		}
	}

	/// Compute the amount of flow control credit consumed, or return an error if more was consumed
	/// than issued
	fn credit_consumed_by(&self, offset: u64, received: u64, max_data: u64) -> Result<u64, TransportError> {
		let prev_end = self.end;
		let new_bytes = offset.saturating_sub(prev_end);
		if offset > self.sent_max_stream_data || received + new_bytes > max_data {
			debug!(
				received,
				new_bytes,
				max_data,
				offset,
				stream_max_data = self.sent_max_stream_data,
				"flow control error"
			);
			return Err(TransportError::FLOW_CONTROL_ERROR(""));
		}

		Ok(new_bytes)
	}
}

/// Chunks returned from [`Stream::read()`].
///
/// ### Note: Finalization Needed
/// Bytes read from the stream are not released from the congestion window until
/// either [`Self::finalize()`] is called, or this type is dropped.
///
/// It is recommended that you call [`Self::finalize()`] because it returns a flag
/// telling you whether reading from the stream has resulted in the need to transmit a packet.
///
/// If this type is leaked, the stream will remain blocked on the remote peer until
/// another read from the stream is done.
pub struct Chunks<'a> {
	id: StreamId,
	streams: &'a mut StreamsState,
	pending: &'a mut Retransmits,
	state: ChunksState,
	read: u64,
}

impl<'a> Chunks<'a> {
	pub(super) fn new(
		id: StreamId,
		streams: &'a mut StreamsState,
		pending: &'a mut Retransmits,
	) -> Result<Self, Error> {
		let mut entry = match streams.recv.entry(id) {
			Entry::Occupied(entry) => entry,
			Entry::Vacant(_) => return Err(Error::Closed),
		};

		let recv = get_or_insert_recv(entry.get_mut(), streams.initial_stream_receive_window);
		if recv.stopped {
			return Err(Error::Closed);
		}
		let recv = entry.remove().unwrap().into_inner(); // initialized above
		Ok(Self {
			id,
			streams,
			pending,
			state: ChunksState::Readable(recv),
			read: 0,
		})
	}

	/// The next contiguous chunk of at most `max_length` bytes, or `None` once the stream finished
	///
	/// Fails with [`Error::Blocked`] until more arrives, or [`Error::Reset`]. Call
	/// [`Self::finalize()`] when done.
	pub fn next(&mut self, max_length: usize) -> Result<Option<Chunk>, Error> {
		let rs = match &mut self.state {
			ChunksState::Readable(rs) => rs,
			ChunksState::Reset(error_code) => {
				return Err(Error::Reset(*error_code));
			}
			ChunksState::Finished => {
				return Ok(None);
			}
			ChunksState::Finalized => panic!("must not call next() after finalize()"),
		};

		if let Some(chunk) = rs.assembler.read(max_length) {
			self.read += chunk.bytes.len() as u64;
			return Ok(Some(chunk));
		}

		match rs.state {
			RecvState::ResetRecvd { reliable_size, .. } if rs.assembler.unread_before(reliable_size) > 0 => {
				// The data below the Reliable Size is still in flight
				Err(Error::Blocked)
			}
			RecvState::ResetRecvd { error_code, .. } => {
				let state = mem::replace(&mut self.state, ChunksState::Reset(error_code));
				// At this point if we have `rs` self.state must be `ChunksState::Readable`
				let recv = match state {
					ChunksState::Readable(recv) => StreamRecv::Open(recv),
					_ => unreachable!("state must be ChunkState::Readable"),
				};
				self.pending.max_data |= self.streams.stream_recv_freed(self.id, recv).should_transmit();
				Err(Error::Reset(error_code))
			}
			RecvState::Recv { size } => {
				if size == Some(rs.end) && rs.assembler.bytes_read() == rs.end {
					let state = mem::replace(&mut self.state, ChunksState::Finished);
					// At this point if we have `rs` self.state must be `ChunksState::Readable`
					let recv = match state {
						ChunksState::Readable(recv) => StreamRecv::Open(recv),
						_ => unreachable!("state must be ChunkState::Readable"),
					};
					self.pending.max_data |= self.streams.stream_recv_freed(self.id, recv).should_transmit();
					Ok(None)
				} else {
					// We don't need a distinct `ChunksState` variant for a blocked stream because
					// retrying a read harmlessly re-traces our steps back to returning
					// `Err(Blocked)` again. The buffers can't refill and the stream's own state
					// can't change so long as this `Chunks` exists.
					Err(Error::Blocked)
				}
			}
		}
	}

	/// [`Self::next`], or wait for more to arrive
	///
	/// Pending registers `waiter` until data, a FIN, or a reset arrives. Data that arrived stays
	/// readable after the connection is lost, and only then does this fail with
	/// [`Error::ConnectionLost`].
	pub fn poll_next(&mut self, waiter: &kio::Waiter, max_length: usize) -> Poll<Result<Option<Chunk>, Error>> {
		match self.next(max_length) {
			Err(Error::Blocked) if self.streams.waits.lost().is_some() => Poll::Ready(Err(Error::ConnectionLost)),
			Err(Error::Blocked) => {
				self.streams.waits.register(self.id, Wait::Read, waiter);
				Poll::Pending
			}
			result => Poll::Ready(result),
		}
	}

	/// Mark the read data as consumed from the stream.
	///
	/// The number of read bytes will be released from the congestion window,
	/// allowing the remote peer to send more data if it was previously blocked.
	///
	/// If [`ShouldTransmit::should_transmit()`] returns `true`,
	/// a packet needs to be sent to the peer informing them that the stream is unblocked.
	/// This means that you should call [`Connection::poll_transmit()`][crate::connection::Connection::poll_transmit]
	/// and send the returned packet as soon as is reasonable, to unblock the remote peer.
	pub fn finalize(mut self) -> ShouldTransmit {
		self.finalize_inner()
	}

	fn finalize_inner(&mut self) -> ShouldTransmit {
		let state = mem::replace(&mut self.state, ChunksState::Finalized);
		if let ChunksState::Finalized = state {
			// Noop on repeated calls
			return ShouldTransmit(false);
		}

		// We issue additional stream ID credit after the application is notified that a previously
		// open stream has finished or been reset and we've therefore disposed of its state, as
		// recorded by `stream_freed` calls in `next`.
		let mut should_transmit = self.streams.queue_max_stream_id(self.pending);

		// If the stream hasn't finished, we may need to issue stream-level flow control credit
		if let ChunksState::Readable(mut rs) = state {
			let (_, max_stream_data) = rs.max_stream_data(self.streams.stream_receive_window);
			should_transmit |= max_stream_data.0;
			if max_stream_data.0 {
				self.pending.max_stream_data.insert(self.id);
			}
			// Return the stream to storage for future use
			self.streams.recv.insert(self.id, Some(StreamRecv::Open(rs)));
		}

		// Issue connection-level flow control credit for any data we read regardless of state
		let max_data = self.streams.add_read_credits(self.read);
		self.pending.max_data |= max_data.0;
		should_transmit |= max_data.0;
		ShouldTransmit(should_transmit)
	}
}

impl Drop for Chunks<'_> {
	fn drop(&mut self) {
		let _ = self.finalize_inner();
	}
}

enum ChunksState {
	Readable(Box<Recv>),
	Reset(VarInt),
	Finished,
	Finalized,
}

/// Why a read or lease failed
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
	/// Nothing more has arrived yet; [`Chunks::poll_next`] and [`Stream::poll_lease`] wait for it
	#[error("blocked")]
	Blocked,
	/// The peer abandoned the stream, with its code
	#[error("reset by peer: code {0}")]
	Reset(VarInt),
	/// The connection window was stuck and this stream was discarded to free it
	///
	/// The peer has not been told: stop the stream with [`Stream::stop()`] and a code of the
	/// caller's choosing. Until then the peer may keep sending, which costs bandwidth but no
	/// window, since it is credited on arrival, and the bytes the reader took keep their credit,
	/// which holds further eviction off.
	#[error("evicted")]
	Evicted,
	/// A lease wanted more bytes than the connection receive window can ever hold
	#[error("want exceeds the receive window")]
	TooLarge,
	/// The stream was stopped, or never opened, or already read to its end
	#[error("closed stream")]
	Closed,
	/// The connection was lost before the stream ended; [`Connection::poll_closed`](crate::connection::Connection::poll_closed)
	/// says why
	#[error("connection lost")]
	ConnectionLost,
}

impl From<Error> for io::Error {
	fn from(err: Error) -> Self {
		let kind = match err {
			Error::Blocked => io::ErrorKind::WouldBlock,
			Error::Reset(_) | Error::Evicted => io::ErrorKind::ConnectionReset,
			Error::TooLarge => io::ErrorKind::InvalidInput,
			Error::Closed | Error::ConnectionLost => io::ErrorKind::NotConnected,
		};
		Self::new(kind, err)
	}
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum RecvState {
	Recv {
		size: Option<u64>,
	},
	/// Reset, once the data below `reliable_size` has been read
	ResetRecvd {
		size: u64,
		error_code: VarInt,
		reliable_size: u64,
	},
}

impl Default for RecvState {
	fn default() -> Self {
		Self::Recv { size: None }
	}
}

#[cfg(test)]
mod tests {
	use bytes::Bytes;

	use crate::{Dir, Side};

	use super::*;

	#[test]
	fn reordered_frames_while_stopped() {
		const INITIAL_BYTES: u64 = 3;
		const INITIAL_OFFSET: u64 = 3;
		const RECV_WINDOW: u64 = 8;
		let mut s = Recv::new(RECV_WINDOW);
		let mut data_recvd = 0;
		// Receive bytes 3..6
		let (new_bytes, is_closed) = s
			.ingest(
				frame::Stream {
					id: StreamId::new(Side::Client, Dir::Uni, 0),
					offset: INITIAL_OFFSET,
					fin: false,
					data: Bytes::from_static(&[0; INITIAL_BYTES as usize]),
				},
				123,
				data_recvd,
				data_recvd + 1024,
			)
			.unwrap();
		data_recvd += new_bytes;
		assert_eq!(new_bytes, INITIAL_OFFSET + INITIAL_BYTES);
		assert!(!is_closed);

		let (credits, transmit) = s.stop().unwrap();
		assert!(transmit.should_transmit());
		assert_eq!(
			credits,
			INITIAL_OFFSET + INITIAL_BYTES,
			"full connection flow control credit is issued by stop"
		);

		let (max_stream_data, transmit) = s.max_stream_data(RECV_WINDOW);
		assert!(!transmit.should_transmit());
		assert_eq!(
			max_stream_data, RECV_WINDOW,
			"stream flow control credit isn't issued by stop"
		);

		// Receive byte 7
		let (new_bytes, is_closed) = s
			.ingest(
				frame::Stream {
					id: StreamId::new(Side::Client, Dir::Uni, 0),
					offset: RECV_WINDOW - 1,
					fin: false,
					data: Bytes::from_static(&[0; 1]),
				},
				123,
				data_recvd,
				data_recvd + 1024,
			)
			.unwrap();
		data_recvd += new_bytes;
		assert_eq!(new_bytes, RECV_WINDOW - (INITIAL_OFFSET + INITIAL_BYTES));
		assert!(!is_closed);

		let (max_stream_data, transmit) = s.max_stream_data(RECV_WINDOW);
		assert!(!transmit.should_transmit());
		assert_eq!(
			max_stream_data, RECV_WINDOW,
			"stream flow control credit isn't issued after stop"
		);

		// Receive bytes 0..3
		let (new_bytes, is_closed) = s
			.ingest(
				frame::Stream {
					id: StreamId::new(Side::Client, Dir::Uni, 0),
					offset: 0,
					fin: false,
					data: Bytes::from_static(&[0; INITIAL_OFFSET as usize]),
				},
				123,
				data_recvd,
				data_recvd + 1024,
			)
			.unwrap();
		assert_eq!(
			new_bytes, 0,
			"reordered frames don't issue connection-level flow control for stopped streams"
		);
		assert!(!is_closed);

		let (max_stream_data, transmit) = s.max_stream_data(RECV_WINDOW);
		assert!(!transmit.should_transmit());
		assert_eq!(
			max_stream_data, RECV_WINDOW,
			"stream flow control credit isn't issued after stop"
		);
	}
}
