//! Incoming streams: their handles, leases, and errors.

use std::collections::hash_map::{self, Entry};
use std::{io, mem, task::Poll};

use thiserror::Error;
use tracing::debug;

use super::lease::Leased;
use super::state::get_or_insert_recv;
use super::{Retransmits, StreamId, StreamsState};
use crate::connection::assembler::Assembler;
pub use crate::connection::assembler::Chunk;
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
		// took to this stop. Send STOP_SENDING if the final size is still unknown or if gaps
		// existed before the evicted high-water mark (so the peer is still retransmitting).
		let (evicted, read_credits, stop_sending) = match stream.reader {
			Reader::Evicted { has_gaps } => {
				stream.reader = Reader::Stopped;
				(true, 0, stream.final_offset_unknown() || has_gaps)
			}
			_ => {
				let (read_credits, stop_sending) = stream.stop()?;
				(false, read_credits, stop_sending)
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

		let leased = self.state.discard_lease(self.id, &mut lease, evicted);
		let transmit = self.state.add_read_credits(read_credits);
		self.pending.max_data |= leased || transmit;
		// The credit returned may only have paid a shrunk window's debt.
		self.unstick();
		// A reader parked on the stream learns it is closed.
		self.state.waits.wake(self.id, Wait::Read);
		// STOP_SENDING, or the credit the stop returned.
		self.state.waits.wake_transmit();

		Ok(())
	}

	/// Check whether this stream has been reset by the peer, returning the reset error code if so
	///
	/// After returning `Ok(Some(_))` once, stream state will be discarded and all future calls will
	/// fail with [`Error::Closed`]. A stream evicted to free a stuck window fails with
	/// [`Error::Evicted`] until the application stops it, so a reader that watches for a reset
	/// instead of reading learns it too: bytes it took hold further eviction off until then.
	pub fn received_reset(&mut self) -> Result<Option<VarInt>, Error> {
		let Some(code) = self.reset_code()? else {
			return Ok(None);
		};

		// Clean up state after application observes the reset, since there's no reason for the
		// application to attempt to read or stop the stream once it knows it's reset. A lease still
		// holding credit returns it here, since nothing is left to release or stop it.
		let recv = self.state.recv.remove(&self.id).flatten();
		let transmit = self
			.state
			.stream_recv_freed(self.id, recv.expect("must have recv on reset"));
		self.pending.max_data |= transmit;
		self.state.queue_max_stream_id(self.pending);
		// The credit returned may only have paid a shrunk window's debt.
		self.unstick();
		self.state.waits.wake_transmit();

		Ok(Some(code))
	}

	/// The peer's reset code, as [`Self::received_reset`] reports it, but keeping the stream
	///
	/// A lease that failed with [`Error::Reset`] may still have reliable bytes for a smaller
	/// `want`, which asking for the code this way leaves leasable.
	pub fn reset_code(&self) -> Result<Option<VarInt>, Error> {
		let Some(slot) = self.state.recv.get(&self.id) else {
			return Err(Error::Closed);
		};
		let Some(s) = slot.as_deref() else {
			return Ok(None);
		};
		match s.reader {
			Reader::Reading => Ok(s.reset_code()),
			Reader::Evicted { .. } => Err(Error::Evicted),
			Reader::Stopped => Err(Error::Closed),
		}
	}

	/// [`Self::received_reset`], or wait for the peer to reset the stream
	///
	/// Pending registers `waiter` until data, a FIN, or a reset arrives, after which the answer
	/// may still be no reset. Fails with [`Error::Closed`] once the stream is gone, as when every
	/// byte up to a FIN was read, and with [`Error::ConnectionLost`] once the connection is lost
	/// without a reset.
	pub fn poll_received_reset(&mut self, waiter: &kio::Waiter) -> Poll<Result<VarInt, Error>> {
		match self.received_reset() {
			Ok(Some(code)) => Poll::Ready(Ok(code)),
			Err(err) => Poll::Ready(Err(err)),
			Ok(None) if self.state.waits.lost().is_some() => Poll::Ready(Err(Error::ConnectionLost)),
			Ok(None) => {
				self.state.waits.register(self.id, Wait::Read, waiter);
				Poll::Pending
			}
		}
	}
}

