use std::{collections::hash_map, io};

use bytes::Bytes;
use thiserror::Error;
use tracing::trace;

use super::spaces::{Retransmits, ThinRetransmits};
use crate::{
	Dir, StreamId, VarInt,
	connection::{ConnectionTag, streams::state::get_or_insert_recv},
	frame,
};

mod lease;
pub use lease::{LeaseError, Leased};
mod recv;
use recv::Recv;

#[cfg(test)]
mod saturated;
mod schedule;
pub use recv::{Chunks, ReadError, ReadableError};
use schedule::{Next, Schedule};

mod send;
pub(crate) use send::{ByteSlice, BytesArray};
use send::{BytesSource, Send, SendState};
pub use send::{FinishError, WriteError, Written};

mod state;
pub(crate) use state::StreamsState;

/// Access to streams
pub struct Streams<'a> {
	pub(super) state: &'a mut StreamsState,
	pub(super) conn_state: &'a super::State,
}

impl<'a> Streams<'a> {
	/// Drive a bare `StreamsState` without a connection, for the `streams` fuzz target
	#[cfg(any(test, feature = "fuzz"))]
	pub(crate) fn new(state: &'a mut StreamsState, conn_state: &'a super::State) -> Self {
		Self { state, conn_state }
	}

	/// Open a single stream if possible, its sending half scheduled by `options`
	///
	/// Returns `None` if the streams in the given direction are currently exhausted.
	///
	/// # Panics
	/// - when `options.group` was opened on another connection
	pub fn open(&mut self, dir: Dir, options: SendOptions<'_>) -> Option<StreamId> {
		let group = options.group.map(|g| g.index(self.state.tag));
		if self.conn_state.is_closed() {
			return None;
		}

		if self.state.next[dir as usize] >= self.state.max[dir as usize] {
			self.state.streams_blocked[dir as usize] = true;
			self.state.open_refused[dir as usize] = true;
			return None;
		}

		self.state.next[dir as usize] += 1;
		let id = StreamId::new(self.state.side, dir, self.state.next[dir as usize] - 1);
		self.state.insert_local(id);
		self.state.send_streams += 1;

		// Send state is allocated on first use, so the default schedule needs none.
		if group.is_some() || options.priority != 0 {
			let max_send_data = self.state.max_send_data(id);
			let stream = self.state.send.get_mut(&id).unwrap().insert(Send::new(max_send_data));
			stream.priority = options.priority;
			self.state.pending.join(id, &mut stream.slot, group, stream.priority);
		}
		Some(id)
	}

	/// Accept a remotely initiated stream of a certain directionality, if possible
	///
	/// Returns `None` if there are no new incoming streams for this connection.
	/// Has no impact on the data flow-control or stream concurrency limits.
	pub fn accept(&mut self, dir: Dir) -> Option<StreamId> {
		if self.state.next_remote[dir as usize] == self.state.next_reported_remote[dir as usize] {
			return None;
		}

		let x = self.state.next_reported_remote[dir as usize];
		self.state.next_reported_remote[dir as usize] = x + 1;
		if dir == Dir::Bi {
			self.state.send_streams += 1;
		}

		Some(StreamId::new(!self.state.side, dir, x))
	}

	/// The number of streams that may have unacknowledged data.
	pub fn send_streams(&self) -> usize {
		self.state.send_streams
	}

	/// The number of remotely initiated open streams of a certain directionality.
	///
	/// Includes remotely initiated streams, which have not been accepted via [`accept`](Self::accept).
	/// These streams count against the configured concurrency limit.
	pub fn remote_open_streams(&self, dir: Dir) -> u64 {
		// total opened - total closed = total opened - ( total permitted - total permitted unclosed )
		self.state.next_remote[dir as usize]
			- (self.state.max_remote[dir as usize] - self.state.allocated_remote_count[dir as usize])
	}
}

/// Access to streams
pub struct RecvStream<'a> {
	pub(super) id: StreamId,
	pub(super) state: &'a mut StreamsState,
	pub(super) pending: &'a mut Retransmits,
}

