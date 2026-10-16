use std::{
    collections::{VecDeque, hash_map},
    convert::TryFrom,
    mem,
};

use bytes::BufMut;
use rustc_hash::FxHashMap;
use tracing::{debug, trace};

use super::{
    Credit, PendingStreamsQueue, Recv, Retransmits, Send, SendState, ShouldTransmit, StreamEvent,
    StreamHalf, ThinRetransmits,
};
use crate::{
    Dir, MAX_STREAM_COUNT, Side, StreamId, TransportError, VarInt,
    coding::BufMutExt,
    connection::ConnectionTag,
    connection::stats::{CreditStats, FrameStats},
    frame::{self, FrameStruct, StreamMetaVec},
    transport_parameters::TransportParameters,
};

/// Wrapper around `Recv` that facilitates reusing `Recv` instances
#[derive(Debug)]
pub(super) enum StreamRecv {
    /// A `Recv` that is ready to be opened
    Free(Box<Recv>),
    /// A `Recv` that has been opened
    Open(Box<Recv>),
}

impl StreamRecv {
    /// Returns a reference to the inner `Recv` if the stream is open
    pub(super) fn as_open_recv(&self) -> Option<&Recv> {
        match self {
            Self::Open(r) => Some(r),
            _ => None,
        }
    }

    // Returns a mutable reference to the inner `Recv` if the stream is open
    pub(super) fn as_open_recv_mut(&mut self) -> Option<&mut Recv> {
        match self {
            Self::Open(r) => Some(r),
            _ => None,
        }
    }

    // Returns the inner `Recv`
    pub(super) fn into_inner(self) -> Box<Recv> {
        match self {
            Self::Free(r) | Self::Open(r) => r,
        }
    }

    // Reinitialize the stream so the inner `Recv` can be reused
    pub(super) fn free(self, initial_max_data: u64) -> Self {
        match self {
            Self::Free(_) => unreachable!("Self::Free on reinit()"),
            Self::Open(mut recv) => {
                recv.reinit(initial_max_data);
                Self::Free(recv)
            }
        }
    }
}

#[allow(unreachable_pub)] // fuzzing only
pub struct StreamsState {
    pub(super) side: Side,
    // Set of streams that are currently open, or could be immediately opened by the peer
    pub(super) send: FxHashMap<StreamId, Option<Box<Send>>>,
    pub(super) recv: FxHashMap<StreamId, Option<StreamRecv>>,
    pub(super) free_recv: Vec<StreamRecv>,
    pub(super) next: [u64; 2],
    /// Maximum number of locally-initiated streams that may be opened over the lifetime of the
    /// connection so far, per direction
    pub(super) max: [u64; 2],
    /// Maximum number of remotely-initiated streams that may be opened over the lifetime of the
    /// connection so far, per direction
    pub(super) max_remote: [u64; 2],
    /// Value of `max_remote` most recently transmitted to the peer in a `MAX_STREAMS` frame
    sent_max_remote: [u64; 2],
    /// Number of streams that we've given the peer permission to open and which aren't fully closed
    pub(super) allocated_remote_count: [u64; 2],
    /// Size of the desired stream flow control window. May be smaller than `allocated_remote_count`
    /// due to `set_max_concurrent` calls.
    max_concurrent_remote_count: [u64; 2],
    /// Whether `max_concurrent_remote_count` has ever changed
    flow_control_adjusted: bool,
    /// Lowest remotely-initiated stream index that haven't actually been opened by the peer
    pub(super) next_remote: [u64; 2],
    /// Whether the remote endpoint has opened any streams the application doesn't know about yet,
    /// per directionality
    opened: [bool; 2],
    // Next to report to the application, once opened
    pub(super) next_reported_remote: [u64; 2],
    /// Number of outbound streams
    ///
    /// This differs from `self.send.len()` in that it does not include streams that the peer is
    /// permitted to open but which have not yet been opened.
    pub(super) send_streams: usize,
    /// Streams with outgoing data queued, sorted by priority
    pub(super) pending: PendingStreamsQueue,

    events: VecDeque<StreamEvent>,
    /// Streams blocked on connection-level flow control or stream window space
    ///
    /// Streams are only added to this list when a write fails.
    pub(super) connection_blocked: Vec<StreamId>,
    /// Connection-level flow control budget dictated by the peer
    pub(super) max_data: u64,
    /// The initial receive window
    receive_window: u64,
    /// Limit on incoming data, which is transmitted through `MAX_DATA` frames
    local_max_data: u64,
    /// The last value of `MAX_DATA` which had been queued for transmission in
    /// an outgoing `MAX_DATA` frame
    sent_max_data: VarInt,
    /// Sum of current offsets of all send streams.
    pub(super) data_sent: u64,
    /// Sum of end offsets of all receive streams. Includes gaps, so it's an upper bound.
    data_recvd: u64,
    /// Bytes read with [`Chunks::hold`](super::Chunks::hold) whose connection credit the
    /// application has not released yet
    pub(super) held: u64,
    /// Which connection [`Credit`]s handed out here belong to
    pub(super) tag: ConnectionTag,
    /// Total quantity of outgoing data retained in send buffers, including acknowledged data
    pub(super) buffered_data: u64,
    /// Configured upper bound for `buffered_data`.
    ///
    /// Note this may be less than `buffered_data` if the user has set a new value.
    pub(super) send_window: u64,
    /// Configured upper bound for how much unacked data the peer can send us per stream
    pub(super) stream_receive_window: u64,

    // Pertinent state from the TransportParameters supplied by the peer
    initial_max_stream_data_uni: VarInt,
    initial_max_stream_data_bidi_local: VarInt,
    initial_max_stream_data_bidi_remote: VarInt,
    /// Whether the peer accepts RESET_STREAM_AT
    pub(super) peer_reset_stream_at: bool,
    /// Bumped when rejected 0-RTT discards our streams and their ids are opened again, so a
    /// [`Checkpoint`](super::Checkpoint) from before cannot reach the new stream of the same id
    pub(super) generation: u64,

    /// The shrink to be applied to local_max_data when receive_window is shrunk
    receive_window_shrink_debt: u64,
    /// Whether the locally-initiated stream limit has been hit, per direction
    pub(super) streams_blocked: [bool; 2],
    /// Value of `max_data` for which a `DATA_BLOCKED` frame was most recently queued
    ///
    /// A new frame is only queued once the peer has raised the limit.
    pub(super) data_blocked_limit: Option<u64>,
}

impl StreamsState {
    #[allow(unreachable_pub)] // fuzzing only
    pub fn new(
        side: Side,
        max_remote_uni: VarInt,
        max_remote_bi: VarInt,
        send_window: u64,
        receive_window: VarInt,
        stream_receive_window: VarInt,
    ) -> Self {
        Self {
            side,
            send: FxHashMap::default(),
            recv: FxHashMap::default(),
            free_recv: Vec::new(),
            next: [0, 0],
            max: [0, 0],
            max_remote: [max_remote_bi.into(), max_remote_uni.into()],
            sent_max_remote: [max_remote_bi.into(), max_remote_uni.into()],
            allocated_remote_count: [max_remote_bi.into(), max_remote_uni.into()],
            max_concurrent_remote_count: [max_remote_bi.into(), max_remote_uni.into()],
            flow_control_adjusted: false,
            next_remote: [0, 0],
            opened: [false, false],
            next_reported_remote: [0, 0],
            send_streams: 0,
            pending: PendingStreamsQueue::new(),
            events: VecDeque::new(),
            connection_blocked: Vec::new(),
            max_data: 0,
            receive_window: receive_window.into(),
            local_max_data: receive_window.into(),
            sent_max_data: receive_window,
            data_sent: 0,
            data_recvd: 0,
            held: 0,
            tag: ConnectionTag::next(),
            buffered_data: 0,
            send_window,
            stream_receive_window: stream_receive_window.into(),
            initial_max_stream_data_uni: 0u32.into(),
            initial_max_stream_data_bidi_local: 0u32.into(),
            initial_max_stream_data_bidi_remote: 0u32.into(),
            peer_reset_stream_at: false,
            generation: 0,
            receive_window_shrink_debt: 0,
            streams_blocked: [false, false],
            data_blocked_limit: None,
        }
    }

    pub(crate) fn set_params(&mut self, params: &TransportParameters) {
        self.initial_max_stream_data_uni = params.initial_max_stream_data_uni;
        self.initial_max_stream_data_bidi_local = params.initial_max_stream_data_bidi_local;
        self.initial_max_stream_data_bidi_remote = params.initial_max_stream_data_bidi_remote;
        self.peer_reset_stream_at = params.reset_stream_at;
        self.max[Dir::Bi as usize] = params.initial_max_streams_bidi.into();
        self.max[Dir::Uni as usize] = params.initial_max_streams_uni.into();
        self.received_max_data(params.initial_max_data);
        for (&id, slot) in self.send.iter_mut() {
            if id.initiator() != self.side
                && id.dir() == Dir::Bi
                && let Some(s) = slot.as_mut()
            {
                s.max_data = params.initial_max_stream_data_bidi_local.into();
            }
        }
    }

    /// Ensure we have space for at least a full flow control window of remotely-initiated streams
    /// to be open, and notify the peer if the window has moved
    fn ensure_remote_streams(&mut self, dir: Dir) {
        let new_count = self.max_concurrent_remote_count[dir as usize]
            .saturating_sub(self.allocated_remote_count[dir as usize]);
        self.allocated_remote_count[dir as usize] += new_count;
        self.max_remote[dir as usize] += new_count;
    }

    pub(crate) fn zero_rtt_rejected(&mut self) {
        // Revert to initial state for outgoing streams
        for dir in Dir::iter() {
            for i in 0..self.next[dir as usize] {
                // We don't bother calling `stream_freed` here because we explicitly reset affected
                // counters below.
                let id = StreamId::new(self.side, dir, i);
                self.send.remove(&id).unwrap();
                if let Dir::Bi = dir {
                    self.recv.remove(&id).unwrap();
                }
            }
            self.next[dir as usize] = 0;

            // If 0-RTT was rejected, any flow control frames we sent were lost.
            if self.flow_control_adjusted {
                // Conservative approximation of whatever we sent in transport parameters
                self.sent_max_remote[dir as usize] = 0;
            }
        }

        self.generation += 1;
        self.pending.clear();
        self.send_streams = 0;
        self.data_sent = 0;
        self.buffered_data = 0;
        self.connection_blocked.clear();
        self.data_blocked_limit = None;
    }

    /// Process incoming stream frame
    ///
    /// If successful, returns whether a `MAX_DATA` frame needs to be transmitted
    pub(crate) fn received(
        &mut self,
        frame: frame::Stream,
        payload_len: usize,
    ) -> Result<ShouldTransmit, TransportError> {
        let id = frame.id;
        self.validate_receive_id(id).inspect_err(|_| {
            debug!("received illegal STREAM frame");
        })?;

        self.insert_remote(id);

        let Some(rs) = self
            .recv
            .get_mut(&id)
            .map(|opt| get_or_insert_recv(opt, self.stream_receive_window))
        else {
            trace!("dropping frame for closed stream");
            return Ok(ShouldTransmit(false));
        };

        if !rs.is_receiving() {
            trace!("dropping frame for finished stream");
            return Ok(ShouldTransmit(false));
        }

        let (new_bytes, closed) =
            rs.ingest(frame, payload_len, self.data_recvd, self.local_max_data)?;
        self.data_recvd = self.data_recvd.saturating_add(new_bytes);

        if !rs.stopped {
            self.events.push_back(StreamEvent::Readable { id });
            // Credit released while the peer still had room may not have been worth a frame
            // then; it is now that the peer is about to block.
            return Ok(ShouldTransmit(self.max_data_due()));
        }

        // Stopped streams become closed instantly on FIN, so check whether we need to clean up
        if closed {
            let rs = self.recv.remove(&id).flatten().unwrap();
            self.stream_recv_freed(id, rs);
        }

        // We don't buffer data on stopped streams, so issue flow control credit immediately
        Ok(self.add_read_credits(new_bytes))
    }

    /// Process incoming RESET_STREAM frame
    ///
    /// If successful, returns whether a `MAX_DATA` frame needs to be transmitted
    #[allow(unreachable_pub)] // fuzzing only
    pub fn received_reset(
        &mut self,
        frame: frame::ResetStream,
    ) -> Result<ShouldTransmit, TransportError> {
        let frame::ResetStream {
            id,
            error_code,
            final_offset,
            reliable_size,
        } = frame;
        if reliable_size > Some(final_offset) {
            return Err(TransportError::FRAME_ENCODING_ERROR(
                "reliable size exceeds final size",
            ));
        }
        self.validate_receive_id(id).inspect_err(|_| {
            debug!("received illegal RESET_STREAM frame");
        })?;

        self.insert_remote(id);

        let Some(rs) = self
            .recv
            .get_mut(&id)
            .map(|opt| get_or_insert_recv(opt, self.stream_receive_window))
        else {
            trace!("received RESET_STREAM on closed stream");
            return Ok(ShouldTransmit(false));
        };

        // State transition
        let reliable_size = reliable_size.map_or(0, u64::from);
        let Some((new_bytes, freed)) = rs.reset(
            error_code,
            final_offset,
            reliable_size,
            self.data_recvd,
            self.local_max_data,
        )?
        else {
            // Redundant reset
            return Ok(ShouldTransmit(false));
        };
        self.data_recvd = self.data_recvd.saturating_add(new_bytes);
        let credit = if rs.stopped {
            // Stopped streams should be disposed immediately on reset, and have already returned
            // read credits up to their highest offset.
            let rs = self.recv.remove(&id).flatten().unwrap();
            self.stream_recv_freed(id, rs);
            new_bytes
        } else {
            self.events.push_back(StreamEvent::Readable { id });
            freed
        };

        // Update connection-level flow control
        Ok(match credit {
            0 => ShouldTransmit(false),
            credit => self.add_read_credits(credit),
        })
    }