#[derive(Debug, Default)]
pub(super) struct Recv {
	state: RecvState,
	pub(super) assembler: Assembler,
	sent_max_stream_data: u64,
	pub(super) end: u64,
	/// The end of the data STREAM frames carried, which on an ordered transport is where the
	/// next one starts. Unlike `end`, a reset's final size does not raise it.
	pub(super) data_end: u64,
	pub(super) reader: Reader,
	/// What a reader leased, which lives as long as the stream does.
	pub(super) lease: Leased,
	/// The credit its reader lets the peer send past what it took, when smaller than the
	/// connection's stream receive window; see [`Stream::set_window`](super::lease).
	window: Option<u64>,
	/// Whether a quiesce asked the peer to stop, which leaves the reader reading
	pub(super) quiesced: bool,
}

/// Whether the application still reads a receive stream
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Reader {
	#[default]
	Reading,
	/// Discarded to free a stuck window, with the reader and the peer yet to hear of it from a
	/// stop; it keeps no data meanwhile, but its lease keeps the credit of bytes the reader took.
	/// `has_gaps` records whether the assembler had missing bytes before its high-water mark
	/// when evicted (e.g. a reordered FIN with missing earlier packets), so `stop()` still sends
	/// `STOP_SENDING` to halt peer retransmissions even when `final_offset` is known.
	Evicted { has_gaps: bool },
	/// The application stopped the stream, which keeps no data
	Stopped,
}

impl Reader {
	pub(super) fn is_evicted(self) -> bool {
		matches!(self, Self::Evicted { .. })
	}
}

impl Recv {
	pub(super) fn new(initial_max_data: u64) -> Box<Self> {
		Box::new(Self {
			state: RecvState::default(),
			assembler: Assembler::new(),
			sent_max_stream_data: initial_max_data,
			end: 0,
			data_end: 0,
			reader: Reader::Reading,
			lease: Leased::default(),
			window: None,
			quiesced: false,
		})
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
		self.data_end = self.data_end.max(end);
		// A reset stream only still delivers the data below its Reliable Size
		if let RecvState::ResetRecvd { reliable_size, .. } = self.state {
			let len = reliable_size.saturating_sub(frame.offset).min(frame.data.len() as u64);
			frame.data.truncate(len as usize);
		}
		// Don't bother storing data or releasing stream-level flow control credit if the stream's
		// already stopped
		if self.reader == Reader::Reading && !frame.data.is_empty() {
			self.assembler
				.insert(frame.offset, frame.data, payload_len)
				.map_err(|_| TransportError::INTERNAL_ERROR("too many gaps in stream buffer"))?;
		}

		Ok((new_bytes, frame.fin && self.reader != Reader::Reading))
	}