impl RecvStream<'_> {
	/// Read the stream's next contiguous bytes without copying.
	pub fn read(&mut self) -> Result<Chunks<'_>, ReadableError> {
		Chunks::new(self.id, self.state, self.pending)
	}

	/// Stop accepting data on the given receive stream
	///
	/// Discards unread and leased data, returning its credit, and notifies the peer to stop
	/// transmitting. Once stopped, further attempts to operate on a stream will yield
	/// `ClosedStream` errors.
	pub fn stop(&mut self, error_code: VarInt) -> Result<(), ClosedStream> {
		let hash_map::Entry::Occupied(mut entry) = self.state.recv.entry(self.id) else {
			return Err(ClosedStream { _private: () });
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

		Ok(())
	}

	/// Check whether this stream has been reset by the peer, returning the reset error code if so
	///
	/// After returning `Ok(Some(_))` once, stream state will be discarded and all future calls will
	/// return `Err(ClosedStream)`.
	pub fn received_reset(&mut self) -> Result<Option<VarInt>, ClosedStream> {
		let hash_map::Entry::Occupied(entry) = self.state.recv.entry(self.id) else {
			return Err(ClosedStream { _private: () });
		};
		let Some(s) = entry.get().as_ref().and_then(|s| s.as_open_recv()) else {
			return Ok(None);
		};
		if s.stopped {
			return Err(ClosedStream { _private: () });
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

/// Access to streams
pub struct SendStream<'a> {
	pub(super) id: StreamId,
	pub(super) state: &'a mut StreamsState,
	pub(super) pending: &'a mut Retransmits,
	pub(super) conn_state: &'a super::State,
}

impl<'a> SendStream<'a> {
	/// Drive a bare `StreamsState` without a connection, for the `streams` fuzz target
	#[cfg(any(test, feature = "fuzz"))]
	pub(crate) fn new(
		id: StreamId,
		state: &'a mut StreamsState,
		pending: &'a mut Retransmits,
		conn_state: &'a super::State,
	) -> Self {
		Self {
			id,
			state,
			pending,
			conn_state,
		}
	}

	/// Send data on the given stream
	///
	/// Returns the number of bytes successfully written.
	pub fn write(&mut self, data: &[u8]) -> Result<usize, WriteError> {
		Ok(self.write_source(&mut ByteSlice::from_slice(data))?.bytes)
	}

	/// Send data on the given stream
	///
	/// Returns the number of bytes and chunks successfully written.
	/// Note that this method might also write a partial chunk. In this case
	/// [`Written::chunks`] will not count this chunk as fully written. However
	/// the chunk will be advanced and contain only non-written data after the call.
	pub fn write_chunks(&mut self, data: &mut [Bytes]) -> Result<Written, WriteError> {
		self.write_source(&mut BytesArray::from_chunks(data))
	}

	/// How many bytes a write would accept right now
	///
	/// Zero when a write would block or fail. Lets a caller size a chunk before taking it from a
	/// buffer it cannot put bytes back into.
	pub fn write_limit(&self) -> usize {
		if self.conn_state.is_closed() {
			return 0;
		}
		let budget = match self.state.send.get(&self.id) {
			None => return 0,
			Some(None) => self.state.max_send_data(self.id).into_inner(),
			Some(Some(stream)) if !stream.is_writable() || stream.stop_reason.is_some() => {
				return 0;
			}
			Some(Some(stream)) => stream.max_data - stream.offset(),
		};
		usize::try_from(budget.min(self.state.write_limit())).unwrap_or(usize::MAX)
	}

	fn write_source<B: BytesSource>(&mut self, source: &mut B) -> Result<Written, WriteError> {
		if self.conn_state.is_closed() {
			trace!(%self.id, "write blocked; connection draining");
			return Err(WriteError::Blocked);
		}

		let limit = self.state.write_limit();

		let max_send_data = self.state.max_send_data(self.id);

		let stream = self
			.state
			.send
			.get_mut(&self.id)
			.ok_or(WriteError::ClosedStream)?
			.get_or_insert_with(|| Send::new(max_send_data));

		let was_pending = stream.is_pending();
		let written = match stream.write(source, limit) {
			Ok(written) => written,
			Err(WriteError::Blocked) => {
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
				return Err(WriteError::Blocked);
			}
			Err(e) => return Err(e),
		};
		self.state.data_sent += written.bytes as u64;
		self.state.buffered_data += written.bytes as u64;
		trace!(stream = %self.id, "wrote {} bytes", written.bytes);
		if !was_pending {
			self.state.pending.push(self.id, &mut stream.slot, stream.priority);
		}
		Ok(written)
	}

	/// Check if this stream was stopped, get the reason if it was
	pub fn stopped(&self) -> Result<Option<VarInt>, ClosedStream> {
		match self.state.send.get(&self.id).as_ref() {
			Some(Some(s)) => Ok(s.stop_reason),
			Some(None) => Ok(None),
			None => Err(ClosedStream { _private: () }),
		}
	}

	/// Finish a send stream, signalling that no more data will be sent.
	///
	/// If this fails, no [`StreamEvent::Finished`] will be generated.
	///
	/// [`StreamEvent::Finished`]: crate::StreamEvent::Finished
	pub fn finish(&mut self) -> Result<(), FinishError> {
		let max_send_data = self.state.max_send_data(self.id);
		let stream = self
			.state
			.send
			.get_mut(&self.id)
			.ok_or(FinishError::ClosedStream)?
			.get_or_insert_with(|| Send::new(max_send_data));

		let was_pending = stream.is_pending();
		stream.finish()?;
		self.state.connection_blocked.swap_remove(&self.id);
		if !was_pending {
			self.state.pending.push(self.id, &mut stream.slot, stream.priority);
		}

		Ok(())
	}

	/// Abandon transmitting data on a stream
	///
	/// # Panics
	/// - when applied to a receive stream
	pub fn reset(&mut self, error_code: VarInt) -> Result<(), ClosedStream> {
		self.reset_at(0, error_code)
	}

	/// How many bytes were written to the stream so far, the offset the next write starts at
	///
	/// After a reset, the bytes the reset still delivers. Zero before the first write, or once
	/// the stream is gone.
	///
	/// # Panics
	/// - when applied to a receive stream
	pub fn offset(&self) -> u64 {
		match self.state.send.get(&self.id) {
			Some(Some(stream)) => stream.offset(),
			_ => 0,
		}
	}

	/// Whether the peer acknowledged every byte before `offset`
	///
	/// A `false` notes to report a [`StreamEvent::Acked`] once the answer changes, so poll again
	/// then. The stream holds only its lowest waiting offset, so every waiter on it polls again
	/// on that event. Like a [`StreamEvent::Finished`] stream, an acknowledgment only means the
	/// peer's QUIC stack holds the data, not that its application read it.
	///
	/// Fails with [`ClosedStream`] once the peer will never acknowledge it, or no longer reads
	/// it: a reset dropped the data before `offset`, the peer sent STOP_SENDING, or the
	/// connection closed. It also fails once the stream is freed, after its
	/// [`StreamEvent::Finished`], which means every byte was acknowledged.
	///
	/// # Panics
	/// - when applied to a receive stream
	/// - when `offset` is past the data written to a stream that was not reset
	pub fn acked(&mut self, offset: u64) -> Result<bool, ClosedStream> {
		if self.conn_state.is_closed() {
			return Err(ClosedStream { _private: () });
		}
		match self.state.send.get_mut(&self.id) {
			Some(Some(stream)) => stream.acked(offset),
			Some(None) => {
				assert_eq!(offset, 0, "acked past the data written");
				Ok(true)
			}
			None => Err(ClosedStream { _private: () }),
		}
	}

	/// Abandon the stream, but still deliver its first `offset` bytes
	///
	/// Nothing past `offset` is sent or retransmitted. A peer that accepts RESET_STREAM_AT gets
	/// it at once, and reads the data before the error. Otherwise the data keeps being
	/// retransmitted, and a plain RESET_STREAM follows once the peer has acknowledged all of it, a
	/// round trip later. An acknowledgment only means the peer's QUIC stack holds the data:
	/// RFC 9000 lets it discard data its application has not read yet on RESET_STREAM, so this
	/// only reliably delivers data the peer parses on arrival, like a WebTransport stream header.
	/// A peer that asked us to stop sending gets the reset at once, without the data.
	///
	/// Fails with [`ClosedStream`] if the stream was already reset or is gone.
	///
	/// # Panics
	/// - when applied to a receive stream
	/// - when `offset` is past the data written
	pub fn reset_at(&mut self, offset: u64, error_code: VarInt) -> Result<(), ClosedStream> {
		let max_send_data = self.state.max_send_data(self.id);
		let at = self.state.peer_reset_stream_at;
		let stream = self
			.state
			.send
			.get_mut(&self.id)
			.ok_or(ClosedStream { _private: () })?
			.get_or_insert_with(|| Send::new(max_send_data));

		if stream.is_reset() {
			// Redundant reset call
			return Err(ClosedStream { _private: () });
		}
		assert!(offset <= stream.offset(), "reset past the data written");
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

		// Don't reopen an already-closed stream we haven't forgotten yet
		Ok(())
	}

	/// Set the priority within the stream's send group, or session-wide when ungrouped
	///
	/// Higher values go first; equal values share bytes round robin. A queued stream moves at
	/// once, behind the streams already queued at its new priority. With
	/// [`transport::Config::send_fairness`](crate::transport::Config::send_fairness) off, a stream
	/// its group is partway through keeps going first until it runs out of data.
	///
	/// # Panics
	/// - when applied to a receive stream
	pub fn set_priority(&mut self, priority: i64) -> Result<(), ClosedStream> {
		let max_send_data = self.state.max_send_data(self.id);
		let stream = self
			.state
			.send
			.get_mut(&self.id)
			.ok_or(ClosedStream { _private: () })?
			.get_or_insert_with(|| Send::new(max_send_data));

		stream.priority = priority;
		self.state.pending.set_stream_priority(&mut stream.slot, priority);
		Ok(())
	}

	/// Get the priority of a stream
	///
	/// # Panics
	/// - when applied to a receive stream
	pub fn priority(&self) -> Result<i64, ClosedStream> {
		let stream = self.state.send.get(&self.id).ok_or(ClosedStream { _private: () })?;

		Ok(stream.as_ref().map(|s| s.priority).unwrap_or_default())
	}

	/// Move the stream into a send group, or with `None` back to the ungrouped streams
	///
	/// Ungrouped streams are sent before any send group. A queued stream keeps its place.
	///
	/// # Panics
	/// - when applied to a receive stream
	/// - when `group` was opened on another connection
	pub fn set_group(&mut self, group: Option<&SendGroup>) -> Result<(), ClosedStream> {
		let max_send_data = self.state.max_send_data(self.id);
		let stream = self
			.state
			.send
			.get_mut(&self.id)
			.ok_or(ClosedStream { _private: () })?
			.get_or_insert_with(|| Send::new(max_send_data));

		let group = group.map(|g| g.index(self.state.tag));
		self.state
			.pending
			.join(self.id, &mut stream.slot, group, stream.priority);
		Ok(())
	}
}

/// Scheduling of outgoing streams between send groups
///
/// A higher priority group preempts every lower one, and groups of equal priority get an
/// equal share of bytes no matter how many streams each has queued. Streams outside any group
/// go before all of it. A group's datagrams, from [`Datagrams::send`](crate::Datagrams::send),
/// share its bytes in the same order.
pub struct SendGroups<'a> {
	pub(super) state: &'a mut StreamsState,
}

impl SendGroups<'_> {
	/// Open a send group at `priority`
	pub fn open(&mut self, priority: i64) -> SendGroup {
		SendGroup {
			index: self.state.pending.open_group(priority),
			tag: self.state.tag,
		}
	}

	/// Release a send group; its streams keep their place until they finish
	///
	/// # Panics
	/// - when `group` was opened on another connection
	pub fn close(&mut self, group: SendGroup) {
		let index = group.index(self.state.tag);
		self.state.pending.close_group(index);
	}

	/// Change a group's priority, moving any data it has queued along with it
	///
	/// # Panics
	/// - when `group` was opened on another connection
	pub fn set_priority(&mut self, group: &SendGroup, priority: i64) {
		let index = group.index(self.state.tag);
		self.state.pending.set_group_priority(index, priority);
	}

	/// The group's priority
	///
	/// # Panics
	/// - when `group` was opened on another connection
	pub fn priority(&self, group: &SendGroup) -> i64 {
		self.state.pending.group_priority(group.index(self.state.tag))
	}
}

