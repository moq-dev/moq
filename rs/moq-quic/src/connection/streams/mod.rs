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

mod recv;
use recv::Recv;

#[cfg(test)]
mod saturated;
mod schedule;
pub use recv::{Chunks, Credit, ReadError, ReadableError};
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

#[allow(clippy::needless_lifetimes)] // `new` names the lifetime, under the fuzz harness
impl<'a> Streams<'a> {
    /// Drive a bare `StreamsState` without a connection, for the `streams` fuzz target
    #[cfg(any(test, feature = "fuzz"))]
    pub(crate) fn new(state: &'a mut StreamsState, conn_state: &'a super::State) -> Self {
        Self { state, conn_state }
    }

    /// Open a single stream if possible
    ///
    /// Returns `None` if the streams in the given direction are currently exhausted.
    pub fn open(&mut self, dir: Dir) -> Option<StreamId> {
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
    /// These streams count against the respective concurrency limit reported by
    /// [`Connection::max_concurrent_streams`](super::Connection::max_concurrent_streams).
    pub fn remote_open_streams(&self, dir: Dir) -> u64 {
        // total opened - total closed = total opened - ( total permitted - total permitted unclosed )
        self.state.next_remote[dir as usize]
            - (self.state.max_remote[dir as usize]
                - self.state.allocated_remote_count[dir as usize])
    }
}

/// Access to streams
pub struct RecvStream<'a> {
    pub(super) id: StreamId,
    pub(super) state: &'a mut StreamsState,
    pub(super) pending: &'a mut Retransmits,
}