    /// Process incoming `STOP_SENDING` frame
    ///
    /// Returns the Reliable Size of 0 for a reset to send now, since the peer will not read the
    /// data a reset still delivers.
    #[allow(unreachable_pub)] // fuzzing only
    pub fn received_stop_sending(&mut self, id: StreamId, error_code: VarInt) -> Option<u64> {
        self.insert_remote(id);

        let max_send_data = self.max_send_data(id);
        let stream = self
            .send
            .get_mut(&id)?
            .get_or_insert_with(|| Send::new(max_send_data));

        if stream.try_stop(error_code) {
            self.events
                .push_back(StreamEvent::Stopped { id, error_code });
        }
        let buffered = stream.pending.buffered();
        let reset = stream.abandon_reliable();
        self.buffered_data -= buffered - stream.pending.buffered();
        reset
    }

    /// Handle acknowledgement of a reset frame sent while the Reliable Size was `reliable_size`
    pub(crate) fn reset_acked(&mut self, id: StreamId, reliable_size: u64) {
        let hash_map::Entry::Occupied(mut e) = self.send.entry(id) else {
            return;
        };
        // A reliable reset lingers until the data below its Reliable Size is acknowledged too, and
        // one since shrunk by STOP_SENDING until its replacement is
        if e.get_mut()
            .as_mut()
            .is_some_and(|s| s.reset_acked(reliable_size))
        {
            e.remove_entry();
            self.stream_freed(id, StreamHalf::Send);
        }
    }

    /// Whether any stream data is queued, regardless of control frames
    pub(crate) fn can_send_stream_data(&self) -> bool {
        // Reset streams may linger in the pending stream list, but only produce stream frames
        // for data below their Reliable Size
        self.pending.iter().any(|stream| {
            self.send
                .get(&stream.id)
                .and_then(|s| s.as_ref())
                .is_some_and(|s| !s.is_reset() || s.is_pending())
        })
    }

    /// Whether MAX_STREAM_DATA frames could be sent for stream `id`
    pub(crate) fn can_send_flow_control(&self, id: StreamId) -> bool {
        self.recv
            .get(&id)
            .and_then(|s| s.as_ref())
            .and_then(|s| s.as_open_recv())
            .is_some_and(|s| s.can_send_flow_control())
    }

    /// Whether a `DATA_BLOCKED` frame could be sent
    pub(crate) fn can_send_data_blocked(&self) -> bool {
        self.data_blocked_limit == Some(self.max_data)
    }

    /// Limit to send in a `STREAM_DATA_BLOCKED` frame for stream `id`, if one could be sent
    ///
    /// `None` if the stream is no longer writable, was stopped by the peer, or the peer has raised
    /// the limit since the frame was queued.
    pub(crate) fn stream_data_blocked_limit(&self, id: StreamId) -> Option<u64> {
        let stream = self.send.get(&id)?.as_ref()?;
        stream.data_blocked_limit.filter(|&limit| {
            stream.is_writable() && stream.stop_reason.is_none() && limit == stream.max_data
        })
    }

    pub(in crate::connection) fn write_control_frames(
        &mut self,
        buf: &mut Vec<u8>,
        pending: &mut Retransmits,
        retransmits: &mut ThinRetransmits,
        stats: &mut FrameStats,
        max_size: usize,
    ) {
        // RESET_STREAM
        let at = self.peer_reset_stream_at;
        while buf.len() + frame::ResetStream::SIZE_BOUND < max_size {
            let Some((id, _)) = pending.reset_stream.pop() else {
                break;
            };
            let Some(stream) = self.send.get(&id).and_then(|s| s.as_ref()) else {
                continue;
            };
            let (Some(frame), Some(reliable_size)) =
                (stream.reset_frame(id, at), stream.reliable_size())
            else {
                continue;
            };
            trace!(stream = %id, reliable_size = ?frame.reliable_size, "RESET_STREAM");
            retransmits
                .get_or_create()
                .reset_stream
                .push((id, reliable_size));
            frame.encode(buf);
            stats.reset_stream += 1;
        }

        // STOP_SENDING
        while buf.len() + frame::StopSending::SIZE_BOUND < max_size {
            let Some(frame) = pending.stop_sending.pop() else {
                break;
            };
            // We may need to transmit STOP_SENDING even for streams whose state we have discarded,
            // because we are able to discard local state for stopped streams immediately upon
            // receiving FIN, even if the peer still has arbitrarily large amounts of data to
            // (re)transmit due to loss or unconventional sending strategy. We could fine-tune this
            // a little by dropping the frame if we specifically know the stream's been reset by the
            // peer, but we discard that information as soon as the application consumes it, so it
            // can't be relied upon regardless.
            trace!(stream = %frame.id, "STOP_SENDING");
            frame.encode(buf);
            retransmits.get_or_create().stop_sending.push(frame);
            stats.stop_sending += 1;
        }

        // MAX_DATA
        if pending.max_data && buf.len() + 9 < max_size {
            pending.max_data = false;

            // `local_max_data` can grow bigger than `VarInt`.
            // For transmission inside QUIC frames we need to clamp it to the
            // maximum allowed `VarInt` size.
            let max = VarInt::try_from(self.local_max_data).unwrap_or(VarInt::MAX);

            trace!(value = max.into_inner(), "MAX_DATA");
            if max > self.sent_max_data {
                // Record that a `MAX_DATA` announcing a certain window was sent. This will
                // suppress enqueuing further `MAX_DATA` frames unless either the previous
                // transmission was not acknowledged or the window further increased.
                self.sent_max_data = max;
            }

            retransmits.get_or_create().max_data = true;
            buf.write(frame::FrameType::MAX_DATA);
            buf.write(max);
            stats.max_data += 1;
        }

        // MAX_STREAM_DATA
        while buf.len() + 17 < max_size {
            let Some(&id) = pending.max_stream_data.iter().next() else {
                break;
            };
            pending.max_stream_data.remove(&id);
            let Some(rs) = self
                .recv
                .get_mut(&id)
                .and_then(|s| s.as_mut())
                .and_then(|s| s.as_open_recv_mut())
            else {
                continue;
            };
            if !rs.can_send_flow_control() {
                continue;
            }
            retransmits.get_or_create().max_stream_data.insert(id);

            let (max, _) = rs.max_stream_data(self.stream_receive_window);
            rs.record_sent_max_stream_data(max);

            trace!(stream = %id, max = max, "MAX_STREAM_DATA");
            buf.write(frame::FrameType::MAX_STREAM_DATA);
            buf.write(id);
            buf.write_var(max);
            stats.max_stream_data += 1;
        }

        // MAX_STREAMS
        for dir in Dir::iter() {
            if !pending.max_stream_id[dir as usize] || buf.len() + 9 >= max_size {
                continue;
            }

            pending.max_stream_id[dir as usize] = false;
            retransmits.get_or_create().max_stream_id[dir as usize] = true;
            self.sent_max_remote[dir as usize] = self.max_remote[dir as usize];
            trace!(
                value = self.max_remote[dir as usize],
                "MAX_STREAMS ({:?})", dir
            );
            buf.write(match dir {
                Dir::Uni => frame::FrameType::MAX_STREAMS_UNI,
                Dir::Bi => frame::FrameType::MAX_STREAMS_BIDI,
            });
            buf.write_var(self.max_remote[dir as usize]);
            match dir {
                Dir::Uni => stats.max_streams_uni += 1,
                Dir::Bi => stats.max_streams_bidi += 1,
            }
        }

        // STREAMS_BLOCKED
        for dir in Dir::iter() {
            if self.streams_blocked[dir as usize] {
                pending.streams_blocked[dir as usize] = true;
                self.streams_blocked[dir as usize] = false;
            }

            if !pending.streams_blocked[dir as usize] || buf.len() + 9 >= max_size {
                continue;
            }

            pending.streams_blocked[dir as usize] = false;
            retransmits.get_or_create().streams_blocked[dir as usize] = true;
            let limit = self.max[dir as usize];
            trace!(limit, "STREAMS_BLOCKED ({:?})", dir);
            buf.write(match dir {
                Dir::Uni => frame::FrameType::STREAMS_BLOCKED_UNI,
                Dir::Bi => frame::FrameType::STREAMS_BLOCKED_BIDI,
            });
            buf.write_var(limit);
            match dir {
                Dir::Uni => stats.streams_blocked_uni += 1,
                Dir::Bi => stats.streams_blocked_bidi += 1,
            }
        }

        // DATA_BLOCKED
        if pending.data_blocked && buf.len() + 9 < max_size {
            pending.data_blocked = false;
            // The peer may have raised the limit since the frame was queued
            if self.can_send_data_blocked() {
                retransmits.get_or_create().data_blocked = true;
                trace!(limit = self.max_data, "DATA_BLOCKED");
                buf.write(frame::FrameType::DATA_BLOCKED);
                buf.write_var(self.max_data);
                stats.data_blocked += 1;
            }
        }

        // STREAM_DATA_BLOCKED
        while buf.len() + 17 < max_size {
            let Some(&id) = pending.stream_data_blocked.iter().next() else {
                break;
            };
            pending.stream_data_blocked.remove(&id);
            let Some(limit) = self.stream_data_blocked_limit(id) else {
                continue;
            };
            retransmits.get_or_create().stream_data_blocked.insert(id);

            trace!(stream = %id, limit, "STREAM_DATA_BLOCKED");
            buf.write(frame::FrameType::STREAM_DATA_BLOCKED);
            buf.write(id);
            buf.write_var(limit);
            stats.stream_data_blocked += 1;
        }
    }

    pub(crate) fn write_stream_frames(
        &mut self,
        buf: &mut Vec<u8>,
        max_buf_size: usize,
        fair: bool,
    ) -> StreamMetaVec {
        let mut stream_frames = StreamMetaVec::new();
        while buf.len() + frame::Stream::SIZE_BOUND < max_buf_size {
            if max_buf_size
                .checked_sub(buf.len() + frame::Stream::SIZE_BOUND)
                .is_none()
            {
                break;
            }

            // Pop the stream of the highest priority that currently has pending data
            // If the stream still has some pending data left after writing, it will be reinserted, otherwise not
            let Some(stream) = self.pending.pop() else {
                break;
            };

            let id = stream.id;

            let Some(stream) = self.send.get_mut(&id).and_then(|s| s.as_mut()) else {
                // Stream was reset with pending data and the reset was acknowledged
                continue;
            };

            // Reset streams aren't removed from the pending list and still exist while the peer
            // hasn't acknowledged the reset, but only generate STREAM frames for data below their
            // Reliable Size, so we need to check for them explicitly.
            if stream.is_reset() && !stream.is_pending() {
                continue;
            }

            // Now that we know the `StreamId`, we can better account for how many bytes
            // are required to encode it.
            let max_buf_size = max_buf_size - buf.len() - 1 - VarInt::size(id.into());
            let (offsets, encode_length) = stream.pending.poll_transmit(max_buf_size);
            let fin = offsets.end == stream.pending.offset()
                && matches!(stream.state, SendState::DataSent { .. });
            if fin {
                stream.fin_pending = false;
            }

            if stream.is_pending() {
                // If the stream still has pending data, reinsert it, possibly with an updated priority value
                // Fairness with other streams is achieved by implementing round-robin scheduling,
                // so that the other streams will have a chance to write data
                // before we touch this stream again.
                if fair {
                    self.pending.push_pending(id, stream.priority);
                } else {
                    self.pending.reinsert_pending(id, stream.priority);
                }
            }

            let meta = frame::StreamMeta { id, offsets, fin };
            trace!(id = %meta.id, off = meta.offsets.start, len = meta.offsets.end - meta.offsets.start, fin = meta.fin, "STREAM");
            meta.encode(encode_length, buf);

            // The range might not be retrievable in a single `get` if it is
            // stored in noncontiguous fashion. Therefore this loop iterates
            // until the range is fully copied into the frame.
            let mut offsets = meta.offsets.clone();
            while offsets.start != offsets.end {
                let data = stream.pending.get(offsets.clone());
                offsets.start += data.len() as u64;
                buf.put_slice(data);
            }
            stream_frames.push(meta);
        }

        stream_frames
    }

    /// Process the acknowledgement of stream data
    ///
    /// Returns the Reliable Size of a held reset to send now, once the peer holds all the data it
    /// waited on.
    pub(crate) fn received_ack_of(&mut self, frame: frame::StreamMeta) -> Option<u64> {
        let mut entry = match self.send.entry(frame.id) {
            hash_map::Entry::Vacant(_) => return None,
            hash_map::Entry::Occupied(e) => e,
        };

        let Some(stream) = entry.get_mut().as_mut() else {
            // Because we only call this after sending data on this stream,
            // this closure should be unreachable. If we did somehow screw that up,
            // then we might hit an underflow below with unpredictable effects down
            // the line. Best to short-circuit.
            return None;
        };

        let id = frame.id;
        let buffered = stream.pending.buffered();
        let finished = stream.ack(frame);
        self.buffered_data -= buffered - stream.pending.buffered();
        if !finished {
            // The stream is unfinished or may still need retransmits
            return stream.take_held_reset();
        }

        // A reset is not reported as finished, like one without a Reliable Size
        let reset = stream.is_reset();
        entry.remove_entry();
        self.stream_freed(id, StreamHalf::Send);
        if !reset {
            self.events.push_back(StreamEvent::Finished { id });
        }
        None
    }