impl StreamsState {
	/// Queue a datagram scheduled by `options`, within `budget` bytes of queued datagrams.
	///
	/// Returns whether it stayed queued rather than being evicted itself.
	///
	/// # Panics
	/// - when `options.group` was opened on another connection
	pub(crate) fn queue_datagram(
		&mut self,
		datagram: frame::Datagram,
		options: SendOptions<'_>,
		budget: usize,
	) -> bool {
		let group = options.group.map(|g| g.index(self.tag));
		let frame = datagram.size(true);
		self.pending
			.push_datagram(group, options.priority, datagram, frame, budget)
	}

	/// Drop queued datagrams with a payload over `max_payload` bytes, returning whether any was.
	pub(crate) fn drop_oversized_datagrams(&mut self, max_payload: usize) -> bool {
		self.pending.retain_datagrams(|datagram| {
			let keep = datagram.data.len() <= max_payload;
			if !keep {
				trace!(
					"dropping {} byte datagram violating {} byte limit",
					datagram.data.len(),
					max_payload
				);
			}
			keep
		})
	}

	/// Frame bytes queued datagrams hold against the send buffer.
	pub(crate) fn datagram_bytes(&self) -> usize {
		self.pending.datagram_bytes()
	}
}

/// How an outgoing stream or datagram is scheduled, like W3C WebTransport's send options
///
/// The default is ungrouped at priority 0. Generic so the drivers can carry their own group
/// handle in the same shape.
#[derive(Debug)]
pub struct SendOptions<'a, G = SendGroup> {
	/// The send group to join, or `None` for the ungrouped streams that go before every group
	pub group: Option<&'a G>,
	/// The priority within the group: higher goes first, and a datagram goes ahead of a
	/// stream of equal priority
	pub priority: i64,
}