impl RecvStream<'_> {
    /// Read from the given recv stream
    ///
    /// `max_length` limits the maximum size of the returned `Bytes` value; passing `usize::MAX`
    /// will yield the best performance. `ordered` will make sure the returned chunk's offset will
    /// have an offset exactly equal to the previously returned offset plus the previously returned
    /// bytes' length.
    ///
    /// Yields `Ok(None)` if the stream was finished. Otherwise, yields a segment of data and its
    /// offset in the stream. If `ordered` is `false`, segments may be received in any order, and
    /// the `Chunk`'s `offset` field can be used to determine ordering in the caller.
    ///
    /// While most applications will prefer to consume stream data in order, unordered reads can
    /// improve performance when packet loss occurs and data cannot be retransmitted before the flow
    /// control window is filled. On any given stream, you can switch from ordered to unordered
    /// reads, but ordered reads on streams that have seen previous unordered reads will return
    /// `ReadError::IllegalOrderedRead`.
    pub fn read(&mut self, ordered: bool) -> Result<Chunks<'_>, ReadableError> {
        Chunks::new(self.id, ordered, self.state, self.pending)
    }

    /// Stop accepting data on the given receive stream
    ///
    /// Discards unread data and notifies the peer to stop transmitting. Once stopped, further
    /// attempts to operate on a stream will yield `ClosedStream` errors.
    pub fn stop(&mut self, error_code: VarInt) -> Result<(), ClosedStream> {
        let mut entry = match self.state.recv.entry(self.id) {
            hash_map::Entry::Occupied(s) => s,
            hash_map::Entry::Vacant(_) => return Err(ClosedStream { _private: () }),
        };
        let stream = get_or_insert_recv(entry.get_mut(), self.state.stream_receive_window);

        let (read_credits, stop_sending) = stream.stop()?;
        if stop_sending.should_transmit() {
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
            self.state.stream_recv_freed(self.id, recv);
        }

        if self.state.add_read_credits(read_credits).should_transmit() {
            self.pending.max_data = true;
        }

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
        // application to attempt to read or stop the stream once it knows it's reset
        let (_, recv) = entry.remove_entry();
        self.state
            .stream_recv_freed(self.id, recv.expect("must have recv on reset"));
        self.state.queue_max_stream_id(self.pending);

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

#[allow(clippy::needless_lifetimes)] // `new` names the lifetime, under the fuzz harness
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

        if limit == 0 {
            trace!(
                stream = %self.id, max_data = self.state.max_data, data_sent = self.state.data_sent,
                "write blocked by connection-level flow control or send window"
            );
            if !stream.connection_blocked {
                stream.connection_blocked = true;
                self.state.connection_blocked.push(self.id);
            }
            // Only report blocking on the peer's limit, not on our own send window
            if self.state.data_sent == self.state.max_data
                && self.state.data_blocked_limit != Some(self.state.max_data)
            {
                self.state.data_blocked_limit = Some(self.state.max_data);
                self.pending.data_blocked = true;
            }
            return Err(WriteError::Blocked);
        }

        let was_pending = stream.is_pending();
        let written = match stream.write(source, limit) {
            Ok(written) => written,
            Err(WriteError::Blocked) => {
                if stream.data_blocked_limit != Some(stream.max_data) {
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
            self.state
                .pending
                .push(self.id, &mut stream.slot, stream.priority);
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
        if !was_pending {
            self.state
                .pending
                .push(self.id, &mut stream.slot, stream.priority);
        }

        Ok(())
    }

    /// Abandon transmitting data on a stream
    ///
    /// # Panics
    /// - when applied to a receive stream
    pub fn reset(&mut self, error_code: VarInt) -> Result<(), ClosedStream> {
        self.reset_to(error_code, 0)
    }

    /// Mark the data written so far, so a later [`Checkpoint::reset`] still delivers it
    ///
    /// # Panics
    /// - when applied to a receive stream
    pub fn checkpoint(&self) -> Checkpoint {
        let offset = match self.state.send.get(&self.id) {
            Some(Some(stream)) => stream.offset(),
            // Not written yet, or already closed, where the reset fails anyway
            _ => 0,
        };
        Checkpoint {
            id: self.id,
            tag: self.state.tag,
            generation: self.state.generation,
            offset,
        }
    }

    /// Reset the stream, still delivering its first `reliable_size` bytes
    fn reset_to(&mut self, error_code: VarInt, reliable_size: u64) -> Result<(), ClosedStream> {
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
        // A checkpoint names its stream's connection and generation, so it never runs past the data
        debug_assert!(reliable_size <= stream.offset());
        // The peer that sent STOP_SENDING will not read the prefix
        let reliable_size = match stream.stop_reason {
            Some(_) => 0,
            None => reliable_size,
        };

        // Restore the portion of the send window consumed by the data that we aren't about to
        // send. We leave flow control alone because the peer's responsible for issuing additional
        // credit based on the final offset communicated in the RESET_STREAM frame we send.
        let buffered = stream.pending.buffered();
        let send = stream.reset(error_code, reliable_size, at);
        self.state.buffered_data -= buffered - stream.pending.buffered();
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
    /// Higher values go first; equal values share bytes round robin. A queued stream moves to
    /// its new priority the next time it is requeued, which is after every frame when
    /// [`TransportConfig::send_fairness`](crate::TransportConfig::send_fairness) is on.
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
        Ok(())
    }

    /// Get the priority of a stream
    ///
    /// # Panics
    /// - when applied to a receive stream
    pub fn priority(&self) -> Result<i64, ClosedStream> {
        let stream = self
            .state
            .send
            .get(&self.id)
            .ok_or(ClosedStream { _private: () })?;

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

/// The data written to a send stream up to some point, which a reset can still deliver
///
/// Taken with [`SendStream::checkpoint`], and only valid on the connection it came from.
#[derive(Debug)]
pub struct Checkpoint {
    id: StreamId,
    tag: ConnectionTag,
    generation: u64,
    offset: u64,
}

impl Checkpoint {
    /// Abandon the stream, but still deliver everything written before the checkpoint
    ///
    /// Nothing past the checkpoint is sent or retransmitted. A peer that accepts RESET_STREAM_AT
    /// gets it at once, and reads the data before the error. Otherwise the data keeps being
    /// retransmitted, and a plain RESET_STREAM follows once the peer has acknowledged all of it, a
    /// round trip later. An acknowledgment only means the peer's QUIC stack holds the data:
    /// RFC 9000 lets it discard data its application has not read yet on RESET_STREAM, so this
    /// only reliably delivers data the peer parses on arrival, like a WebTransport stream header.
    /// A peer that asked us to stop sending gets the reset at once, without the data.
    ///
    /// Fails with [`ClosedStream`] if the stream was already reset or is gone, including one
    /// rejected 0-RTT discarded, even if a new stream has since taken its id.
    ///
    /// # Panics
    /// If the checkpoint came from another connection.
    pub fn reset(
        self,
        conn: &mut crate::Connection,
        error_code: VarInt,
    ) -> Result<(), ClosedStream> {
        let mut stream = conn.send_stream(self.id);
        assert!(
            self.tag == stream.state.tag,
            "checkpoint used on another connection"
        );
        // Rejected 0-RTT discarded the stream, and a new one may have its id
        if self.generation != stream.state.generation {
            return Err(ClosedStream { _private: () });
        }
        stream.reset_to(error_code, self.offset)
    }
}

/// Scheduling of outgoing streams between send groups
///
/// A send group shares bytes with the other groups of its domain: a higher priority group
/// preempts every lower one, and groups of equal priority get an equal share of bytes no
/// matter how many streams each has queued. Domains share bytes equally whatever their
/// groups' priorities. Streams outside any group go before all of it. A group's datagrams,
/// from [`Datagrams::send`](crate::Datagrams::send), share its bytes in the same order.
pub struct SendGroups<'a> {
    pub(super) state: &'a mut StreamsState,
}

impl SendGroups<'_> {
    /// Open a send group at `priority` in `domain`, or in the default domain
    ///
    /// # Panics
    /// - when `domain` was opened on another connection
    pub fn open(&mut self, priority: i64, domain: Option<&SendDomain>) -> SendGroup {
        let tag = self.state.tag;
        let domain = domain.map(|d| d.index(tag));
        SendGroup {
            index: self.state.pending.open_group(priority, domain),
            tag,
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
        self.state
            .pending
            .group_priority(group.index(self.state.tag))
    }

    /// Open a domain, which gets an equal share of bytes with every other domain
    pub fn open_domain(&mut self) -> SendDomain {
        SendDomain {
            index: self.state.pending.open_domain(),
            tag: self.state.tag,
        }
    }

    /// Release a domain; its groups keep their share until they are released
    ///
    /// # Panics
    /// - when `domain` was opened on another connection
    pub fn close_domain(&mut self, domain: SendDomain) {
        let index = domain.index(self.state.tag);
        self.state.pending.close_domain(index);
    }
}

impl StreamsState {
    /// Queue a datagram in `group` at `order`, within `budget` bytes of queued datagrams.
    ///
    /// Returns whether it stayed queued rather than being evicted itself.
    ///
    /// # Panics
    /// - when `group` was opened on another connection
    pub(crate) fn queue_datagram(
        &mut self,
        datagram: frame::Datagram,
        group: Option<&SendGroup>,
        order: i64,
        budget: usize,
    ) -> bool {
        let group = group.map(|g| g.index(self.tag));
        let frame = datagram.size(true);
        self.pending
            .push_datagram(group, order, datagram, frame, budget)
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

/// A domain of send groups, from [`SendGroups::open_domain`]
///
/// Release it with [`SendGroups::close_domain`] on the connection that opened it.
#[derive(Debug, PartialEq, Eq)]
#[must_use = "a domain lives until it is closed"]
pub struct SendDomain {
    index: usize,
    tag: ConnectionTag,
}

impl SendDomain {
    fn index(&self, tag: ConnectionTag) -> usize {
        assert_eq!(self.tag, tag, "send domain used on another connection");
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