    pub(crate) fn retransmit(&mut self, frame: frame::StreamMeta) {
        let Some(stream) = self.send.get_mut(&frame.id).and_then(|s| s.as_mut()) else {
            // Loss of data on a closed stream is a noop
            return;
        };
        let was_pending = stream.is_pending();
        // The Reliable Size of a reset stream supersedes any FIN
        stream.fin_pending |= frame.fin && !stream.is_reset();
        stream.pending.retransmit(frame.offsets);
        if !was_pending && stream.is_pending() {
            self.pending.push_pending(frame.id, stream.priority);
        }
    }

    pub(crate) fn retransmit_all_for_0rtt(&mut self) {
        for dir in Dir::iter() {
            for index in 0..self.next[dir as usize] {
                let id = StreamId::new(Side::Client, dir, index);
                let Some(stream) = self.send.get_mut(&id).and_then(|s| s.as_mut()) else {
                    continue;
                };
                if stream.pending.is_fully_acked() && !stream.fin_pending {
                    // Stream data can't be acked in 0-RTT, so we must not have sent anything on
                    // this stream
                    continue;
                }
                if !stream.is_pending() {
                    self.pending.push_pending(id, stream.priority);
                }
                stream.pending.retransmit_all_for_0rtt();
            }
        }
    }

    pub(crate) fn received_max_streams(
        &mut self,
        dir: Dir,
        count: u64,
    ) -> Result<(), TransportError> {
        if count > MAX_STREAM_COUNT {
            return Err(TransportError::FRAME_ENCODING_ERROR(
                "unrepresentable stream limit",
            ));
        }

        let current = &mut self.max[dir as usize];
        if count > *current {
            *current = count;
            self.streams_blocked[dir as usize] = false;
            self.events.push_back(StreamEvent::Available { dir });
        }

        Ok(())
    }

    /// Handle increase to connection-level flow control limit
    pub(crate) fn received_max_data(&mut self, n: VarInt) {
        self.max_data = self.max_data.max(n.into());
    }

    pub(crate) fn received_max_stream_data(
        &mut self,
        id: StreamId,
        offset: u64,
    ) -> Result<(), TransportError> {
        if id.initiator() != self.side && id.dir() == Dir::Uni {
            debug!("got MAX_STREAM_DATA on recv-only {}", id);
            return Err(TransportError::STREAM_STATE_ERROR(
                "MAX_STREAM_DATA on recv-only stream",
            ));
        }
        if id.dir() == Dir::Bi {
            // Ensure we don't implicitly open more streams than permitted
            self.validate_receive_id(id).inspect_err(|_| {
                debug!("received illegal MAX_STREAM_DATA frame");
            })?;
        }

        self.insert_remote(id);

        let write_limit = self.write_limit();
        let max_send_data = self.max_send_data(id);
        if let Some(ss_opt) = self.send.get_mut(&id) {
            let ss = ss_opt.get_or_insert_with(|| Send::new(max_send_data));
            if ss.increase_max_data(offset) {
                if write_limit > 0 {
                    self.events.push_back(StreamEvent::Writable { id });
                } else if !ss.connection_blocked {
                    // The stream is still blocked on the connection flow control
                    // window. In order to get unblocked when the window relaxes
                    // it needs to be in the connection blocked list.
                    ss.connection_blocked = true;
                    self.connection_blocked.push(id);
                }
            }
        } else if id.initiator() == self.side && self.is_local_unopened(id) {
            debug!("got MAX_STREAM_DATA on unopened {}", id);
            return Err(TransportError::STREAM_STATE_ERROR(
                "MAX_STREAM_DATA on unopened stream",
            ));
        }

        Ok(())
    }

    /// Whether the local send buffer prevents the application from writing more data.
    pub(crate) fn send_window_full(&self) -> bool {
        self.buffered_data >= self.send_window
    }

    /// Returns the maximum amount of data this is allowed to be written on the connection
    pub(crate) fn write_limit(&self) -> u64 {
        (self.max_data - self.data_sent)
            // `send_window` can be set after construction to something *less* than `buffered_data`
            .min(self.send_window.saturating_sub(self.buffered_data))
    }

    /// Yield stream events
    pub(crate) fn poll(&mut self) -> Option<StreamEvent> {
        if let Some(dir) = Dir::iter().find(|&i| mem::replace(&mut self.opened[i as usize], false))
        {
            return Some(StreamEvent::Opened { dir });
        }

        if self.write_limit() > 0 {
            while let Some(id) = self.connection_blocked.pop() {
                let Some(stream) = self.send.get_mut(&id).and_then(|s| s.as_mut()) else {
                    continue;
                };

                debug_assert!(stream.connection_blocked);
                stream.connection_blocked = false;

                // If it's no longer sensible to write to a stream (even to detect an error) then don't
                // report it.
                if stream.is_writable() && stream.max_data > stream.offset() {
                    return Some(StreamEvent::Writable { id });
                }
            }
        }

        self.events.pop_front()
    }

    /// Queues MAX_STREAM_ID frames in `pending` if needed
    ///
    /// Returns whether any frames were queued.
    pub(crate) fn queue_max_stream_id(&mut self, pending: &mut Retransmits) -> bool {
        let mut queued = false;
        for dir in Dir::iter() {
            let diff = self.max_remote[dir as usize] - self.sent_max_remote[dir as usize];
            // To reduce traffic, only announce updates if at least 1/8 of the flow control window
            // has been consumed.
            if diff > self.max_concurrent_remote_count[dir as usize] / 8 {
                pending.max_stream_id[dir as usize] = true;
                queued = true;
            }
        }
        queued
    }

    /// Check for errors entailed by the peer's use of `id` as a send stream
    fn validate_receive_id(&mut self, id: StreamId) -> Result<(), TransportError> {
        if self.side == id.initiator() {
            match id.dir() {
                Dir::Uni => {
                    return Err(TransportError::STREAM_STATE_ERROR(
                        "illegal operation on send-only stream",
                    ));
                }
                Dir::Bi if id.index() >= self.next[Dir::Bi as usize] => {
                    return Err(TransportError::STREAM_STATE_ERROR(
                        "operation on unopened stream",
                    ));
                }
                Dir::Bi => {}
            };
        } else {
            let limit = self.max_remote[id.dir() as usize];
            if id.index() >= limit {
                return Err(TransportError::STREAM_LIMIT_ERROR(""));
            }
        }
        Ok(())
    }

    /// Whether a locally initiated stream has never been open
    pub(crate) fn is_local_unopened(&self, id: StreamId) -> bool {
        id.index() >= self.next[id.dir() as usize]
    }

    pub(crate) fn set_max_concurrent(&mut self, dir: Dir, count: VarInt) {
        self.flow_control_adjusted = true;
        self.max_concurrent_remote_count[dir as usize] = count.into();
        self.ensure_remote_streams(dir);
    }

    pub(crate) fn max_concurrent(&self, dir: Dir) -> u64 {
        self.allocated_remote_count[dir as usize]
    }

    pub(crate) fn set_send_window(&mut self, send_window: u64) {
        self.send_window = send_window;
    }

    /// Set the receive_window and returns whether the receive_window has been
    /// expanded or shrunk: true if expanded, false if shrunk.
    pub(crate) fn set_receive_window(&mut self, receive_window: VarInt) -> bool {
        let receive_window = receive_window.into();
        let mut expanded = false;
        if receive_window > self.receive_window {
            self.local_max_data = self
                .local_max_data
                .saturating_add(receive_window - self.receive_window);
            expanded = true;
        } else {
            let diff = self.receive_window - receive_window;
            self.receive_window_shrink_debt = self.receive_window_shrink_debt.saturating_add(diff);
        }
        self.receive_window = receive_window;
        expanded
    }

    /// Allocate state for a locally-initiated stream
    pub(super) fn insert_local(&mut self, id: StreamId) {
        debug_assert_eq!(id.initiator(), self.side);
        assert!(self.send.insert(id, None).is_none());
        if id.dir() == Dir::Bi {
            let recv = self.free_recv.pop();
            assert!(self.recv.insert(id, recv).is_none());
        }
    }

    /// Allocate state for remotely-initiated streams up to and including `id`
    ///
    /// A frame for index N implicitly opens `0..N`, so the skipped indices need slots too.
    fn insert_remote(&mut self, id: StreamId) {
        let dir_idx = id.dir() as usize;

        if id.initiator() == self.side
            // STREAM/RESET_STREAM/MAX_STREAM_DATA enforce this in `validate_receive_id`, but
            // STOP_SENDING does not
            || id.index() >= self.max_remote[dir_idx]
            || id.index() < self.next_remote[dir_idx]
        {
            return;
        }

        for i in self.next_remote[dir_idx]..=id.index() {
            let tid = StreamId::new(!self.side, id.dir(), i);
            let recv = self.free_recv.pop();
            assert!(self.recv.insert(tid, recv).is_none());
            if id.dir() == Dir::Bi {
                assert!(self.send.insert(tid, None).is_none());
            }
        }
        self.next_remote[dir_idx] = id.index() + 1;
        self.opened[dir_idx] = true;
    }

    /// Adds credits to the connection flow control window
    ///
    /// Returns whether a `MAX_DATA` frame should be enqueued as soon as possible.
    /// This will only be the case if the window update would is significant
    /// enough. As soon as a window update with a `MAX_DATA` frame has been
    /// queued, the [`Recv::record_sent_max_stream_data`] function should be called to
    /// suppress sending further updates until the window increases significantly
    /// again.
    pub(super) fn add_read_credits(&mut self, credits: u64) -> ShouldTransmit {
        if credits > self.receive_window_shrink_debt {
            let net_credits = credits - self.receive_window_shrink_debt;
            self.local_max_data = self.local_max_data.saturating_add(net_credits);
            self.receive_window_shrink_debt = 0;
        } else {
            self.receive_window_shrink_debt -= credits;
        }

        ShouldTransmit(self.max_data_due())
    }

    /// Return the connection credit held by `credit` to the peer.
    pub(crate) fn release(&mut self, credit: Credit) -> ShouldTransmit {
        assert!(
            credit.is_empty() || credit.tag == self.tag,
            "released credit from another connection"
        );
        let bytes = credit.into_bytes();
        assert!(
            bytes <= self.held,
            "released {bytes} bytes of credit with only {} held",
            self.held
        );
        self.held -= bytes;
        self.add_read_credits(bytes)
    }

    /// Whether the window has grown enough since the last `MAX_DATA` to be worth announcing.
    ///
    /// Updates are batched to a fraction of the receive window, which accommodates bigger windows
    /// with fewer frames. Once the peer is within that fraction of its limit any increase is
    /// worth a frame, because the credit an application holds may never add up to the fraction.
    fn max_data_due(&self) -> bool {
        if self.local_max_data > VarInt::MAX.into_inner() {
            return false;
        }

        let sent = self.sent_max_data.into_inner();
        let threshold = self.receive_window / 8;
        let diff = self.local_max_data - sent;
        diff >= threshold || (diff > 0 && sent.saturating_sub(self.data_recvd) < threshold)
    }

    pub(crate) fn credit_stats(&self) -> CreditStats {
        CreditStats {
            held: self.held,
            available: self.local_max_data.saturating_sub(self.data_recvd),
        }
    }

    /// Update counters for removal of a stream
    pub(super) fn stream_freed(&mut self, id: StreamId, half: StreamHalf) {
        if id.initiator() != self.side {
            let fully_free = id.dir() == Dir::Uni
                || match half {
                    StreamHalf::Send => !self.recv.contains_key(&id),
                    StreamHalf::Recv => !self.send.contains_key(&id),
                };
            if fully_free {
                self.allocated_remote_count[id.dir() as usize] -= 1;
                self.ensure_remote_streams(id.dir());
            }
        }
        if half == StreamHalf::Send {
            self.send_streams -= 1;
        }
    }

    pub(super) fn stream_recv_freed(&mut self, id: StreamId, recv: StreamRecv) {
        self.free_recv.push(recv.free(self.stream_receive_window));
        self.stream_freed(id, StreamHalf::Recv);
    }

    pub(super) fn max_send_data(&self, id: StreamId) -> VarInt {
        let remote = self.side != id.initiator();
        match id.dir() {
            Dir::Uni => self.initial_max_stream_data_uni,
            // Remote/local appear reversed here because the transport parameters are named from
            // the perspective of the peer.
            Dir::Bi if remote => self.initial_max_stream_data_bidi_local,
            Dir::Bi => self.initial_max_stream_data_bidi_remote,
        }
    }
}