impl<G> Default for SendOptions<'_, G> {
	fn default() -> Self {
		Self {
			group: None,
			priority: 0,
		}
	}
}

impl<G> Clone for SendOptions<'_, G> {
	fn clone(&self) -> Self {
		*self
	}
}

impl<G> Copy for SendOptions<'_, G> {}

/// A send group, from [`SendGroups::open`]
///
/// Release it with [`SendGroups::close`] on the connection that opened it.
#[derive(Debug, PartialEq, Eq)]
#[must_use = "a send group lives until it is closed"]
pub struct SendGroup {
	index: usize,
	tag: ConnectionTag,
}

impl SendGroup {
	fn index(&self, tag: ConnectionTag) -> usize {
		assert_eq!(self.tag, tag, "send group used on another connection");
		self.index
	}
}

/// Application events about streams
#[derive(Debug, PartialEq, Eq)]
pub enum StreamEvent {
	/// One or more new streams has been opened and can be accepted
	///
	/// A separate [`StreamEvent::Readable`] is emitted once data arrives.
	Opened {
		/// Directionality for which streams have been opened
		dir: Dir,
	},
	/// A currently open stream likely has data or errors waiting to be read
	Readable {
		/// Which stream is now readable
		id: StreamId,
	},
	/// A formerly write-blocked stream might be ready for a write or have been stopped
	///
	/// Only generated for streams that are currently open.
	Writable {
		/// Which stream is now writable
		id: StreamId,
	},
	/// A finished stream has been fully acknowledged or stopped
	Finished {
		/// Which stream has been finished
		id: StreamId,
	},
	/// The answer changed for the lowest offset a [`SendStream::acked`] call is waiting for
	///
	/// The stream's contiguous acknowledged prefix reached it, or a reset or STOP_SENDING may have
	/// failed any waiter. Every waiter on the stream polls again: one at or below `offset` is
	/// acknowledged, and one past it learns its error or registers again. Reported before a [`StreamEvent::Finished`] from the same ACK.
	/// Not reported when the connection closes, which fails [`SendStream::acked`] instead.
	Acked {
		/// Which stream was acknowledged
		id: StreamId,
		/// The end of the stream's contiguous acknowledged prefix
		offset: u64,
	},
	/// The peer asked us to stop sending on an outgoing stream
	Stopped {
		/// Which stream has been stopped
		id: StreamId,
		/// Error code supplied by the peer
		error_code: VarInt,
	},
	/// At least one new stream of a certain directionality may be opened
	Available {
		/// Directionality for which streams are newly available
		dir: Dir,
	},
}

/// Indicates whether a frame needs to be transmitted
///
/// This type wraps around bool and uses the `#[must_use]` attribute in order
/// to prevent accidental loss of the frame transmission requirement.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
#[must_use = "A frame might need to be enqueued"]
pub struct ShouldTransmit(bool);

impl ShouldTransmit {
	/// Returns whether a frame should be transmitted
	pub fn should_transmit(self) -> bool {
		self.0
	}
}

/// Error indicating that a stream has not been opened or has already been finished or reset
#[derive(Debug, Default, Error, Clone, PartialEq, Eq)]
#[error("closed stream")]
pub struct ClosedStream {
	_private: (),
}

impl From<ClosedStream> for io::Error {
	fn from(x: ClosedStream) -> Self {
		Self::new(io::ErrorKind::NotConnected, x)
	}
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum StreamHalf {
	Send,
	Recv,
}