	pub(super) fn stop(&mut self) -> Result<(u64, bool), Error> {
		if self.reader != Reader::Reading {
			return Err(Error::Closed);
		}

		let read_credits = self.unread_credit();
		// This may send a spurious STOP_SENDING if we've already received all data, but it's a bit
		// fiddly to distinguish that from the case where we've received a FIN but are missing some
		// data that the peer might still be trying to retransmit, in which case a STOP_SENDING is
		// still useful. A stream read to its end, as a lease leaves it, sends none.
		let stop_sending = self.is_receiving() && !self.is_read();
		self.reader = Reader::Stopped;
		self.assembler.clear();
		Ok((read_credits, stop_sending))
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
	pub(super) fn max_stream_data(&self, stream_receive_window: u64) -> (u64, bool) {
		// Credit runs from what the reader took, not what it leased: a reader that stops taking
		// holds at most its window of bytes it has not taken. One waiting on a unit larger than
		// the window gets room for the unit, which the connection window bounds.
		let taken = self.assembler.bytes_read() - self.lease.untaken();
		let stream_receive_window = self
			.window
			.map_or(stream_receive_window, |window| window.min(stream_receive_window));
		let window = stream_receive_window.max(self.lease.wanted());
		// A limit past a varint cannot be encoded, and a peer can never send that far anyway.
		let limit = |credit: u64| credit.min(VarInt::MAX.into_inner());
		// A window shrunk since the last announcement cannot take back credit already granted.
		let max_stream_data = limit(taken + window).max(self.sent_max_stream_data);

		// Only announce a window update if it's significant enough
		// to make it worthwhile sending a MAX_STREAM_DATA frame.
		// We use here a fraction of the configured stream receive window to make
		// the decision, and accommodate for streams using bigger windows requiring
		// less updates. A fixed size would also work - but it would need to be
		// smaller than `stream_receive_window` in order to make sure the stream
		// does not get stuck.
		// A reader waiting on more than the peer may send gets it however small: nothing it
		// takes in the meantime would add to it.
		let diff = max_stream_data - self.sent_max_stream_data;
		let short = self.sent_max_stream_data < limit(taken + self.lease.wanted());
		// At least a byte, so a tiny window never resends an unchanged limit.
		let threshold = (stream_receive_window / 8).max(1);
		let transmit = self.can_send_flow_control() && (diff >= threshold || short);
		(max_stream_data, transmit)
	}

	/// Let the peer send at most `window` bytes past what the reader took, or the connection's
	/// stream receive window if that is smaller.
	pub(super) fn set_window(&mut self, window: u64) {
		self.window = Some(window);
	}

	/// The connection credit this stream may hold under `stream_receive_window`: the bytes that
	/// arrived and were not released, and those the peer may still send.
	pub(super) fn credit(&self, stream_receive_window: u64) -> u64 {
		let (max_stream_data, _) = self.max_stream_data(stream_receive_window);
		let mut limit = max_stream_data.max(self.sent_max_stream_data);
		if let Some(size) = self.final_offset() {
			limit = limit.min(size);
		}
		match self.reader {
			Reader::Reading => limit.saturating_sub(self.end) + self.unread_credit() + self.lease.held,
			// It keeps no data, and the peer's further bytes return their credit as they arrive.
			Reader::Evicted { .. } | Reader::Stopped => self.lease.held,
		}
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
		self.final_offset_unknown() && self.reader == Reader::Reading
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
/// Bytes read from the stream are not released from the receive window until
/// either [`Self::finalize()`] is called, or this type is dropped. Returning credit wakes
/// the connection's transmit waiter when the peer needs an update.
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
		if recv.reader != Reader::Reading {
			return Err(Error::Closed);
		}
		let recv = entry.remove().unwrap(); // initialized above
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
	/// Pending registers `waiter` until data, a FIN, or a reset arrives. A finished stream whose
	/// leased bytes are not all read stays pending rather than finish without them. Fails with
	/// [`Error::Reset`] once the bytes a reset still delivers are read. Data that arrived stays
	/// readable after the connection is lost, and only then does this fail with
	/// [`Error::ConnectionLost`]. Call [`Self::finalize()`] when done.
	pub fn poll_next(&mut self, waiter: &kio::Waiter, max_length: usize) -> Poll<Result<Option<Chunk>, Error>> {
		let rs = match &mut self.state {
			ChunksState::Readable(rs) => rs,
			ChunksState::Finished => return Poll::Ready(Ok(None)),
			ChunksState::Finalized => panic!("must not call poll_next() after finalize()"),
		};

		if let Some(chunk) = rs.assembler.read(max_length) {
			self.read += chunk.bytes.len() as u64;
			return Poll::Ready(Ok(Some(chunk)));
		}

		match rs.state {
			// The data below the Reliable Size is still in flight.
			RecvState::ResetRecvd { reliable_size, .. } if rs.assembler.unread_before(reliable_size) > 0 => {}
			// The stream stays until `received_reset` reports the code, or a stop.
			RecvState::ResetRecvd { .. } => return Poll::Ready(Err(Error::Reset)),
			// Leased bytes left the assembler unread, so the end is not reached until they are
			// read: freeing the stream now would discard them.
			RecvState::Recv { size }
				if size == Some(rs.end) && rs.assembler.bytes_read() == rs.end && !rs.lease.has_unread() =>
			{
				let ChunksState::Readable(recv) = mem::replace(&mut self.state, ChunksState::Finished) else {
					unreachable!("matched Readable above");
				};
				self.pending.max_data |= self.streams.stream_recv_freed(self.id, recv);
				return Poll::Ready(Ok(None));
			}
			RecvState::Recv { .. } => {}
		}

		// Nothing changes while this `Chunks` borrows the stream, so a retry finds the same.
		if self.streams.waits.lost().is_some() {
			return Poll::Ready(Err(Error::ConnectionLost));
		}
		self.streams.waits.register(self.id, Wait::Read, waiter);
		Poll::Pending
	}

	/// Whether a read reached the stream's end, which frees the stream once this is finalized
	///
	/// Every later [`Stream::read()`] of it fails with [`Error::Closed`].
	pub fn is_finished(&self) -> bool {
		matches!(self.state, ChunksState::Finished)
	}

	/// [`Self::poll_next`] without waiting, for tests and the fuzz target.
	#[cfg(any(test, feature = "fuzz"))]
	pub(crate) fn try_next(&mut self, max_length: usize) -> Poll<Result<Option<Chunk>, Error>> {
		self.poll_next(&kio::Waiter::noop(), max_length)
	}

	/// [`Self::try_next`] when something must be ready, for tests.
	///
	/// # Panics
	/// - when nothing has arrived
	#[cfg(test)]
	pub(crate) fn next(&mut self, max_length: usize) -> Result<Option<Chunk>, Error> {
		match self.try_next(max_length) {
			Poll::Ready(result) => result,
			Poll::Pending => panic!("nothing to read"),
		}
	}

	/// Mark the read data as consumed from the stream.
	///
	/// The number of read bytes will be released from the receive window,
	/// allowing the remote peer to send more data if it was previously blocked.
	///
	pub fn finalize(mut self) {
		self.finalize_inner();
	}

	fn finalize_inner(&mut self) {
		let state = mem::replace(&mut self.state, ChunksState::Finalized);
		if let ChunksState::Finalized = state {
			// Noop on repeated calls
			return;
		}

		// We issue additional stream ID credit after the application is notified that a previously
		// open stream has finished or been reset and we've therefore disposed of its state, as
		// recorded by `stream_freed` calls in `next`.
		let mut should_transmit = self.streams.queue_max_stream_id(self.pending);

		// If the stream hasn't finished, we may need to issue stream-level flow control credit
		if let ChunksState::Readable(rs) = state {
			let (_, max_stream_data) = rs.max_stream_data(self.streams.stream_receive_window);
			should_transmit |= max_stream_data;
			if max_stream_data {
				self.pending.max_stream_data.insert(self.id);
			}
			// Return the stream to storage for future use
			self.streams.recv.insert(self.id, Some(rs));
		}

		// Issue connection-level flow control credit for any data we read regardless of state
		let max_data = self.streams.add_read_credits(self.read);
		self.pending.max_data |= max_data;
		should_transmit |= max_data;
		if should_transmit {
			self.streams.waits.wake_transmit();
		}
	}
}

impl Drop for Chunks<'_> {
	fn drop(&mut self) {
		self.finalize_inner();
	}
}

enum ChunksState {
	Readable(Box<Recv>),
	Finished,
	Finalized,
}

/// Why a read or lease failed
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
	/// The peer abandoned the stream: [`Stream::received_reset()`] gives its code and frees it
	///
	/// The stream keeps its state, and its stream ID, until then or a [`Stream::stop()`].
	#[error("reset by peer")]
	Reset,
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
			Error::Reset | Error::Evicted => io::ErrorKind::ConnectionReset,
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
		assert!(transmit);
		assert_eq!(
			credits,
			INITIAL_OFFSET + INITIAL_BYTES,
			"full connection flow control credit is issued by stop"
		);

		let (max_stream_data, transmit) = s.max_stream_data(RECV_WINDOW);
		assert!(!transmit);
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
		assert!(!transmit);
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
		assert!(!transmit);
		assert_eq!(
			max_stream_data, RECV_WINDOW,
			"stream flow control credit isn't issued after stop"
		);
	}
}