#[inline]
pub(super) fn get_or_insert_recv(opt: &mut Option<StreamRecv>, initial_max_data: u64) -> &mut Recv {
    *opt = opt.take().map(|s| match s {
        StreamRecv::Free(recv) => StreamRecv::Open(recv),
        s => s,
    });
    opt.get_or_insert_with(|| StreamRecv::Open(Recv::new(initial_max_data)))
        .as_open_recv_mut()
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ReadableError, RecvStream, SendStream, TransportErrorCode, WriteError,
        connection::State as ConnState, connection::Streams,
    };
    use bytes::Bytes;

    fn make(side: Side) -> StreamsState {
        StreamsState::new(
            side,
            128u32.into(),
            128u32.into(),
            1024 * 1024,
            (1024 * 1024u32).into(),
            (1024 * 1024u32).into(),
        )
    }

    #[test]
    fn send_window_retains_selectively_acked_data() {
        let mut server = make(Side::Server);
        server.send_window = 24;
        server.set_params(&TransportParameters {
            initial_max_streams_uni: 1u32.into(),
            initial_max_data: 1000u32.into(),
            initial_max_stream_data_uni: 1000u32.into(),
            ..TransportParameters::default()
        });
        let (mut pending, state) = (Retransmits::default(), ConnState::Established);
        let id = Streams {
            state: &mut server,
            conn_state: &state,
        }
        .open(Dir::Uni)
        .unwrap();
        SendStream {
            id,
            state: &mut server,
            pending: &mut pending,
            conn_state: &state,
        }
        .write(&[42; 24])
        .unwrap();
        let send = server.send.get_mut(&id).unwrap().as_mut().unwrap();
        assert_eq!(send.pending.poll_transmit(16).0, 0..16);
        assert_eq!(send.pending.poll_transmit(16).0, 16..23);
        assert_eq!(send.pending.poll_transmit(16).0, 23..24);

        server.received_ack_of(frame::StreamMeta {
            id,
            offsets: 16..23,
            fin: false,
        });
        assert_eq!(
            server.write_limit(),
            0,
            "acknowledged suffix still occupies storage"
        );
        server.received_ack_of(frame::StreamMeta {
            id,
            offsets: 0..16,
            fin: false,
        });
        assert_eq!(
            server.write_limit(),
            0,
            "partial prefix keeps the original Bytes allocation"
        );
        server.received_ack_of(frame::StreamMeta {
            id,
            offsets: 23..24,
            fin: false,
        });
        assert_eq!(server.write_limit(), 24);
        SendStream {
            id,
            state: &mut server,
            pending: &mut pending,
            conn_state: &state,
        }
        .write(&[7; 24])
        .unwrap();
        assert_eq!(server.write_limit(), 0);
    }

    #[test]
    fn reset_releases_retained_send_storage() {
        let mut server = make(Side::Server);
        server.send_window = 24;
        server.set_params(&TransportParameters {
            initial_max_streams_uni: 1u32.into(),
            initial_max_data: 1000u32.into(),
            initial_max_stream_data_uni: 1000u32.into(),
            ..TransportParameters::default()
        });
        let (mut pending, state) = (Retransmits::default(), ConnState::Established);
        let id = Streams {
            state: &mut server,
            conn_state: &state,
        }
        .open(Dir::Uni)
        .unwrap();
        SendStream {
            id,
            state: &mut server,
            pending: &mut pending,
            conn_state: &state,
        }
        .write(&[42; 24])
        .unwrap();
        let send = server.send.get_mut(&id).unwrap().as_mut().unwrap();
        send.pending.poll_transmit(16);
        send.pending.poll_transmit(16);
        server.received_ack_of(frame::StreamMeta {
            id,
            offsets: 16..23,
            fin: false,
        });
        SendStream {
            id,
            state: &mut server,
            pending: &mut pending,
            conn_state: &state,
        }
        .reset(0u32.into())
        .unwrap();

        let send = server.send.get_mut(&id).unwrap().as_mut().unwrap();
        assert!(
            send.pending.is_fully_acked(),
            "reset must release the abandoned allocation"
        );
        assert_eq!(
            send.reset_frame(id, false).unwrap().final_offset,
            VarInt(24),
            "RESET_STREAM must retain its final offset"
        );
        assert_eq!(server.write_limit(), 24);
        server.retransmit(frame::StreamMeta {
            id,
            offsets: 0..16,
            fin: false,
        });
        server.received_ack_of(frame::StreamMeta {
            id,
            offsets: 0..16,
            fin: false,
        });
        assert!(!server.send[&id].as_ref().unwrap().is_pending());
        assert_eq!(server.write_limit(), 24);
    }

    #[test]
    fn trivial_flow_control() {
        let mut client = StreamsState::new(
            Side::Client,
            1u32.into(),
            1u32.into(),
            1024 * 1024,
            (1024 * 1024u32).into(),
            (1024 * 1024u32).into(),
        );
        let id = StreamId::new(Side::Server, Dir::Uni, 0);
        let initial_max = client.local_max_data;
        const MESSAGE_SIZE: usize = 2048;
        assert_eq!(
            client
                .received(
                    frame::Stream {
                        id,
                        offset: 0,
                        fin: true,
                        data: Bytes::from_static(&[0; MESSAGE_SIZE]),
                    },
                    2048
                )
                .unwrap(),
            ShouldTransmit(false)
        );
        assert_eq!(client.data_recvd, 2048);
        assert_eq!(client.local_max_data - initial_max, 0);

        let mut pending = Retransmits::default();
        let mut recv = RecvStream {
            id,
            state: &mut client,
            pending: &mut pending,
        };

        let mut chunks = recv.read(true).unwrap();
        assert_eq!(
            chunks.next(MESSAGE_SIZE).unwrap().unwrap().bytes.len(),
            MESSAGE_SIZE
        );
        assert!(chunks.next(0).unwrap().is_none());
        let should_transmit = chunks.finalize();
        assert!(should_transmit.0);
        assert!(pending.max_stream_id[Dir::Uni as usize]);
        assert_eq!(client.local_max_data - initial_max, MESSAGE_SIZE as u64);
    }

    #[test]
    fn reset_flow_control() {
        let mut client = make(Side::Client);
        let id = StreamId::new(Side::Server, Dir::Uni, 0);
        let initial_max = client.local_max_data;
        assert_eq!(
            client
                .received(
                    frame::Stream {
                        id,
                        offset: 0,
                        fin: false,
                        data: Bytes::from_static(&[0; 2048]),
                    },
                    2048
                )
                .unwrap(),
            ShouldTransmit(false)
        );
        assert_eq!(client.data_recvd, 2048);
        assert_eq!(client.local_max_data - initial_max, 0);

        let mut pending = Retransmits::default();
        let mut recv = RecvStream {
            id,
            state: &mut client,
            pending: &mut pending,
        };

        let mut chunks = recv.read(true).unwrap();
        chunks.next(1024).unwrap();
        let _ = chunks.finalize();
        assert_eq!(client.local_max_data - initial_max, 1024);
        assert_eq!(
            client
                .received_reset(frame::ResetStream {
                    id,
                    error_code: 0u32.into(),
                    final_offset: 4096u32.into(),
                    reliable_size: None,
                })
                .unwrap(),
            ShouldTransmit(false)
        );

        assert_eq!(client.data_recvd, 4096);
        assert_eq!(client.local_max_data - initial_max, 4096);

        // Ensure reading after a reset doesn't issue redundant credit
        let mut recv = RecvStream {
            id,
            state: &mut client,
            pending: &mut pending,
        };
        let mut chunks = recv.read(true).unwrap();
        assert_eq!(
            chunks.next(1024).unwrap_err(),
            crate::ReadError::Reset(0u32.into())
        );
        let _ = chunks.finalize();
        assert_eq!(client.data_recvd, 4096);
        assert_eq!(client.local_max_data - initial_max, 4096);
    }

    #[test]
    fn reset_after_empty_frame_flow_control() {
        let mut client = make(Side::Client);
        let id = StreamId::new(Side::Server, Dir::Uni, 0);
        let initial_max = client.local_max_data;
        assert_eq!(
            client
                .received(
                    frame::Stream {
                        id,
                        offset: 4096,
                        fin: false,
                        data: Bytes::from_static(&[0; 0]),
                    },
                    0
                )
                .unwrap(),
            ShouldTransmit(false)
        );
        assert_eq!(client.data_recvd, 4096);
        assert_eq!(client.local_max_data - initial_max, 0);
        assert_eq!(
            client
                .received_reset(frame::ResetStream {
                    id,
                    error_code: 0u32.into(),
                    final_offset: 4096u32.into(),
                    reliable_size: None,
                })
                .unwrap(),
            ShouldTransmit(false)
        );
        assert_eq!(client.data_recvd, 4096);
        assert_eq!(client.local_max_data - initial_max, 4096);
    }

    #[test]
    fn duplicate_reset_flow_control() {
        let mut client = make(Side::Client);
        let id = StreamId::new(Side::Server, Dir::Uni, 0);
        assert_eq!(
            client
                .received_reset(frame::ResetStream {
                    id,
                    error_code: 0u32.into(),
                    final_offset: 4096u32.into(),
                    reliable_size: None,
                })
                .unwrap(),
            ShouldTransmit(false)
        );
        assert_eq!(client.data_recvd, 4096);
        assert_eq!(
            client
                .received_reset(frame::ResetStream {
                    id,
                    error_code: 0u32.into(),
                    final_offset: 4096u32.into(),
                    reliable_size: None,
                })
                .unwrap(),
            ShouldTransmit(false)
        );
        assert_eq!(client.data_recvd, 4096);
    }

    #[test]
    fn recv_stopped() {
        let mut client = make(Side::Client);
        let id = StreamId::new(Side::Server, Dir::Uni, 0);
        let initial_max = client.local_max_data;
        assert_eq!(
            client
                .received(
                    frame::Stream {
                        id,
                        offset: 0,
                        fin: false,
                        data: Bytes::from_static(&[0; 32]),
                    },
                    32
                )
                .unwrap(),
            ShouldTransmit(false)
        );
        assert_eq!(client.local_max_data, initial_max);

        let mut pending = Retransmits::default();
        let mut recv = RecvStream {
            id,
            state: &mut client,
            pending: &mut pending,
        };

        recv.stop(0u32.into()).unwrap();
        assert_eq!(recv.pending.stop_sending.len(), 1);
        assert!(!recv.pending.max_data);

        assert!(recv.stop(0u32.into()).is_err());
        assert_eq!(recv.read(true).err(), Some(ReadableError::ClosedStream));
        assert_eq!(recv.read(false).err(), Some(ReadableError::ClosedStream));

        assert_eq!(client.local_max_data - initial_max, 32);
        assert_eq!(
            client
                .received(
                    frame::Stream {
                        id,
                        offset: 32,
                        fin: true,
                        data: Bytes::from_static(&[0; 16]),
                    },
                    16
                )
                .unwrap(),
            ShouldTransmit(false)
        );
        assert_eq!(client.local_max_data - initial_max, 48);
        assert!(!client.recv.contains_key(&id));
    }

    #[test]
    fn stopped_reset() {
        for final_offset in [32u32, 48] {
            let mut client = make(Side::Client);
            let initial_max_data = client.local_max_data;
            let id = StreamId::new(Side::Server, Dir::Uni, 0);
            // Server opens stream
            assert_eq!(
                client
                    .received(
                        frame::Stream {
                            id,
                            offset: 0,
                            fin: false,
                            data: Bytes::from_static(&[0; 32])
                        },
                        32
                    )
                    .unwrap(),
                ShouldTransmit(false)
            );

            let mut pending = Retransmits::default();
            let mut recv = RecvStream {
                id,
                state: &mut client,
                pending: &mut pending,
            };

            recv.stop(0u32.into()).unwrap();
            assert_eq!(pending.stop_sending.len(), 1);
            assert!(!pending.max_data);
            assert_eq!(client.local_max_data, initial_max_data + 32);

            // Server complies
            let prev_max = client.max_remote[Dir::Uni as usize];
            assert_eq!(
                client
                    .received_reset(frame::ResetStream {
                        id,
                        error_code: 0u32.into(),
                        final_offset: final_offset.into(),
                        reliable_size: None,
                    })
                    .unwrap(),
                ShouldTransmit(false)
            );
            assert!(!client.recv.contains_key(&id), "stream state is freed");
            assert_eq!(client.max_remote[Dir::Uni as usize], prev_max + 1);
            assert_eq!(client.data_recvd, u64::from(final_offset));
            assert_eq!(
                client.local_max_data,
                initial_max_data + u64::from(final_offset)
            );
        }
    }

    #[test]
    fn send_stopped() {
        let mut server = make(Side::Server);
        server.set_params(&TransportParameters {
            initial_max_streams_uni: 1u32.into(),
            initial_max_data: 42u32.into(),
            initial_max_stream_data_uni: 42u32.into(),
            ..TransportParameters::default()
        });

        let (mut pending, state) = (Retransmits::default(), ConnState::Established);
        let id = Streams {
            state: &mut server,
            conn_state: &state,
        }
        .open(Dir::Uni)
        .unwrap();

        let mut stream = SendStream {
            id,
            state: &mut server,
            pending: &mut pending,
            conn_state: &state,
        };

        let error_code = 0u32.into();
        stream.state.received_stop_sending(id, error_code);
        assert!(
            stream
                .state
                .events
                .contains(&StreamEvent::Stopped { id, error_code })
        );
        stream.state.events.clear();

        assert_eq!(stream.write(&[]), Err(WriteError::Stopped(error_code)));

        stream.reset(0u32.into()).unwrap();
        assert_eq!(stream.write(&[]), Err(WriteError::ClosedStream));

        // A duplicate frame is a no-op
        stream.state.received_stop_sending(id, error_code);
        assert!(stream.state.events.is_empty());
    }

    #[test]
    fn final_offset_flow_control() {
        let mut client = make(Side::Client);
        assert_eq!(
            client
                .received_reset(frame::ResetStream {
                    id: StreamId::new(Side::Server, Dir::Uni, 0),
                    error_code: 0u32.into(),
                    final_offset: VarInt::MAX,
                    reliable_size: None,
                })
                .unwrap_err()
                .code,
            TransportErrorCode::FLOW_CONTROL_ERROR
        );
    }

    #[test]
    fn stream_priority() {
        let mut server = make(Side::Server);
        server.set_params(&TransportParameters {
            initial_max_streams_bidi: 3u32.into(),
            initial_max_data: 10u32.into(),
            initial_max_stream_data_bidi_remote: 10u32.into(),
            ..TransportParameters::default()
        });

        let (mut pending, state) = (Retransmits::default(), ConnState::Established);
        let mut streams = Streams {
            state: &mut server,
            conn_state: &state,
        };

        let id_high = streams.open(Dir::Bi).unwrap();
        let id_mid = streams.open(Dir::Bi).unwrap();
        let id_low = streams.open(Dir::Bi).unwrap();

        let mut mid = SendStream {
            id: id_mid,
            state: &mut server,
            pending: &mut pending,
            conn_state: &state,
        };
        mid.write(b"mid").unwrap();

        let mut low = SendStream {
            id: id_low,
            state: &mut server,
            pending: &mut pending,
            conn_state: &state,
        };
        low.set_priority(-1).unwrap();
        low.write(b"low").unwrap();

        let mut high = SendStream {
            id: id_high,
            state: &mut server,
            pending: &mut pending,
            conn_state: &state,
        };
        high.set_priority(1).unwrap();
        high.write(b"high").unwrap();

        let mut buf = Vec::with_capacity(40);
        let meta = server.write_stream_frames(&mut buf, 40, true);
        assert_eq!(meta[0].id, id_high);
        assert_eq!(meta[1].id, id_mid);
        assert_eq!(meta[2].id, id_low);

        assert!(!server.can_send_stream_data());
        assert_eq!(server.pending.len(), 0);
    }

    #[test]
    fn retransmit_stream_priority() {
        for fair in [true, false] {
            let mut server = make(Side::Server);
            server.set_params(&TransportParameters {
                initial_max_streams_bidi: 2u32.into(),
                initial_max_data: 10u32.into(),
                initial_max_stream_data_bidi_remote: 10u32.into(),
                ..TransportParameters::default()
            });

            let (mut pending, state) = (Retransmits::default(), ConnState::Established);
            let mut streams = Streams {
                state: &mut server,
                conn_state: &state,
            };
            let id_low = streams.open(Dir::Bi).unwrap();
            let id_high = streams.open(Dir::Bi).unwrap();

            let mut low = SendStream {
                id: id_low,
                state: &mut server,
                pending: &mut pending,
                conn_state: &state,
            };
            low.set_priority(-1).unwrap();
            assert_eq!(low.write(b"low").unwrap(), 3);

            let mut buf = Vec::with_capacity(40);
            let mut sent = server.write_stream_frames(&mut buf, 40, fair);
            assert_eq!(sent.len(), 1);
            let lost = sent.pop().unwrap();
            assert_eq!(lost.id, id_low);
            assert_eq!(lost.offsets, 0..3);
            // An existing queue entry would skip retransmit's priority assignment.
            assert!(!server.can_send_stream_data());
            assert_eq!(server.pending.len(), 0);

            server.retransmit(lost);
            assert_eq!(server.pending.len(), 1);

            let mut high = SendStream {
                id: id_high,
                state: &mut server,
                pending: &mut pending,
                conn_state: &state,
            };
            high.set_priority(1).unwrap();
            assert_eq!(high.write(b"high").unwrap(), 4);

            buf.clear();
            let sent = server.write_stream_frames(&mut buf, 40, fair);
            assert_eq!(sent.len(), 2);
            assert_eq!(sent[0].id, id_high);
            assert_eq!(sent[0].offsets, 0..4);
            assert_eq!(sent[1].id, id_low);
            assert_eq!(sent[1].offsets, 0..3);
            assert!(!server.can_send_stream_data());
            assert_eq!(server.pending.len(), 0);
        }
    }

    #[test]
    fn requeue_stream_priority() {
        let mut server = make(Side::Server);
        server.set_params(&TransportParameters {
            initial_max_streams_bidi: 3u32.into(),
            initial_max_data: 1000u32.into(),
            initial_max_stream_data_bidi_remote: 1000u32.into(),
            ..TransportParameters::default()
        });

        let (mut pending, state) = (Retransmits::default(), ConnState::Established);
        let mut streams = Streams {
            state: &mut server,
            conn_state: &state,
        };

        let id_high = streams.open(Dir::Bi).unwrap();
        let id_mid = streams.open(Dir::Bi).unwrap();

        let mut mid = SendStream {
            id: id_mid,
            state: &mut server,
            pending: &mut pending,
            conn_state: &state,
        };
        assert_eq!(mid.write(b"mid").unwrap(), 3);
        assert_eq!(server.pending.len(), 1);

        let mut high = SendStream {
            id: id_high,
            state: &mut server,
            pending: &mut pending,
            conn_state: &state,
        };
        high.set_priority(1).unwrap();
        assert_eq!(high.write(&[0; 200]).unwrap(), 200);
        assert_eq!(server.pending.len(), 2);

        // Requeue the high priority stream to lowest priority. The initial send
        // still uses high priority since it's queued that way. After that it will
        // switch to low priority
        let mut high = SendStream {
            id: id_high,
            state: &mut server,
            pending: &mut pending,
            conn_state: &state,
        };
        high.set_priority(-1).unwrap();

        let mut buf = Vec::with_capacity(1000);
        let meta = server.write_stream_frames(&mut buf, 40, true);
        assert_eq!(meta.len(), 1);
        assert_eq!(meta[0].id, id_high);

        // After requeuing we should end up with 2 priorities - not 3
        assert_eq!(server.pending.len(), 2);

        // Send the remaining data. The initial mid priority one should go first now
        let meta = server.write_stream_frames(&mut buf, 1000, true);
        assert_eq!(meta.len(), 2);
        assert_eq!(meta[0].id, id_mid);
        assert_eq!(meta[1].id, id_high);

        assert!(!server.can_send_stream_data());
        assert_eq!(server.pending.len(), 0);
    }

    #[test]
    fn same_stream_priority() {
        for fair in [true, false] {
            let mut server = make(Side::Server);
            server.set_params(&TransportParameters {
                initial_max_streams_bidi: 3u32.into(),
                initial_max_data: 300u32.into(),
                initial_max_stream_data_bidi_remote: 300u32.into(),
                ..TransportParameters::default()
            });

            let (mut pending, state) = (Retransmits::default(), ConnState::Established);
            let mut streams = Streams {
                state: &mut server,
                conn_state: &state,
            };

            // a, b and c all have the same priority
            let id_a = streams.open(Dir::Bi).unwrap();
            let id_b = streams.open(Dir::Bi).unwrap();
            let id_c = streams.open(Dir::Bi).unwrap();

            let mut stream_a = SendStream {
                id: id_a,
                state: &mut server,
                pending: &mut pending,
                conn_state: &state,
            };
            stream_a.write(&[b'a'; 100]).unwrap();

            let mut stream_b = SendStream {
                id: id_b,
                state: &mut server,
                pending: &mut pending,
                conn_state: &state,
            };
            stream_b.write(&[b'b'; 100]).unwrap();

            let mut stream_c = SendStream {
                id: id_c,
                state: &mut server,
                pending: &mut pending,
                conn_state: &state,
            };
            stream_c.write(&[b'c'; 100]).unwrap();

            let mut metas = vec![];
            let mut buf = Vec::with_capacity(1024);

            // loop until all the streams are written
            loop {
                let buf_len = buf.len();
                let meta = server.write_stream_frames(&mut buf, buf_len + 40, fair);
                if meta.is_empty() {
                    break;
                }
                metas.extend(meta);
            }

            assert!(!server.can_send_stream_data());
            assert_eq!(server.pending.len(), 0);

            let stream_ids = metas.iter().map(|m| m.id).collect::<Vec<_>>();
            if fair {
                // When fairness is enabled, if we run out of buffer space to write out a stream,
                // the stream is re-queued after all the streams with the same priority.
                assert_eq!(
                    stream_ids,
                    vec![id_a, id_b, id_c, id_a, id_b, id_c, id_a, id_b, id_c]
                );
            } else {
                // When fairness is disabled the stream is re-queued before all the other streams
                // with the same priority.
                assert_eq!(
                    stream_ids,
                    vec![id_a, id_a, id_a, id_b, id_b, id_b, id_c, id_c, id_c]
                );
            }
        }
    }

    #[test]
    fn unfair_priority_bump() {
        let mut server = make(Side::Server);
        server.set_params(&TransportParameters {
            initial_max_streams_bidi: 3u32.into(),
            initial_max_data: 300u32.into(),
            initial_max_stream_data_bidi_remote: 300u32.into(),
            ..TransportParameters::default()
        });

        let (mut pending, state) = (Retransmits::default(), ConnState::Established);
        let mut streams = Streams {
            state: &mut server,
            conn_state: &state,
        };

        // a, and b have the same priority, c has higher priority
        let id_a = streams.open(Dir::Bi).unwrap();
        let id_b = streams.open(Dir::Bi).unwrap();
        let id_c = streams.open(Dir::Bi).unwrap();

        let mut stream_a = SendStream {
            id: id_a,
            state: &mut server,
            pending: &mut pending,
            conn_state: &state,
        };
        stream_a.write(&[b'a'; 100]).unwrap();

        let mut stream_b = SendStream {
            id: id_b,
            state: &mut server,
            pending: &mut pending,
            conn_state: &state,
        };
        stream_b.write(&[b'b'; 100]).unwrap();

        let mut metas = vec![];
        let mut buf = Vec::with_capacity(1024);

        // Write the first chunk of stream_a
        let buf_len = buf.len();
        let meta = server.write_stream_frames(&mut buf, buf_len + 40, false);
        assert!(!meta.is_empty());
        metas.extend(meta);

        // Queue stream_c which has higher priority
        let mut stream_c = SendStream {
            id: id_c,
            state: &mut server,
            pending: &mut pending,
            conn_state: &state,
        };
        stream_c.set_priority(1).unwrap();
        stream_c.write(&[b'b'; 100]).unwrap();

        // loop until all the streams are written
        loop {
            let buf_len = buf.len();
            let meta = server.write_stream_frames(&mut buf, buf_len + 40, false);
            if meta.is_empty() {
                break;
            }
            metas.extend(meta);
        }

        assert!(!server.can_send_stream_data());
        assert_eq!(server.pending.len(), 0);

        let stream_ids = metas.iter().map(|m| m.id).collect::<Vec<_>>();
        assert_eq!(
            stream_ids,
            // stream_c bumps stream_b but doesn't bump stream_a which had already been partly
            // written out
            vec![id_a, id_a, id_a, id_c, id_c, id_c, id_b, id_b, id_b]
        );
    }

    #[test]
    fn stop_finished() {
        let mut client = make(Side::Client);
        let id = StreamId::new(Side::Server, Dir::Uni, 0);
        // Server finishes stream
        let _ = client
            .received(
                frame::Stream {
                    id,
                    offset: 0,
                    fin: true,
                    data: Bytes::from_static(&[0; 32]),
                },
                32,
            )
            .unwrap();
        let mut pending = Retransmits::default();
        let mut stream = RecvStream {
            id,
            state: &mut client,
            pending: &mut pending,
        };
        stream.stop(0u32.into()).unwrap();
        assert!(client.recv.get_mut(&id).is_none(), "stream is freed");
    }

    // Verify that a stream that's been reset doesn't cause the appearance of pending data
    #[test]
    fn reset_stream_cannot_send() {
        let mut server = make(Side::Server);
        server.set_params(&TransportParameters {
            initial_max_streams_uni: 1u32.into(),
            initial_max_data: 42u32.into(),
            initial_max_stream_data_uni: 42u32.into(),
            ..TransportParameters::default()
        });
        let (mut pending, state) = (Retransmits::default(), ConnState::Established);
        let mut streams = Streams {
            state: &mut server,
            conn_state: &state,
        };

        let id = streams.open(Dir::Uni).unwrap();
        let mut stream = SendStream {
            id,
            state: &mut server,
            pending: &mut pending,
            conn_state: &state,
        };
        stream.write(b"hello").unwrap();
        stream.reset(0u32.into()).unwrap();

        assert_eq!(pending.reset_stream, &[(id, 0)]);
        assert!(!server.can_send_stream_data());
    }

    #[test]
    fn stream_limit_fixed() {
        let mut client = make(Side::Client);
        // Open streams 0-127
        assert_eq!(
            client.received(
                frame::Stream {
                    id: StreamId::new(Side::Server, Dir::Uni, 127),
                    offset: 0,
                    fin: true,
                    data: Bytes::from_static(&[]),
                },
                0
            ),
            Ok(ShouldTransmit(false))
        );
        // Try to open stream 128, exceeding limit
        assert_eq!(
            client
                .received(
                    frame::Stream {
                        id: StreamId::new(Side::Server, Dir::Uni, 128),
                        offset: 0,
                        fin: true,
                        data: Bytes::from_static(&[]),
                    },
                    0
                )
                .unwrap_err()
                .code,
            TransportErrorCode::STREAM_LIMIT_ERROR
        );

        // Free stream 127
        let mut pending = Retransmits::default();
        let mut stream = RecvStream {
            id: StreamId::new(Side::Server, Dir::Uni, 127),
            state: &mut client,
            pending: &mut pending,
        };
        stream.stop(0u32.into()).unwrap();

        // Open stream 128
        assert_eq!(
            client.received(
                frame::Stream {
                    id: StreamId::new(Side::Server, Dir::Uni, 128),
                    offset: 0,
                    fin: true,
                    data: Bytes::from_static(&[]),
                },
                0
            ),
            Ok(ShouldTransmit(false))
        );
    }

    #[test]
    fn stream_limit_grows() {
        let mut client = make(Side::Client);
        // Open streams 0-127
        assert_eq!(
            client.received(
                frame::Stream {
                    id: StreamId::new(Side::Server, Dir::Uni, 127),
                    offset: 0,
                    fin: true,
                    data: Bytes::from_static(&[]),
                },
                0
            ),
            Ok(ShouldTransmit(false))
        );
        // Try to open stream 128, exceeding limit
        assert_eq!(
            client
                .received(
                    frame::Stream {
                        id: StreamId::new(Side::Server, Dir::Uni, 128),
                        offset: 0,
                        fin: true,
                        data: Bytes::from_static(&[]),
                    },
                    0
                )
                .unwrap_err()
                .code,
            TransportErrorCode::STREAM_LIMIT_ERROR
        );

        // Relax limit by one
        client.set_max_concurrent(Dir::Uni, 129u32.into());

        // Open stream 128
        assert_eq!(
            client.received(
                frame::Stream {
                    id: StreamId::new(Side::Server, Dir::Uni, 128),
                    offset: 0,
                    fin: true,
                    data: Bytes::from_static(&[]),
                },
                0
            ),
            Ok(ShouldTransmit(false))
        );
    }

    #[test]
    fn stream_limit_shrinks() {
        let mut client = make(Side::Client);
        // Open streams 0-127
        assert_eq!(
            client.received(
                frame::Stream {
                    id: StreamId::new(Side::Server, Dir::Uni, 127),
                    offset: 0,
                    fin: true,
                    data: Bytes::from_static(&[]),
                },
                0
            ),
            Ok(ShouldTransmit(false))
        );

        // Tighten limit by one
        client.set_max_concurrent(Dir::Uni, 127u32.into());

        // Free stream 127
        let mut pending = Retransmits::default();
        let mut stream = RecvStream {
            id: StreamId::new(Side::Server, Dir::Uni, 127),
            state: &mut client,
            pending: &mut pending,
        };
        stream.stop(0u32.into()).unwrap();

        // Try to open stream 128, still exceeding limit
        assert_eq!(
            client
                .received(
                    frame::Stream {
                        id: StreamId::new(Side::Server, Dir::Uni, 128),
                        offset: 0,
                        fin: true,
                        data: Bytes::from_static(&[]),
                    },
                    0
                )
                .unwrap_err()
                .code,
            TransportErrorCode::STREAM_LIMIT_ERROR
        );

        // Free stream 126
        assert_eq!(
            client.received_reset(frame::ResetStream {
                id: StreamId::new(Side::Server, Dir::Uni, 126),
                error_code: 0u32.into(),
                final_offset: 0u32.into(),
                reliable_size: None,
            }),
            Ok(ShouldTransmit(false))
        );
        let mut pending = Retransmits::default();
        let mut stream = RecvStream {
            id: StreamId::new(Side::Server, Dir::Uni, 126),
            state: &mut client,
            pending: &mut pending,
        };
        stream.stop(0u32.into()).unwrap();

        // Open stream 128
        assert_eq!(
            client.received(
                frame::Stream {
                    id: StreamId::new(Side::Server, Dir::Uni, 128),
                    offset: 0,
                    fin: true,
                    data: Bytes::from_static(&[]),
                },
                0
            ),
            Ok(ShouldTransmit(false))
        );
    }

    #[test]
    fn remote_stream_capacity() {
        let mut client = make(Side::Client);
        for _ in 0..2 {
            client.set_max_concurrent(Dir::Uni, 200u32.into());
            client.set_max_concurrent(Dir::Bi, 201u32.into());
            assert_eq!(client.max_remote[Dir::Uni as usize], 200);
            assert_eq!(client.max_remote[Dir::Bi as usize], 201);
            assert_eq!(client.allocated_remote_count[Dir::Uni as usize], 200);
            assert_eq!(client.allocated_remote_count[Dir::Bi as usize], 201);
            // Slots are allocated lazily, so no stream exists yet
            assert!(client.recv.is_empty());
            assert!(client.send.is_empty());
        }
    }

    #[test]
    fn lazy_remote_allocation_starts_empty() {
        let client = StreamsState::new(
            Side::Client,
            10_000u32.into(),
            10_000u32.into(),
            1024 * 1024,
            (1024 * 1024u32).into(),
            (1024 * 1024u32).into(),
        );
        assert!(client.recv.is_empty());
        assert!(client.send.is_empty());
        assert_eq!(client.recv.capacity(), 0);
        assert_eq!(client.send.capacity(), 0);
    }

    #[test]
    fn out_of_order_implicit_open() {
        // Receiving index 5 implicitly opens 0..=4, so a later frame for 3 must still work
        let mut client = make(Side::Client);
        assert_eq!(
            client.received(
                frame::Stream {
                    id: StreamId::new(Side::Server, Dir::Uni, 5),
                    offset: 0,
                    fin: true,
                    data: Bytes::from_static(&[0; 8]),
                },
                8,
            ),
            Ok(ShouldTransmit(false))
        );
        assert_eq!(client.next_remote[Dir::Uni as usize], 6);
        assert_eq!(
            client.received(
                frame::Stream {
                    id: StreamId::new(Side::Server, Dir::Uni, 3),
                    offset: 0,
                    fin: true,
                    data: Bytes::from_static(&[0; 4]),
                },
                4,
            ),
            Ok(ShouldTransmit(false))
        );
        let id = StreamId::new(Side::Server, Dir::Uni, 3);
        let mut pending = Retransmits::default();
        let mut recv = RecvStream {
            id,
            state: &mut client,
            pending: &mut pending,
        };
        let mut chunks = recv.read(true).unwrap();
        assert_eq!(chunks.next(4).unwrap().unwrap().bytes.len(), 4);
        let _ = chunks.finalize();
    }

    #[test]
    fn frame_for_closed_stream_is_dropped() {
        // Below the frontier, absence from the map means closed
        let mut client = make(Side::Client);
        let id = StreamId::new(Side::Server, Dir::Uni, 0);
        assert_eq!(
            client.received(
                frame::Stream {
                    id,
                    offset: 0,
                    fin: true,
                    data: Bytes::from_static(&[0; 4]),
                },
                4,
            ),
            Ok(ShouldTransmit(false))
        );
        // Drain the stream so it's fully freed
        let mut pending = Retransmits::default();
        let mut recv = RecvStream {
            id,
            state: &mut client,
            pending: &mut pending,
        };
        let mut chunks = recv.read(true).unwrap();
        assert_eq!(chunks.next(4).unwrap().unwrap().bytes.len(), 4);
        assert!(chunks.next(4).unwrap().is_none());
        let _ = chunks.finalize();
        assert!(!client.recv.contains_key(&id));

        // A stray retransmit must not resurrect the stream
        assert_eq!(
            client.received(
                frame::Stream {
                    id,
                    offset: 0,
                    fin: true,
                    data: Bytes::from_static(&[0; 4]),
                },
                4,
            ),
            Ok(ShouldTransmit(false))
        );
        assert!(!client.recv.contains_key(&id));
    }

    #[test]
    fn churn_keeps_maps_bounded() {
        // Opening and closing many streams must not accumulate state
        let mut client = make(Side::Client);
        const N: u64 = 5_000;
        for i in 0..N {
            // Give ourselves room as we churn through ids
            client.set_max_concurrent(Dir::Uni, (i + 200).try_into().unwrap());
            let id = StreamId::new(Side::Server, Dir::Uni, i);
            assert_eq!(
                client.received(
                    frame::Stream {
                        id,
                        offset: 0,
                        fin: true,
                        data: Bytes::from_static(&[0; 1]),
                    },
                    1,
                ),
                Ok(ShouldTransmit(false))
            );
            let mut pending = Retransmits::default();
            let mut recv = RecvStream {
                id,
                state: &mut client,
                pending: &mut pending,
            };
            let mut chunks = recv.read(true).unwrap();
            let _ = chunks.next(1).unwrap();
            assert!(chunks.next(1).unwrap().is_none());
            let _ = chunks.finalize();
        }
        assert_eq!(client.recv.len(), 0);
        assert_eq!(client.send.len(), 0);
    }

    #[test]
    fn expand_receive_window() {
        let mut server = make(Side::Server);
        let new_receive_window = 2 * server.receive_window as u32;
        let expanded = server.set_receive_window(new_receive_window.into());
        assert!(expanded);
        assert_eq!(server.receive_window, new_receive_window as u64);
        assert_eq!(server.local_max_data, new_receive_window as u64);
        assert_eq!(server.receive_window_shrink_debt, 0);
        let prev_local_max_data = server.local_max_data;

        // credit, expecting all of them added to local_max_data
        let credits = 1024u64;
        let should_transmit = server.add_read_credits(credits);
        assert_eq!(server.receive_window_shrink_debt, 0);
        assert_eq!(server.local_max_data, prev_local_max_data + credits);
        assert!(should_transmit.should_transmit());
    }

    #[test]
    fn shrink_receive_window() {
        let mut server = make(Side::Server);
        let new_receive_window = server.receive_window as u32 / 2;
        let prev_local_max_data = server.local_max_data;

        // shrink the receive_winbow, local_max_data is not expected to be changed
        let shrink_diff = server.receive_window - new_receive_window as u64;
        let expanded = server.set_receive_window(new_receive_window.into());
        assert!(!expanded);
        assert_eq!(server.receive_window, new_receive_window as u64);
        assert_eq!(server.local_max_data, prev_local_max_data);
        assert_eq!(server.receive_window_shrink_debt, shrink_diff);
        let prev_local_max_data = server.local_max_data;

        // credit twice, local_max_data does not change as it is absorbed by receive_window_shrink_debt
        let credits = 1024u64;
        for _ in 0..2 {
            let expected_receive_window_shrink_debt = server.receive_window_shrink_debt - credits;
            let should_transmit = server.add_read_credits(credits);
            assert_eq!(
                server.receive_window_shrink_debt,
                expected_receive_window_shrink_debt
            );
            assert_eq!(server.local_max_data, prev_local_max_data);
            assert!(!should_transmit.should_transmit());
        }

        // credit again which exceeds all remaining expected_receive_window_shrink_debt
        let credits = 1024 * 512;
        let prev_local_max_data = server.local_max_data;
        let expected_local_max_data =
            server.local_max_data + (credits - server.receive_window_shrink_debt);
        let _should_transmit = server.add_read_credits(credits);
        assert_eq!(server.receive_window_shrink_debt, 0);
        assert_eq!(server.local_max_data, expected_local_max_data);
        assert!(server.local_max_data > prev_local_max_data);

        // credit again, all should be added to local_max_data
        let credits = 1024 * 512;
        let expected_local_max_data = server.local_max_data + credits;
        let should_transmit = server.add_read_credits(credits);
        assert_eq!(server.receive_window_shrink_debt, 0);
        assert_eq!(server.local_max_data, expected_local_max_data);
        assert!(should_transmit.should_transmit());
    }

    #[test]
    fn expand_send_window() {
        let mut server = make(Side::Server);

        let initial_send_window = server.send_window;
        let larger_send_window = initial_send_window * 2;

        // Set `initial_max_data` larger than `send_window` so we're limited by local flow control
        server.set_params(&TransportParameters {
            initial_max_data: VarInt::MAX,
            initial_max_stream_data_uni: VarInt::MAX,
            initial_max_streams_uni: VarInt::from_u32(100),
            ..TransportParameters::default()
        });

        assert_eq!(server.write_limit(), initial_send_window);
        assert_eq!(server.poll(), None);

        let mut retransmits = Retransmits::default();
        let conn_state = ConnState::Established;

        let stream_id = Streams {
            state: &mut server,
            conn_state: &conn_state,
        }
        .open(Dir::Uni)
        .expect("should be able to open a stream");

        let mut stream = SendStream {
            id: stream_id,
            state: &mut server,
            pending: &mut retransmits,
            conn_state: &conn_state,
        };

        // Check that the stream accepts `initial_send_window` bytes
        let initial_send_len = initial_send_window as usize;
        let data = vec![0xFFu8; initial_send_len];

        assert_eq!(stream.write(&data), Ok(initial_send_len));

        // Try to write the same data again, observe that it's blocked
        assert_eq!(stream.write(&data), Err(WriteError::Blocked));

        // Check that we get a `Writable` event after increasing the send window
        stream.state.set_send_window(larger_send_window);
        assert_eq!(
            stream.state.poll(),
            Some(StreamEvent::Writable { id: stream_id })
        );

        // Check that the stream accepts the exact same amount of data again
        assert_eq!(stream.write(&data), Ok(initial_send_len));
        assert_eq!(stream.write(&data), Err(WriteError::Blocked));

        assert_eq!(stream.state.poll(), None);

        // Ack the data
        stream.state.received_ack_of(frame::StreamMeta {
            id: stream_id,
            offsets: 0..larger_send_window,
            fin: false,
        });

        assert_eq!(
            stream.state.poll(),
            Some(StreamEvent::Writable { id: stream_id })
        );

        // Check that our full send window is available again
        assert_eq!(stream.write(&data), Ok(initial_send_len));
        assert_eq!(stream.write(&data), Ok(initial_send_len));
        assert_eq!(stream.write(&data), Err(WriteError::Blocked));
    }

    #[test]
    fn shrink_send_window() {
        let mut server = make(Side::Server);

        let initial_send_window = server.send_window;
        let smaller_send_window = server.send_window / 2;

        // Set `initial_max_data` larger than `send_window` so we're limited by local flow control
        server.set_params(&TransportParameters {
            initial_max_data: VarInt::MAX,
            initial_max_stream_data_uni: VarInt::MAX,
            initial_max_streams_uni: VarInt::from_u32(100),
            ..TransportParameters::default()
        });

        assert_eq!(server.write_limit(), initial_send_window);
        assert_eq!(server.poll(), None);

        let mut retransmits = Retransmits::default();
        let conn_state = ConnState::Established;

        let stream_id = Streams {
            state: &mut server,
            conn_state: &conn_state,
        }
        .open(Dir::Uni)
        .expect("should be able to open a stream");

        let mut stream = SendStream {
            id: stream_id,
            state: &mut server,
            pending: &mut retransmits,
            conn_state: &conn_state,
        };

        let initial_send_len = initial_send_window as usize;

        let data = vec![0xFFu8; initial_send_len];

        // Assert that the full send window is accepted
        assert_eq!(stream.write(&data), Ok(initial_send_len));
        assert_eq!(stream.write(&data), Err(WriteError::Blocked));

        assert_eq!(stream.state.write_limit(), 0);
        assert_eq!(stream.state.poll(), None);

        // Shrink our send window, assert that it's still not writable
        stream.state.set_send_window(smaller_send_window);
        assert_eq!(stream.state.write_limit(), 0);
        assert_eq!(stream.state.poll(), None);

        // Assert that data is still not accepted
        assert_eq!(stream.write(&data), Err(WriteError::Blocked));

        // Ack some data, assert that writes are still not accepted due to outstanding sends
        stream.state.received_ack_of(frame::StreamMeta {
            id: stream_id,
            offsets: 0..smaller_send_window,
            fin: false,
        });

        assert_eq!(stream.write(&data), Err(WriteError::Blocked));

        // Ack the rest of the data
        stream.state.received_ack_of(frame::StreamMeta {
            id: stream_id,
            offsets: smaller_send_window..initial_send_window,
            fin: false,
        });

        // This should generate a `Writable` event
        assert_eq!(
            stream.state.poll(),
            Some(StreamEvent::Writable { id: stream_id })
        );
        assert_eq!(stream.state.write_limit(), smaller_send_window);

        // Assert that only `smaller_send_window` bytes are accepted
        assert_eq!(stream.write(&data), Ok(smaller_send_window as usize));
        assert_eq!(stream.write(&data), Err(WriteError::Blocked));
    }

    #[test]
    fn max_stream_data_over_stream_limit() {
        let mut server = make(Side::Server);
        // The limit is 128 streams, so the client may open the streams 0 to 127.
        let id = StreamId::new(Side::Client, Dir::Bi, 1 << 40);
        let result = server.received_max_stream_data(id, 1);
        assert_eq!(
            result.map_err(|e| e.code),
            Err(TransportErrorCode::STREAM_LIMIT_ERROR),
            "next_remote is now {}",
            server.next_remote[Dir::Bi as usize]
        );
    }

    fn reset_at(
        id: StreamId,
        error_code: u32,
        final_offset: u32,
        reliable_size: Option<u32>,
    ) -> frame::ResetStream {
        frame::ResetStream {
            id,
            error_code: error_code.into(),
            final_offset: final_offset.into(),
            reliable_size: reliable_size.map(VarInt::from_u32),
        }
    }

    fn receive(state: &mut StreamsState, id: StreamId, data: std::ops::Range<u64>) {
        let len = (data.end - data.start) as usize;
        let _ = state
            .received(
                frame::Stream {
                    id,
                    offset: data.start,
                    fin: false,
                    data: Bytes::from(vec![0; len]),
                },
                len,
            )
            .unwrap();
    }

    /// Read everything readable, returning the byte count and how the read stopped
    fn drain(state: &mut StreamsState, id: StreamId, ordered: bool) -> (u64, crate::ReadError) {
        let mut pending = Retransmits::default();
        let mut recv = RecvStream {
            id,
            state,
            pending: &mut pending,
        };
        let mut chunks = recv.read(ordered).unwrap();
        let mut read = 0;
        let err = loop {
            match chunks.next(usize::MAX) {
                Ok(Some(chunk)) => read += chunk.bytes.len() as u64,
                Ok(None) => panic!("reset stream finished"),
                Err(e) => break e,
            }
        };
        let _ = chunks.finalize();
        (read, err)
    }

    #[test]
    fn reset_at_credits_abandoned_data() {
        let mut client = make(Side::Client);
        let id = StreamId::new(Side::Server, Dir::Uni, 0);
        let initial_max = client.local_max_data;
        receive(&mut client, id, 0..4);

        // Everything past the Reliable Size is credited at once
        let _ = client
            .received_reset(reset_at(id, 7, 100, Some(10)))
            .unwrap();
        assert_eq!(client.data_recvd, 100);
        assert_eq!(client.local_max_data - initial_max, 90);

        // Data past the Reliable Size is dropped without counting twice
        receive(&mut client, id, 4..20);
        assert_eq!(client.data_recvd, 100);
        assert_eq!(
            drain(&mut client, id, true),
            (10, crate::ReadError::Reset(7u32.into()))
        );
        assert_eq!(client.local_max_data - initial_max, 100);
    }

    #[test]
    fn reset_at_withholds_error_until_prefix_arrives() {
        let mut client = make(Side::Client);
        let id = StreamId::new(Side::Server, Dir::Uni, 0);
        let initial_max = client.local_max_data;
        let _ = client
            .received_reset(reset_at(id, 7, 20, Some(10)))
            .unwrap();
        receive(&mut client, id, 4..20);
        assert_eq!(drain(&mut client, id, true), (0, crate::ReadError::Blocked));
        receive(&mut client, id, 0..4);
        assert_eq!(
            drain(&mut client, id, true),
            (10, crate::ReadError::Reset(7u32.into()))
        );
        assert_eq!(client.local_max_data - initial_max, 20);
    }

    #[test]
    fn reset_at_unordered_reads() {
        let mut client = make(Side::Client);
        let id = StreamId::new(Side::Server, Dir::Uni, 0);
        let initial_max = client.local_max_data;
        receive(&mut client, id, 5..10);
        receive(&mut client, id, 0..2);
        assert_eq!(
            drain(&mut client, id, false),
            (7, crate::ReadError::Blocked)
        );

        // Only 2..4 of the prefix is still owed, though bytes past it were already read
        let _ = client.received_reset(reset_at(id, 7, 20, Some(4))).unwrap();
        assert_eq!(client.local_max_data - initial_max, 18);
        assert_eq!(
            drain(&mut client, id, false),
            (0, crate::ReadError::Blocked)
        );
        receive(&mut client, id, 2..4);
        assert_eq!(
            drain(&mut client, id, false),
            (2, crate::ReadError::Reset(7u32.into()))
        );
        assert_eq!(client.local_max_data - initial_max, 20);
    }

    #[test]
    fn reset_at_shrinks_reliable_size() {
        let mut client = make(Side::Client);
        let id = StreamId::new(Side::Server, Dir::Uni, 0);
        let initial_max = client.local_max_data;
        let _ = client
            .received_reset(reset_at(id, 7, 20, Some(10)))
            .unwrap();
        let _ = client.received_reset(reset_at(id, 7, 20, Some(4))).unwrap();
        assert_eq!(client.local_max_data - initial_max, 16);
        // A reordered, larger Reliable Size is ignored
        assert_eq!(
            client.received_reset(reset_at(id, 7, 20, Some(8))),
            Ok(ShouldTransmit(false))
        );
        assert_eq!(client.local_max_data - initial_max, 16);
        receive(&mut client, id, 0..20);
        assert_eq!(
            drain(&mut client, id, true),
            (4, crate::ReadError::Reset(7u32.into()))
        );
        assert_eq!(client.local_max_data - initial_max, 20);
    }

    /// A RESET_STREAM after a RESET_STREAM_AT abandons the prefix too.
    #[test]
    fn reset_after_reset_at() {
        let mut client = make(Side::Client);
        let id = StreamId::new(Side::Server, Dir::Uni, 0);
        let initial_max = client.local_max_data;
        receive(&mut client, id, 0..2);
        let _ = client
            .received_reset(reset_at(id, 7, 20, Some(10)))
            .unwrap();
        let _ = client.received_reset(reset_at(id, 7, 20, None)).unwrap();
        assert_eq!(
            drain(&mut client, id, true),
            (0, crate::ReadError::Reset(7u32.into()))
        );
        assert_eq!(client.local_max_data - initial_max, 20);
    }

    #[test]
    fn reset_at_error_code_change() {
        let mut client = make(Side::Client);
        let id = StreamId::new(Side::Server, Dir::Uni, 0);
        let _ = client
            .received_reset(reset_at(id, 7, 20, Some(10)))
            .unwrap();
        assert_eq!(
            client
                .received_reset(reset_at(id, 8, 20, Some(4)))
                .map_err(|e| e.code),
            Err(TransportErrorCode::STREAM_STATE_ERROR)
        );
    }

    #[test]
    fn reset_at_final_size_change() {
        let mut client = make(Side::Client);
        let id = StreamId::new(Side::Server, Dir::Uni, 0);
        let _ = client
            .received_reset(reset_at(id, 7, 20, Some(10)))
            .unwrap();
        assert_eq!(
            client
                .received_reset(reset_at(id, 7, 21, Some(4)))
                .map_err(|e| e.code),
            Err(TransportErrorCode::FINAL_SIZE_ERROR)
        );
        let mut client = make(Side::Client);
        let _ = client
            .received_reset(reset_at(id, 7, 20, Some(10)))
            .unwrap();
        assert_eq!(
            client
                .received(
                    frame::Stream {
                        id,
                        offset: 20,
                        fin: false,
                        data: Bytes::from_static(&[0]),
                    },
                    1,
                )
                .map_err(|e| e.code),
            Err(TransportErrorCode::FINAL_SIZE_ERROR)
        );
    }

    /// Stopping a reset stream only credits what the reset still owed, rather than crediting the
    /// unread data a second time.
    #[test]
    fn stop_after_reset_credits_once() {
        for reliable_size in [None, Some(15)] {
            let mut client = make(Side::Client);
            let id = StreamId::new(Side::Server, Dir::Uni, 0);
            let initial_max = client.local_max_data;
            receive(&mut client, id, 0..10);
            let _ = client
                .received_reset(reset_at(id, 7, 20, reliable_size))
                .unwrap();
            let mut pending = Retransmits::default();
            RecvStream {
                id,
                state: &mut client,
                pending: &mut pending,
            }
            .stop(0u32.into())
            .unwrap();
            assert_eq!(client.local_max_data - initial_max, 20, "{reliable_size:?}");
        }
    }

    fn reset_at_server() -> StreamsState {
        server_with_reset_stream_at(true)
    }

    fn server_with_reset_stream_at(reset_stream_at: bool) -> StreamsState {
        let mut server = make(Side::Server);
        server.set_params(&TransportParameters {
            initial_max_streams_uni: 1u32.into(),
            initial_max_data: 42u32.into(),
            initial_max_stream_data_uni: 42u32.into(),
            reset_stream_at,
            ..TransportParameters::default()
        });
        server
    }

    /// Without RESET_STREAM_AT the reset is held until the prefix is acknowledged, and only the
    /// prefix keeps counting against the send window meanwhile.
    #[test]
    fn held_reset_waits_for_prefix_ack() {
        let mut server = server_with_reset_stream_at(false);
        let (mut pending, state) = (Retransmits::default(), ConnState::Established);
        let id = Streams {
            state: &mut server,
            conn_state: &state,
        }
        .open(Dir::Uni)
        .unwrap();
        let mut stream = SendStream {
            id,
            state: &mut server,
            pending: &mut pending,
            conn_state: &state,
        };
        stream.write(b"hello world").unwrap();
        let mut buf = Vec::new();
        let sent = stream.state.write_stream_frames(&mut buf, 100, true);
        stream.reset_to(7u32.into(), 4).unwrap();
        assert!(pending.reset_stream.is_empty());
        assert_eq!(server.buffered_data, 4);

        // The prefix is lost and resent, without what followed it
        server.retransmit(sent[0].clone());
        buf.clear();
        let resent = server.write_stream_frames(&mut buf, 100, true);
        assert_eq!(resent[0].offsets, 0..4);
        assert_eq!(server.received_ack_of(resent[0].clone()), Some(4));
        assert_eq!(server.buffered_data, 0);

        // The plain RESET_STREAM still names the full final size
        let send = server.send[&id].as_ref().unwrap();
        let frame = send.reset_frame(id, false).unwrap();
        assert_eq!(frame.reliable_size, None);
        assert_eq!(frame.final_offset, VarInt(11));

        // The stream goes once the reset is acknowledged
        server.reset_acked(id, 4);
        assert_eq!(server.send_streams, 0);
    }

    /// A late acknowledgement of a RESET_STREAM_AT that STOP_SENDING has since shrunk leaves the
    /// stream for its zero-sized replacement, which must still be retransmitted until acknowledged.
    #[test]
    fn stale_reset_ack_keeps_shrunk_reset() {
        let mut server = reset_at_server();
        let (mut pending, state) = (Retransmits::default(), ConnState::Established);
        let id = Streams {
            state: &mut server,
            conn_state: &state,
        }
        .open(Dir::Uni)
        .unwrap();
        let mut stream = SendStream {
            id,
            state: &mut server,
            pending: &mut pending,
            conn_state: &state,
        };
        stream.write(b"hello world").unwrap();
        stream.reset_to(7u32.into(), 4).unwrap();
        assert_eq!(pending.reset_stream, &[(id, 4)]);
        assert_eq!(server.received_stop_sending(id, 9u32.into()), Some(0));

        server.reset_acked(id, 4);
        assert_eq!(server.send_streams, 1);
        server.reset_acked(id, 0);
        assert_eq!(server.send_streams, 0);
    }

    /// A RESET_STREAM_AT acknowledged before STOP_SENDING shrinks it no longer counts, so a late
    /// acknowledgement of its prefix leaves the stream for the zero-sized replacement.
    #[test]
    fn acked_reset_shrunk_waits_for_replacement() {
        let mut server = reset_at_server();
        let (mut pending, state) = (Retransmits::default(), ConnState::Established);
        let id = Streams {
            state: &mut server,
            conn_state: &state,
        }
        .open(Dir::Uni)
        .unwrap();
        let mut stream = SendStream {
            id,
            state: &mut server,
            pending: &mut pending,
            conn_state: &state,
        };
        stream.write(b"hello world").unwrap();
        let mut buf = Vec::new();
        let sent = stream.state.write_stream_frames(&mut buf, 100, true);
        stream.reset_to(7u32.into(), 4).unwrap();

        server.reset_acked(id, 4);
        assert_eq!(server.received_stop_sending(id, 9u32.into()), Some(0));
        assert_eq!(server.received_ack_of(sent[0].clone()), None);
        assert_eq!(server.send_streams, 1);
        server.reset_acked(id, 0);
        assert_eq!(server.send_streams, 0);
    }

    /// A Reliable Size past the Final Size is a FRAME_ENCODING_ERROR.
    #[test]
    fn reset_at_beyond_final_size() {
        let mut client = make(Side::Client);
        let id = StreamId::new(Side::Server, Dir::Uni, 0);
        let err = client
            .received_reset(reset_at(id, 7, 9, Some(10)))
            .unwrap_err();
        assert_eq!(err.code, TransportErrorCode::FRAME_ENCODING_ERROR);
    }

    /// A peer that asks us to stop sending will not read the prefix a reset still delivers, so
    /// a held RESET_STREAM goes at once and a RESET_STREAM_AT is replaced by a zero-sized reset.
    #[test]
    fn stop_sending_abandons_reliable_prefix() {
        for reset_stream_at in [false, true] {
            let mut server = server_with_reset_stream_at(reset_stream_at);
            let (mut pending, state) = (Retransmits::default(), ConnState::Established);
            let id = Streams {
                state: &mut server,
                conn_state: &state,
            }
            .open(Dir::Uni)
            .unwrap();
            let mut stream = SendStream {
                id,
                state: &mut server,
                pending: &mut pending,
                conn_state: &state,
            };
            stream.write(b"hello world").unwrap();
            stream.reset_to(7u32.into(), 4).unwrap();
            assert_eq!(pending.reset_stream.is_empty(), !reset_stream_at);

            assert_eq!(server.received_stop_sending(id, 9u32.into()), Some(0));
            assert_eq!(server.buffered_data, 0);
            assert!(!server.can_send_stream_data());
            let send = server.send[&id].as_ref().unwrap();
            assert!(send.pending.is_fully_acked());
            let frame = send.reset_frame(id, reset_stream_at).unwrap();
            assert_eq!(frame.reliable_size, None);
            assert_eq!(frame.final_offset, VarInt(11));
            // Abandoned once, so a repeated STOP_SENDING sends nothing more
            assert_eq!(server.received_stop_sending(id, 9u32.into()), None);
        }
    }

    #[test]
    fn reset_at_retains_prefix_until_acked() {
        let mut server = reset_at_server();
        let (mut pending, state) = (Retransmits::default(), ConnState::Established);
        let id = Streams {
            state: &mut server,
            conn_state: &state,
        }
        .open(Dir::Uni)
        .unwrap();
        let mut stream = SendStream {
            id,
            state: &mut server,
            pending: &mut pending,
            conn_state: &state,
        };
        stream.write(b"hello world").unwrap();
        let mut buf = Vec::new();
        let sent = stream.state.write_stream_frames(&mut buf, 100, true);
        assert_eq!(sent[0].offsets, 0..11);
        stream.reset_to(7u32.into(), 4).unwrap();
        assert_eq!(server.buffered_data, 4);

        // Only the lost prefix is retransmitted, without the FIN of a stream that was never finished
        server.retransmit(sent[0].clone());
        buf.clear();
        let resent = server.write_stream_frames(&mut buf, 100, true);
        assert_eq!(resent.len(), 1);
        assert_eq!(resent[0].offsets, 0..4);
        assert!(!resent[0].fin);
        assert!(!server.can_send_stream_data());

        // The stream lingers until both the reset and the prefix are acknowledged
        server.reset_acked(id, 4);
        assert_eq!(server.send_streams, 1);
        server.received_ack_of(resent[0].clone());
        assert_eq!(server.send_streams, 0);
        assert_eq!(server.buffered_data, 0);
        assert_eq!(server.poll(), None);
    }

    #[test]
    fn reset_at_after_stop_sending_drops_prefix() {
        let mut server = reset_at_server();
        let (mut pending, state) = (Retransmits::default(), ConnState::Established);
        let id = Streams {
            state: &mut server,
            conn_state: &state,
        }
        .open(Dir::Uni)
        .unwrap();
        let mut stream = SendStream {
            id,
            state: &mut server,
            pending: &mut pending,
            conn_state: &state,
        };
        stream.write(b"hello world").unwrap();
        stream.state.received_stop_sending(id, 9u32.into());
        stream.reset_to(7u32.into(), 4).unwrap();

        let send = server.send[&id].as_ref().unwrap();
        let frame = send.reset_frame(id, true).unwrap();
        assert_eq!(frame.reliable_size, None);
        assert_eq!(frame.final_offset, VarInt(11));
        assert!(send.pending.is_fully_acked());
        assert_eq!(server.buffered_data, 0);
    }

    /// How a held stream ends in [`held_credit_returns_once`].
    #[derive(Debug, Clone, Copy)]
    enum End {
        Fin,
        Stop,
        Reset,
        StopThenReset,
    }

    fn data(client: &mut StreamsState, id: StreamId, offset: u64, len: usize, fin: bool) {
        let _ = client
            .received(
                frame::Stream {
                    id,
                    offset,
                    fin,
                    data: Bytes::from(vec![0; len]),
                },
                len,
            )
            .unwrap();
    }

    fn reset(client: &mut StreamsState, id: StreamId, final_offset: u32) {
        let _ = client
            .received_reset(frame::ResetStream {
                id,
                error_code: 0u32.into(),
                final_offset: final_offset.into(),
                reliable_size: None,
            })
            .unwrap();
    }

    /// Read everything readable now, holding its connection credit.
    fn read_held(client: &mut StreamsState, id: StreamId) -> Credit {
        let mut pending = Retransmits::default();
        let mut recv = RecvStream {
            id,
            state: client,
            pending: &mut pending,
        };
        let mut chunks = recv.read(true).unwrap();
        while let Ok(Some(_)) = chunks.next(usize::MAX) {}
        let credit = chunks.hold();
        let _ = chunks.finalize();
        credit
    }

    /// Every byte of a stream returns its connection credit exactly once, whichever terminal path
    /// it takes and whether the held bytes are released before or after its state is reclaimed.
    #[test]
    fn held_credit_returns_once() {
        for end in [End::Fin, End::Stop, End::Reset, End::StopThenReset] {
            let mut client = make(Side::Client);
            let initial = client.local_max_data;
            let id = StreamId::new(Side::Server, Dir::Uni, 0);

            // A readable prefix, then a gap the transport must account for itself.
            data(&mut client, id, 0, 32, false);
            data(&mut client, id, 64, 16, false);
            let credit = read_held(&mut client, id);
            assert_eq!(credit.len(), 32, "{end:?}");
            assert_eq!(client.credit_stats().held, 32, "{end:?}");
            assert_eq!(
                client.local_max_data, initial,
                "{end:?}: held, not returned"
            );

            let size = match end {
                End::Fin => {
                    data(&mut client, id, 32, 32, false);
                    data(&mut client, id, 80, 16, true);
                    let rest = read_held(&mut client, id);
                    assert_eq!(rest.len(), 64, "{end:?}");
                    let _ = client.release(rest);
                    96
                }
                End::Stop => {
                    let mut pending = Retransmits::default();
                    RecvStream {
                        id,
                        state: &mut client,
                        pending: &mut pending,
                    }
                    .stop(0u32.into())
                    .unwrap();
                    // Late data below and beyond the stop is credited on arrival.
                    data(&mut client, id, 32, 32, false);
                    data(&mut client, id, 80, 16, true);
                    96
                }
                End::Reset => {
                    reset(&mut client, id, 128);
                    let mut pending = Retransmits::default();
                    let mut recv = RecvStream {
                        id,
                        state: &mut client,
                        pending: &mut pending,
                    };
                    let mut chunks = recv.read(true).unwrap();
                    assert!(chunks.next(usize::MAX).is_err(), "{end:?}");
                    let _ = chunks.finalize();
                    128
                }
                End::StopThenReset => {
                    let mut pending = Retransmits::default();
                    RecvStream {
                        id,
                        state: &mut client,
                        pending: &mut pending,
                    }
                    .stop(0u32.into())
                    .unwrap();
                    reset(&mut client, id, 128);
                    // A duplicate reset after reclamation changes nothing.
                    reset(&mut client, id, 128);
                    128
                }
            };
            assert!(
                !client.recv.contains_key(&id),
                "{end:?}: state reclaimed while credit is held"
            );
            assert_eq!(
                client.local_max_data,
                initial + size - 32,
                "{end:?}: everything but the held prefix"
            );

            let _ = client.release(credit);
            assert_eq!(client.local_max_data, initial + size, "{end:?}");
            assert_eq!(client.credit_stats().held, 0, "{end:?}");
        }
    }

    #[test]
    fn held_credit_pays_shrink_debt() {
        let mut client = make(Side::Client);
        let initial = client.local_max_data;
        let id = StreamId::new(Side::Server, Dir::Uni, 0);
        data(&mut client, id, 0, 32, false);
        let credit = read_held(&mut client, id);
        client.set_receive_window((client.receive_window - 16).try_into().unwrap());
        let _ = client.release(credit);
        assert_eq!(client.local_max_data, initial + 16);
    }

    /// Another connection holding at least as much credit still refuses it.
    #[test]
    #[should_panic(expected = "released credit from another connection")]
    fn release_from_another_connection() {
        let id = StreamId::new(Side::Server, Dir::Uni, 0);
        let mut a = make(Side::Client);
        data(&mut a, id, 0, 32, false);
        let credit = read_held(&mut a, id);
        let mut b = make(Side::Client);
        data(&mut b, id, 0, 32, false);
        let _held = read_held(&mut b, id);
        let _ = b.release(credit);
    }

    #[test]
    #[should_panic(expected = "merged credit from another connection")]
    fn merge_from_another_connection() {
        let id = StreamId::new(Side::Server, Dir::Uni, 0);
        let mut a = make(Side::Client);
        data(&mut a, id, 0, 32, false);
        let mut b = make(Side::Client);
        data(&mut b, id, 0, 32, false);
        let mut credit = read_held(&mut a, id);
        credit.merge(read_held(&mut b, id));
    }

    /// An empty default credit takes on the connection of the first credit merged into it.
    #[test]
    fn default_credit_adopts_its_connection() {
        let id = StreamId::new(Side::Server, Dir::Uni, 0);
        let mut a = make(Side::Client);
        data(&mut a, id, 0, 32, false);
        let mut credit = Credit::default();
        credit.merge(read_held(&mut a, id));
        assert_eq!(credit.len(), 32);
        let _ = a.release(credit);
        assert_eq!(a.credit_stats().held, 0);
    }

    /// Ingest and read rounds across many streams, with and without holding and releasing each
    /// read's connection credit. The held cost must stay flat as the stream count grows.
    #[test]
    #[ignore = "release benchmark, run by nightly"]
    fn benchmark_receive_credit() {
        const BYTES: usize = 1 << 30;
        println!("streams,payload,plain_ns_per_read,held_ns_per_read");
        for streams in [1u64, 64, 4096] {
            for payload in [1200usize, 64 * 1024] {
                let rounds = (BYTES / payload / streams as usize).max(1) as u64;
                let chunk = Bytes::from(vec![0; payload]);
                let mut ns = [0f64; 2];
                for (mode, held) in [false, true].into_iter().enumerate() {
                    let mut client = StreamsState::new(
                        Side::Client,
                        VarInt::from_u64(streams).unwrap(),
                        0u32.into(),
                        0,
                        VarInt::from_u64(1 << 50).unwrap(),
                        VarInt::from_u64(1 << 40).unwrap(),
                    );
                    let mut pending = Retransmits::default();
                    let start = std::time::Instant::now();
                    for round in 0..rounds {
                        for index in 0..streams {
                            let id = StreamId::new(Side::Server, Dir::Uni, index);
                            let frame = frame::Stream {
                                id,
                                offset: round * payload as u64,
                                fin: false,
                                data: chunk.clone(),
                            };
                            let _ = client.received(frame, payload).unwrap();
                            let mut recv = RecvStream {
                                id,
                                state: &mut client,
                                pending: &mut pending,
                            };
                            let mut chunks = recv.read(true).unwrap();
                            std::hint::black_box(chunks.next(usize::MAX).unwrap());
                            if held {
                                let credit = chunks.hold();
                                let _ = chunks.finalize();
                                let _ = client.release(credit);
                            } else {
                                let _ = chunks.finalize();
                            }
                        }
                    }
                    ns[mode] = start.elapsed().as_nanos() as f64 / (rounds * streams) as f64;
                }
                println!("{streams},{payload},{:.1},{:.1}", ns[0], ns[1]);
            }
        }
    }
}
