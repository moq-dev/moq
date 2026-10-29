use std::{
	collections::{BTreeSet, VecDeque, hash_map},
	convert::TryFrom,
	mem,
	ops::Range,
};

use bytes::BufMut;
use indexmap::IndexSet;
use rustc_hash::{FxBuildHasher, FxHashMap, FxHashSet};
use tracing::{debug, trace};

use super::{
	Next, Recv, Retransmits, Schedule, Send, SendState, ShouldTransmit, StreamEvent, StreamHalf, ThinRetransmits,
	lease::Rank,
};
use crate::{
	Dir, MAX_STREAM_COUNT, Side, StreamId, TransportError, VarInt,
	coding::BufMutExt,
	connection::ConnectionTag,
	connection::stats::{CreditStats, FrameStats},
	frame::{self, FrameStruct, StreamMetaVec},
	transport_parameters::TransportParameters,
};

/// The frames [`StreamsState::write_frames`] wrote into a packet.
#[derive(Debug, Default)]
pub(crate) struct WrittenFrames {
	/// The STREAM frames, which are retransmitted when lost.
	pub(crate) streams: StreamMetaVec,
	/// How many DATAGRAM frames, which are not.
	pub(crate) datagrams: u64,
}

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

	fn inner_mut(&mut self) -> &mut Recv {
		match self {
			Self::Free(r) | Self::Open(r) => r,
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

pub(crate) struct StreamsState {
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
	/// Highest `max_remote` sent in transport parameters or a `MAX_STREAMS` frame
	sent_max_remote: [u64; 2],
	/// Number of streams that we've given the peer permission to open and which aren't fully closed
	pub(super) allocated_remote_count: [u64; 2],
	/// Size of the desired stream flow control window. May be smaller than `allocated_remote_count`
	/// due to `set_max_concurrent` calls.
	max_concurrent_remote_count: [u64; 2],
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
	/// Streams with outgoing data queued and outgoing datagrams, in the order to send them
	pub(super) pending: Schedule<StreamId, frame::Datagram>,

	pub(super) events: VecDeque<StreamEvent>,
	/// Streams blocked on connection-level flow control or stream window space
	///
	/// Deduplicated waiters with constant-time removal. Cancellation may change wake order.
	pub(super) connection_blocked: IndexSet<StreamId, FxBuildHasher>,
	/// Connection-level flow control budget dictated by the peer
	pub(super) max_data: u64,
	/// The initial receive window
	pub(super) receive_window: u64,
	/// Limit on incoming data, which is transmitted through `MAX_DATA` frames
	pub(super) local_max_data: u64,
	/// The last value of `MAX_DATA` which had been queued for transmission in
	/// an outgoing `MAX_DATA` frame
	sent_max_data: VarInt,
	/// Sum of current offsets of all send streams.
	pub(super) data_sent: u64,
	/// Sum of end offsets of all receive streams. Includes gaps, so it's an upper bound.
	pub(super) data_recvd: u64,
	/// Bytes leased from receive streams, taken or not, whose connection credit the application
	/// has not released yet
	pub(super) held: u64,
	/// Leased receive streams that may hold connection credit, in eviction order: every such
	/// stream, and possibly some that no longer do, dropped as the stuck pass reaches them
	pub(super) ranks: BTreeSet<Rank>,
	/// Streams evicted to free a stuck receive window, ever
	pub(super) evicted: u64,
	/// The most credit the peer has had spent at once
	pub(super) peak_spent: u64,
	/// Receive streams whose reader may progress without new credit, which holds eviction off:
	/// every such stream, and possibly some that no longer are, trimmed by the stuck pass
	pub(super) unlooked: FxHashSet<StreamId>,
	/// Credit still held by evicted readers that have not stopped their streams, which holds
	/// further eviction off
	pub(super) evicted_held: u64,
	/// Streams the stuck pass has looked at, to prove it stays cheap
	#[cfg(test)]
	pub(super) scans: usize,
	/// Which connection the send groups handed out here belong to
	pub(super) tag: ConnectionTag,
	/// Total quantity of outgoing data retained in send buffers, including acknowledged data
	pub(super) buffered_data: u64,
	/// Configured upper bound for `buffered_data`.
	///
	/// Note this may be less than `buffered_data` if the user has set a new value.
	pub(super) send_window: u64,
	/// Configured upper bound for how much unacked data the peer can send us per stream
	pub(super) stream_receive_window: u64,
	/// The stream receive window our transport parameters announced, which is the credit every
	/// new stream starts with whatever [`stream_receive_window`](Self::stream_receive_window)
	/// became since
	pub(super) initial_stream_receive_window: u64,

	// Pertinent state from the TransportParameters supplied by the peer
	initial_max_stream_data_uni: VarInt,
	initial_max_stream_data_bidi_local: VarInt,
	initial_max_stream_data_bidi_remote: VarInt,
	/// Whether the peer accepts RESET_STREAM_AT
	pub(super) peer_reset_stream_at: bool,

	/// The shrink to be applied to local_max_data when receive_window is shrunk
	receive_window_shrink_debt: u64,
	/// Whether the locally-initiated stream limit has been hit, per direction
	pub(super) streams_blocked: [bool; 2],
	/// Whether an open was refused since the stream limit last rose, per direction
	pub(super) open_refused: [bool; 2],
	/// Value of `max_data` for which a `DATA_BLOCKED` frame was most recently queued
	///
	/// A new frame is only queued once the peer has raised the limit.
	pub(super) data_blocked_limit: Option<u64>,
}

impl StreamsState {
	pub(crate) fn new(
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
			next_remote: [0, 0],
			opened: [false, false],
			next_reported_remote: [0, 0],
			send_streams: 0,
			pending: Schedule::new(),
			events: VecDeque::new(),
			connection_blocked: IndexSet::default(),
			max_data: 0,
			receive_window: receive_window.into(),
			local_max_data: receive_window.into(),
			sent_max_data: receive_window,
			data_sent: 0,
			data_recvd: 0,
			held: 0,
			evicted: 0,
			peak_spent: 0,
			unlooked: FxHashSet::default(),
			evicted_held: 0,
			#[cfg(test)]
			scans: 0,
			ranks: BTreeSet::new(),
			tag: ConnectionTag::next(),
			buffered_data: 0,
			send_window,
			stream_receive_window: stream_receive_window.into(),
			initial_stream_receive_window: stream_receive_window.into(),
			initial_max_stream_data_uni: 0u32.into(),
			initial_max_stream_data_bidi_local: 0u32.into(),
			initial_max_stream_data_bidi_remote: 0u32.into(),
			peer_reset_stream_at: false,
			receive_window_shrink_debt: 0,
			streams_blocked: [false, false],
			open_refused: [false, false],
			data_blocked_limit: None,
		}
	}

	pub(crate) fn set_params(&mut self, params: &TransportParameters) {
		self.initial_max_stream_data_uni = params.initial_max_stream_data_uni;
		self.initial_max_stream_data_bidi_local = params.initial_max_stream_data_bidi_local;
		self.initial_max_stream_data_bidi_remote = params.initial_max_stream_data_bidi_remote;
		self.peer_reset_stream_at = params.reset_stream_at;
		for (dir, count) in [
			(Dir::Bi, params.initial_max_streams_bidi),
			(Dir::Uni, params.initial_max_streams_uni),
		] {
			let count = count.into();
			// A caller can attempt an open before the peer's limit is known.
			if count > self.max[dir as usize] && mem::take(&mut self.open_refused[dir as usize]) {
				self.events.push_back(StreamEvent::Available { dir });
			}
			self.max[dir as usize] = count;
		}
		self.received_max_data(params.initial_max_data);
	}

	/// Ensure we have space for at least a full flow control window of remotely-initiated streams
	/// to be open, and notify the peer if the window has moved
	fn ensure_remote_streams(&mut self, dir: Dir) {
		let new_count =
			self.max_concurrent_remote_count[dir as usize].saturating_sub(self.allocated_remote_count[dir as usize]);
		self.allocated_remote_count[dir as usize] += new_count;
		self.max_remote[dir as usize] += new_count;
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
			.map(|opt| get_or_insert_recv(opt, self.initial_stream_receive_window))
		else {
			trace!("dropping frame for closed stream");
			return Ok(ShouldTransmit(false));
		};

		if !rs.is_receiving() {
			trace!("dropping frame for finished stream");
			return Ok(ShouldTransmit(false));
		}

		let (new_bytes, closed) = rs.ingest(frame, payload_len, self.data_recvd, self.local_max_data)?;
		let (stopped, evicted) = (rs.stopped, rs.lease.evicted);
		let rank = (!stopped).then(|| rs.lease.arrived(id)).flatten();
		self.spend(new_bytes);

		let transmit = if !stopped {
			self.events.push_back(StreamEvent::Readable { id });
			self.ranks.extend(rank);
			self.unlooked.insert(id);
			// Credit released while the peer still had room may not have been worth a frame
			// then; it is now that the peer is about to block.
			ShouldTransmit(self.max_data_due())
		} else {
			// Stopped streams become closed instantly on FIN, so check whether we need to clean up.
			// An evicted one waits for the application's stop, which still holds its lease.
			if closed && !evicted {
				let rs = self.recv.remove(&id).flatten().unwrap();
				// Its stop already discarded the lease, so this returns no credit.
				let _ = self.stream_recv_freed(id, rs);
			}

			// We don't buffer data on stopped streams, so issue flow control credit immediately
			self.add_read_credits(new_bytes)
		};
		if new_bytes == 0 {
			// Neither spent nor returned credit, so a stuck window stays as it was, and a peer at
			// its limit cannot make every repeated frame pay for a scan of every stream.
			return Ok(transmit);
		}
		Ok(self.evict_after_arrival(transmit))
	}

	/// Count `new_bytes` of connection credit the peer spent.
	fn spend(&mut self, new_bytes: u64) {
		self.data_recvd = self.data_recvd.saturating_add(new_bytes);
		// Credit only returns between arrivals, so an arrival is when the most is spent. A shrink
		// leaves the advertised limit in place as debt, so that credit is still outstanding.
		let spent = self
			.data_recvd
			.saturating_add(self.receive_window)
			.saturating_add(self.receive_window_shrink_debt)
			.saturating_sub(self.local_max_data);
		self.peak_spent = self.peak_spent.max(spent);
	}

	/// Rerun the stuck check after an arrival, which may spend the last of the window or return
	/// credit that only pays shrink debt, adding any `MAX_DATA` it frees to `transmit`.
	fn evict_after_arrival(&mut self, transmit: ShouldTransmit) -> ShouldTransmit {
		let evicted = self.evict_if_stuck();
		ShouldTransmit(transmit.should_transmit() || evicted.is_some_and(ShouldTransmit::should_transmit))
	}

	/// Process incoming RESET_STREAM frame
	///
	/// If successful, returns whether a `MAX_DATA` frame needs to be transmitted
	pub(crate) fn received_reset(&mut self, frame: frame::ResetStream) -> Result<ShouldTransmit, TransportError> {
		let frame::ResetStream {
			id,
			error_code,
			final_offset,
			reliable_size,
		} = frame;
		if reliable_size > Some(final_offset) {
			return Err(TransportError::FRAME_ENCODING_ERROR("reliable size exceeds final size"));
		}
		self.validate_receive_id(id).inspect_err(|_| {
			debug!("received illegal RESET_STREAM frame");
		})?;

		self.insert_remote(id);

		let Some(rs) = self
			.recv
			.get_mut(&id)
			.map(|opt| get_or_insert_recv(opt, self.initial_stream_receive_window))
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
		let (stopped, evicted) = (rs.stopped, rs.lease.evicted);
		let rank = (!stopped).then(|| rs.lease.arrived(id)).flatten();
		self.spend(new_bytes);
		let credit = if stopped {
			// Stopped streams should be disposed immediately on reset, and have already returned
			// read credits up to their highest offset. An evicted one waits for its stop.
			if !evicted {
				let rs = self.recv.remove(&id).flatten().unwrap();
				// Its stop already discarded the lease, so this returns no credit.
				let _ = self.stream_recv_freed(id, rs);
			}
			new_bytes
		} else {
			self.events.push_back(StreamEvent::Readable { id });
			self.ranks.extend(rank);
			self.unlooked.insert(id);
			freed
		};

		// Update connection-level flow control
		let transmit = match credit {
			0 => ShouldTransmit(false),
			credit => self.add_read_credits(credit),
		};
		if new_bytes == 0 && credit == 0 {
			return Ok(transmit);
		}
		Ok(self.evict_after_arrival(transmit))
	}

	/// Process incoming `STOP_SENDING` frame
	///
	/// Returns the Reliable Size of 0 for a reset to send now, since the peer will not read the
	/// data a reset still delivers.
	pub(crate) fn received_stop_sending(
		&mut self,
		id: StreamId,
		error_code: VarInt,
	) -> Result<Option<u64>, TransportError> {
		self.validate_send_id(id).inspect_err(|_| {
			debug!("received illegal STOP_SENDING frame");
		})?;

		self.insert_remote(id);

		let max_send_data = self.max_send_data(id);
		let Some(stream_opt) = self.send.get_mut(&id) else {
			return Ok(None);
		};
		let stream = stream_opt.get_or_insert_with(|| Send::new(max_send_data));

		self.connection_blocked.swap_remove(&id);
		if stream.try_stop(error_code) {
			self.events.push_back(StreamEvent::Stopped { id, error_code });
		}
		let buffered = stream.pending.buffered();
		let reset = stream.abandon_reliable();
		self.buffered_data -= buffered - stream.pending.buffered();
		if let Some(offset) = stream.take_waiting() {
			self.events.push_back(StreamEvent::Acked { id, offset });
		}
		if !stream.is_pending() {
			self.pending.remove(&mut stream.slot);
		}
		Ok(reset)
	}

	/// Handle acknowledgement of a reset frame sent while the Reliable Size was `reliable_size`
	pub(crate) fn reset_acked(&mut self, id: StreamId, reliable_size: u64) {
		let hash_map::Entry::Occupied(mut e) = self.send.entry(id) else {
			return;
		};
		// A reliable reset lingers until the data below its Reliable Size is acknowledged too, and
		// one since shrunk by STOP_SENDING until its replacement is
		if e.get_mut().as_mut().is_some_and(|s| s.reset_acked(reliable_size)) {
			if let (_, Some(mut stream)) = e.remove_entry() {
				self.pending.leave(&mut stream.slot);
			}
			self.stream_freed(id, StreamHalf::Send);
		}
	}

	/// Whether any stream data or datagram is queued, regardless of control frames
	#[cfg(test)]
	pub(crate) fn can_send_stream_data(&self) -> bool {
		!self.pending.is_empty()
	}

	/// Whether [`Self::write_frames`] would write a frame into `space` bytes: any queued
	/// stream, or a queued datagram whose frame fits
	pub(crate) fn can_send_frames(&self, space: usize) -> bool {
		self.pending.can_send(space)
	}

	/// The offset just past the data received so far on `id`
	///
	/// `None` if `id` cannot receive data now, such as a closed stream or our own
	/// unidirectional one. On an ordered transport, data at any other offset is a protocol
	/// violation.
	#[cfg(feature = "mux")]
	pub(crate) fn rx_offset(&self, id: StreamId) -> Option<u64> {
		// A peer stream not opened yet starts at offset 0 too
		if id.initiator() != self.side && id.index() >= self.next_remote[id.dir() as usize] {
			return Some(0);
		}
		let recv = self.recv.get(&id)?;
		// Present but unopened: the peer may send on it, starting at offset 0
		Some(recv.as_ref().and_then(|r| r.as_open_recv()).map_or(0, |r| r.data_end))
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
		stream
			.data_blocked_limit
			.filter(|&limit| stream.is_writable() && stream.stop_reason.is_none() && limit == stream.max_data)
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
			let (Some(frame), Some(reliable_size)) = (stream.reset_frame(id, at), stream.reliable_size()) else {
				continue;
			};
			trace!(stream = %id, reliable_size = ?frame.reliable_size, "RESET_STREAM");
			retransmits.get_or_create().reset_stream.push((id, reliable_size));
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
			trace!(value = self.max_remote[dir as usize], "MAX_STREAMS ({:?})", dir);
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

	/// Write STREAM and DATAGRAM frames in schedule order until the packet is full.
	///
	/// `payload` is the packet's frame space in `buf`, which may already hold control frames.
	/// A datagram cannot be split, so one too large for the room left ends the packet and
	/// leads the next one. One too large for even an empty packet, which a packet clamped
	/// below the path MTU can be, steps aside for what fits behind it instead, since
	/// [`Self::can_send_frames`] promised a frame and control frames alone would leave the
	/// packet short.
	pub(crate) fn write_frames(&mut self, buf: &mut Vec<u8>, payload: Range<usize>, fair: bool) -> WrittenFrames {
		let max_buf_size = payload.end;
		let mut written = WrittenFrames::default();
		while buf.len() + frame::Stream::SIZE_BOUND < max_buf_size {
			// Pop the next frame to send. A stream leaves the schedule once it has nothing
			// left to send, so a reset one stays only for data below its Reliable Size.
			let id = match self.pending.pop() {
				None => break,
				Some(Next::Stream(id)) => id,
				Some(Next::Datagram(group)) => {
					let (_, frame) = self.pending.datagram(group);
					if buf.len() + frame > max_buf_size {
						if payload.start + frame <= max_buf_size {
							break;
						}
						self.pending.defer_datagram(group);
						continue;
					}
					let datagram = self.pending.sent_datagram(group);
					trace!(len = datagram.data.len(), "DATAGRAM");
					datagram.encode(true, buf);
					written.datagrams += 1;
					continue;
				}
			};
			let stream = self
				.send
				.get_mut(&id)
				.and_then(|s| s.as_mut())
				.expect("scheduled stream is open");
			let start = buf.len();

			// Now that we know the `StreamId`, we can better account for how many bytes
			// are required to encode it.
			let max_buf_size = max_buf_size - buf.len() - 1 - VarInt::size(id.into());
			let (offsets, encode_length) = stream.pending.poll_transmit(max_buf_size);
			let fin = offsets.end == stream.pending.offset() && matches!(stream.state, SendState::DataSent { .. });
			if fin {
				stream.fin_pending = false;
			}

			let meta = frame::StreamMeta { id, offsets, fin };
			trace!(id = %meta.id, off = meta.offsets.start, len = meta.offsets.end - meta.offsets.start, fin = meta.fin, "STREAM");
			meta.encode(encode_length, buf);

			for data in stream.pending.get(meta.offsets.clone()) {
				buf.put_slice(data);
			}

			// Requeue a stream with data left, possibly at an updated priority. With fairness,
			// it goes behind its equal-priority peers so they get a turn first. The frame's
			// bytes count against its group's share either way.
			let requeue = stream.is_pending().then_some(stream.priority);
			self.pending
				.sent(id, &mut stream.slot, buf.len() - start, requeue, fair);
			written.streams.push(meta);
		}

		self.pending.restore();
		written
	}

	/// Whether STREAM `frame`, sent earlier, still has data a duplicate may carry
	pub(crate) fn can_duplicate(&self, frame: &frame::StreamMeta) -> bool {
		self.duplicable(frame.id)
			.is_some_and(|stream| stream.pending.unacked(frame.offsets.clone()))
	}

	/// Write STREAM frames resending what is still unacknowledged of `frames`, sent in an
	/// earlier packet, until the packet in `buf` is `max_buf_size` long, appending them to
	/// `written`
	///
	/// The duplicates are not queued for retransmission, and a reset stream's data expired, so
	/// it is never sent again.
	pub(crate) fn write_duplicates(
		&self,
		buf: &mut Vec<u8>,
		max_buf_size: usize,
		frames: &[frame::StreamMeta],
		written: &mut StreamMetaVec,
	) {
		for frame in frames {
			if buf.len() + frame::Stream::SIZE_BOUND >= max_buf_size {
				return;
			}
			let Some(stream) = self.duplicable(frame.id) else {
				continue;
			};
			let max_len = max_buf_size - buf.len() - 1 - VarInt::size(frame.id.into());
			let Some((offsets, encode_length)) = stream.pending.duplicate(frame.offsets.clone(), max_len) else {
				continue;
			};
			let meta = frame::StreamMeta {
				id: frame.id,
				fin: frame.fin && offsets.end == frame.offsets.end,
				offsets,
			};
			trace!(id = %meta.id, off = meta.offsets.start, len = meta.offsets.end - meta.offsets.start, fin = meta.fin, "STREAM (bandwidth probe)");
			meta.encode(encode_length, buf);
			for data in stream.pending.get(meta.offsets.clone()) {
				buf.put_slice(data);
			}
			written.push(meta);
		}
	}

	/// The send stream `id` if a duplicate of its data may still be sent: open, neither reset
	/// nor stopped by the peer
	fn duplicable(&self, id: StreamId) -> Option<&Send> {
		self.send
			.get(&id)?
			.as_deref()
			.filter(|stream| !stream.is_reset() && stream.stop_reason.is_none())
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
		let was_pending = stream.is_pending();
		let finished = stream.ack(frame);
		self.buffered_data -= buffered - stream.pending.buffered();
		if was_pending && !stream.is_pending() {
			// A duplicate delivered the last of the data queued for retransmission
			self.pending.remove(&mut stream.slot);
		}
		if let Some(offset) = stream.take_acked() {
			self.events.push_back(StreamEvent::Acked { id, offset });
		}
		if !finished {
			// The stream is unfinished or may still need retransmits
			return stream.take_held_reset();
		}

		// A reset is not reported as finished, like one without a Reliable Size
		let reset = stream.is_reset();
		if let (_, Some(mut stream)) = entry.remove_entry() {
			self.pending.leave(&mut stream.slot);
		}
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
			self.pending.push(frame.id, &mut stream.slot, stream.priority);
		}
	}

	pub(crate) fn received_max_streams(&mut self, dir: Dir, count: u64) -> Result<(), TransportError> {
		if count > MAX_STREAM_COUNT {
			return Err(TransportError::FRAME_ENCODING_ERROR("unrepresentable stream limit"));
		}

		let current = &mut self.max[dir as usize];
		if count > *current {
			*current = count;
			self.streams_blocked[dir as usize] = false;
			self.open_refused[dir as usize] = false;
			self.events.push_back(StreamEvent::Available { dir });
		}

		Ok(())
	}

	/// Handle increase to connection-level flow control limit
	pub(crate) fn received_max_data(&mut self, n: VarInt) {
		self.max_data = self.max_data.max(n.into());
	}

	pub(crate) fn received_max_stream_data(&mut self, id: StreamId, offset: u64) -> Result<(), TransportError> {
		self.validate_send_id(id).inspect_err(|_| {
			debug!("received illegal MAX_STREAM_DATA frame");
		})?;

		self.insert_remote(id);

		let max_send_data = self.max_send_data(id);
		if let Some(ss_opt) = self.send.get_mut(&id) {
			let ss = ss_opt.get_or_insert_with(|| Send::new(max_send_data));
			if ss.increase_max_data(offset) {
				self.send_unblocked(id);
			}
		}

		Ok(())
	}

	/// Wake the writer of a stream whose flow control limit just rose past what it has written
	fn send_unblocked(&mut self, id: StreamId) {
		let stream = self.send.get(&id).and_then(|s| s.as_ref()).unwrap();
		if !stream.is_writable() || stream.stop_reason.is_some() {
			return;
		}
		if self.write_limit() > 0 {
			self.events.push_back(StreamEvent::Writable { id });
			return;
		}
		// Still blocked on the connection window, so wait in the list that wakes when it relaxes
		self.connection_blocked.insert(id);
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
		if let Some(dir) = Dir::iter().find(|&i| mem::replace(&mut self.opened[i as usize], false)) {
			return Some(StreamEvent::Opened { dir });
		}

		if self.write_limit() > 0 {
			while let Some(id) = self.connection_blocked.pop() {
				let Some(stream) = self.send.get_mut(&id).and_then(|s| s.as_mut()) else {
					continue;
				};

				// If it's no longer sensible to write to a stream (even to detect an error) then don't
				// report it.
				if stream.is_writable() && stream.stop_reason.is_none() && stream.max_data > stream.offset() {
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
					return Err(TransportError::STREAM_STATE_ERROR("operation on unopened stream"));
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

	/// Check for errors entailed by the peer's use of `id` as a receive stream
	fn validate_send_id(&self, id: StreamId) -> Result<(), TransportError> {
		if self.side == id.initiator() {
			if self.is_local_unopened(id) {
				return Err(TransportError::STREAM_STATE_ERROR("operation on unopened stream"));
			}
		} else {
			// A raised `max_remote` may not be in a `MAX_STREAMS` frame yet
			let limit = self.sent_max_remote[id.dir() as usize];
			if id.index() >= limit {
				return Err(TransportError::STREAM_LIMIT_ERROR(""));
			}
			if id.dir() == Dir::Uni {
				return Err(TransportError::STREAM_STATE_ERROR("operation on recv-only stream"));
			}
		}
		Ok(())
	}

	/// Whether a locally initiated stream has never been open
	pub(crate) fn is_local_unopened(&self, id: StreamId) -> bool {
		id.index() >= self.next[id.dir() as usize]
	}

	pub(crate) fn set_max_concurrent(&mut self, dir: Dir, count: VarInt) {
		self.max_concurrent_remote_count[dir as usize] = count.into();
		self.ensure_remote_streams(dir);
	}

	#[cfg(test)]
	pub(crate) fn max_concurrent(&self, dir: Dir) -> u64 {
		self.allocated_remote_count[dir as usize]
	}

	pub(crate) fn set_send_window(&mut self, send_window: u64) {
		self.send_window = send_window;
	}

	/// Set the receive window of each stream, which applies as each stream's credit is next
	/// extended: credit already granted cannot be taken back.
	pub(crate) fn set_stream_receive_window(&mut self, window: u64) {
		self.stream_receive_window = window;
	}

	/// Set the receive_window and returns whether the receive_window has been
	/// expanded or shrunk: true if expanded, false if shrunk.
	pub(crate) fn set_receive_window(&mut self, receive_window: VarInt) -> bool {
		let receive_window = receive_window.into();
		let mut expanded = false;
		if receive_window > self.receive_window {
			// Growth first cancels what an earlier shrink still owes, so it is credited once.
			let growth: u64 = receive_window - self.receive_window;
			let cancelled = growth.min(self.receive_window_shrink_debt);
			self.receive_window_shrink_debt -= cancelled;
			self.local_max_data = self.local_max_data.saturating_add(growth - cancelled);
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
			leased: self.held,
			available: self.local_max_data.saturating_sub(self.data_recvd),
			evicted: self.evicted,
			peak_spent: self.peak_spent,
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
			self.connection_blocked.swap_remove(&id);
			self.send_streams -= 1;
		}
	}

	/// Recycle a receive stream's state, returning any credit its lease still holds, as when a
	/// plain read ends a stream a lease read from.
	///
	/// Returns whether a `MAX_DATA` frame should be enqueued for that credit.
	pub(super) fn stream_recv_freed(&mut self, id: StreamId, mut recv: StreamRecv) -> ShouldTransmit {
		let transmit = self.discard_lease(id, &mut recv.inner_mut().lease);
		self.unlooked.remove(&id);
		self.free_recv.push(recv.free(self.initial_stream_receive_window));
		self.stream_freed(id, StreamHalf::Recv);
		transmit
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
#[expect(
	clippy::disallowed_methods,
	clippy::disallowed_types,
	reason = "tests read the host clock directly"
)]
mod tests {
	use super::*;
	use crate::{
		LeaseError, ReadableError, RecvStream, SendStream, TransportErrorCode, WriteError,
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
	fn connection_blocked_reset_churn_is_bounded() {
		let mut server = make(Side::Server);
		server.set_params(&TransportParameters {
			initial_max_streams_uni: 512u32.into(),
			initial_max_stream_data_uni: 1024u32.into(),
			initial_max_data: 0u32.into(),
			..TransportParameters::default()
		});
		let (mut pending, state) = (Retransmits::default(), ConnState::Established);
		for _ in 0..512 {
			let id = Streams {
				state: &mut server,
				conn_state: &state,
			}
			.open(Dir::Uni, Default::default())
			.unwrap();
			let mut stream = SendStream {
				id,
				state: &mut server,
				pending: &mut pending,
				conn_state: &state,
			};
			assert_eq!(stream.write(b"blocked"), Err(WriteError::Blocked));
			assert_eq!(stream.write(b"blocked"), Err(WriteError::Blocked));
			assert_eq!(stream.state.connection_blocked.len(), 1);
			stream.reset(0u32.into()).unwrap();
			assert!(
				stream.state.connection_blocked.is_empty(),
				"reset writer no longer waits for credit"
			);
			assert_eq!(stream.write(b"blocked"), Err(WriteError::ClosedStream));
			server.reset_acked(id, 0);
			assert!(server.send.is_empty());
			assert!(server.connection_blocked.is_empty());
		}
		assert_eq!(server.write_limit(), 0);
	}

	#[test]
	fn connection_blocked_finished_stream_is_freed_without_credit() {
		let mut server = make(Side::Server);
		server.set_params(&TransportParameters {
			initial_max_streams_uni: 1u32.into(),
			initial_max_stream_data_uni: 1024u32.into(),
			..TransportParameters::default()
		});
		let (mut pending, state) = (Retransmits::default(), ConnState::Established);
		let id = Streams {
			state: &mut server,
			conn_state: &state,
		}
		.open(Dir::Uni, Default::default())
		.unwrap();
		let mut stream = SendStream {
			id,
			state: &mut server,
			pending: &mut pending,
			conn_state: &state,
		};
		assert_eq!(stream.write(b"blocked"), Err(WriteError::Blocked));
		stream.finish().unwrap();
		assert_eq!(stream.write(b"closed"), Err(WriteError::ClosedStream));
		server.received_ack_of(frame::StreamMeta {
			id,
			offsets: 0..0,
			fin: true,
		});
		assert!(server.send.is_empty());
		assert!(server.connection_blocked.is_empty());
		assert_eq!(server.write_limit(), 0);
	}

	#[test]
	fn connection_blocked_writer_observes_stop_with_a_held_window() {
		for peer_credit in [false, true] {
			let mut server = make(Side::Server);
			server.send_window = 1;
			server.set_params(&TransportParameters {
				initial_max_streams_uni: 2u32.into(),
				initial_max_stream_data_uni: 1024u32.into(),
				initial_max_data: if peer_credit { 1u32.into() } else { 1024u32.into() },
				..TransportParameters::default()
			});
			let (mut pending, state) = (Retransmits::default(), ConnState::Established);
			let holder = Streams {
				state: &mut server,
				conn_state: &state,
			}
			.open(Dir::Uni, Default::default())
			.unwrap();
			assert_eq!(
				SendStream {
					id: holder,
					state: &mut server,
					pending: &mut pending,
					conn_state: &state
				}
				.write(b"x"),
				Ok(1)
			);
			let id = Streams {
				state: &mut server,
				conn_state: &state,
			}
			.open(Dir::Uni, Default::default())
			.unwrap();
			let mut stream = SendStream {
				id,
				state: &mut server,
				pending: &mut pending,
				conn_state: &state,
			};
			assert_eq!(stream.write(b"blocked"), Err(WriteError::Blocked));
			let code = 7u32.into();
			stream.state.received_stop_sending(id, code).unwrap();
			assert_eq!(stream.state.poll(), Some(StreamEvent::Stopped { id, error_code: code }));
			assert_eq!(stream.write(b"stopped"), Err(WriteError::Stopped(code)));
			assert_eq!(stream.write(&[]), Err(WriteError::Stopped(code)));
			let mut chunks = [Bytes::from_static(b"stopped")];
			assert_eq!(stream.write_chunks(&mut chunks), Err(WriteError::Stopped(code)));
			assert_eq!(chunks[0], b"stopped"[..]);
			assert!(stream.state.connection_blocked.is_empty());
			assert_eq!(stream.state.write_limit(), 0);
			assert_eq!(stream.state.send[&holder].as_ref().unwrap().pending.buffered(), 1);
		}
	}

	#[test]
	fn connection_blocked_credit_wakes_only_live_writers_once() {
		let mut server = make(Side::Server);
		server.set_params(&TransportParameters {
			initial_max_streams_uni: 4u32.into(),
			initial_max_stream_data_uni: 1024u32.into(),
			..TransportParameters::default()
		});
		let (mut pending, state) = (Retransmits::default(), ConnState::Established);
		let ids: Vec<_> = (0..4)
			.map(|_| {
				let id = Streams {
					state: &mut server,
					conn_state: &state,
				}
				.open(Dir::Uni, Default::default())
				.unwrap();
				let mut stream = SendStream {
					id,
					state: &mut server,
					pending: &mut pending,
					conn_state: &state,
				};
				assert_eq!(stream.write(b"x"), Err(WriteError::Blocked));
				assert_eq!(stream.write(b"x"), Err(WriteError::Blocked));
				id
			})
			.collect();
		server.received_stop_sending(ids[0], 9u32.into()).unwrap();
		SendStream {
			id: ids[1],
			state: &mut server,
			pending: &mut pending,
			conn_state: &state,
		}
		.reset(0u32.into())
		.unwrap();
		SendStream {
			id: ids[2],
			state: &mut server,
			pending: &mut pending,
			conn_state: &state,
		}
		.finish()
		.unwrap();
		// Late stream credit must not register a terminal writer again.
		for &id in &ids[..3] {
			server.received_max_stream_data(id, 2048).unwrap();
		}
		server.events.clear();
		assert_eq!(server.connection_blocked.len(), 1);
		server.received_max_data(1024u32.into());
		assert_eq!(server.poll(), Some(StreamEvent::Writable { id: ids[3] }));
		assert!(server.connection_blocked.is_empty());
		assert_eq!(server.poll(), None);
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
		.open(Dir::Uni, Default::default())
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
		assert_eq!(server.write_limit(), 0, "acknowledged suffix still occupies storage");
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

	/// A bandwidth probe's duplicate can deliver a lost range before its retransmission is sent,
	/// which then has nothing left to send
	#[test]
	fn ack_cancels_queued_retransmit() {
		let mut server = make(Side::Server);
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
		.open(Dir::Uni, Default::default())
		.unwrap();
		SendStream {
			id,
			state: &mut server,
			pending: &mut pending,
			conn_state: &state,
		}
		.write(&[42; 24])
		.unwrap();
		let mut buf = Vec::with_capacity(1000);
		let sent = server.write_frames(&mut buf, 0..1000, true).streams;
		assert_eq!(sent.len(), 1);

		server.retransmit(sent[0].clone());
		assert!(server.can_send_stream_data());
		server.received_ack_of(sent[0].clone());
		assert!(!server.can_send_stream_data());
		buf.clear();
		assert!(server.write_frames(&mut buf, 0..1000, true).streams.is_empty());
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
		.open(Dir::Uni, Default::default())
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

		let mut chunks = recv.read().unwrap();
		assert_eq!(chunks.next(MESSAGE_SIZE).unwrap().unwrap().bytes.len(), MESSAGE_SIZE);
		assert!(chunks.next(0).unwrap().is_none());
		let should_transmit = chunks.finalize();
		assert!(should_transmit.0);
		assert!(pending.max_stream_id[Dir::Uni as usize]);
		assert_eq!(client.local_max_data - initial_max, MESSAGE_SIZE as u64);
	}

	/// Stopping a stream whose final size is known frees it, so its stream ID credit goes out at
	/// once instead of waiting for the next packet to arrive.
	#[test]
	fn stop_after_reset_issues_stream_credit() {
		let mut client = StreamsState::new(
			Side::Client,
			1u32.into(),
			1u32.into(),
			1024 * 1024,
			(1024 * 1024u32).into(),
			(1024 * 1024u32).into(),
		);
		let id = StreamId::new(Side::Server, Dir::Uni, 0);
		assert_eq!(
			client
				.received_reset(frame::ResetStream {
					id,
					error_code: 0u32.into(),
					final_offset: 0u32.into(),
					reliable_size: None,
				})
				.unwrap(),
			ShouldTransmit(false)
		);

		let mut pending = Retransmits::default();
		RecvStream {
			id,
			state: &mut client,
			pending: &mut pending,
		}
		.stop(0u32.into())
		.unwrap();
		assert!(pending.max_stream_id[Dir::Uni as usize]);
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

		let mut chunks = recv.read().unwrap();
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
		let mut chunks = recv.read().unwrap();
		assert_eq!(chunks.next(1024).unwrap_err(), crate::ReadError::Reset(0u32.into()));
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
		assert_eq!(recv.read().err(), Some(ReadableError::ClosedStream));

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
			assert_eq!(client.local_max_data, initial_max_data + u64::from(final_offset));
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
		.open(Dir::Uni, Default::default())
		.unwrap();

		let mut stream = SendStream {
			id,
			state: &mut server,
			pending: &mut pending,
			conn_state: &state,
		};

		let error_code = 0u32.into();
		stream.state.received_stop_sending(id, error_code).unwrap();
		assert!(stream.state.events.contains(&StreamEvent::Stopped { id, error_code }));
		stream.state.events.clear();

		assert_eq!(stream.write(&[]), Err(WriteError::Stopped(error_code)));

		stream.reset(0u32.into()).unwrap();
		assert_eq!(stream.write(&[]), Err(WriteError::ClosedStream));

		// A duplicate frame is a no-op
		stream.state.received_stop_sending(id, error_code).unwrap();
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

		let id_high = streams.open(Dir::Bi, Default::default()).unwrap();
		let id_mid = streams.open(Dir::Bi, Default::default()).unwrap();
		let id_low = streams.open(Dir::Bi, Default::default()).unwrap();

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
		let meta = server.write_frames(&mut buf, 0..40, true).streams;
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
			let id_low = streams.open(Dir::Bi, Default::default()).unwrap();
			let id_high = streams.open(Dir::Bi, Default::default()).unwrap();

			let mut low = SendStream {
				id: id_low,
				state: &mut server,
				pending: &mut pending,
				conn_state: &state,
			};
			low.set_priority(-1).unwrap();
			assert_eq!(low.write(b"low").unwrap(), 3);

			let mut buf = Vec::with_capacity(40);
			let mut sent = server.write_frames(&mut buf, 0..40, fair).streams;
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
			let sent = server.write_frames(&mut buf, 0..40, fair).streams;
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

		let id_high = streams.open(Dir::Bi, Default::default()).unwrap();
		let id_mid = streams.open(Dir::Bi, Default::default()).unwrap();

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

		// Lowering the queued high priority stream moves it behind mid at once.
		let mut high = SendStream {
			id: id_high,
			state: &mut server,
			pending: &mut pending,
			conn_state: &state,
		};
		high.set_priority(-1).unwrap();
		assert_eq!(server.pending.len(), 2);

		let mut buf = Vec::with_capacity(1000);
		let meta = server.write_frames(&mut buf, 0..1000, true).streams;
		assert_eq!(meta.len(), 2);
		assert_eq!(meta[0].id, id_mid);
		assert_eq!(meta[1].id, id_high);

		assert!(!server.can_send_stream_data());
		assert_eq!(server.pending.len(), 0);
	}

	/// A stream starved behind higher priorities that keep requeueing sends next once raised
	/// above them, without waiting to be popped at its old priority.
	#[test]
	fn raising_a_starved_stream_sends_it_next() {
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
		let ids: Vec<_> = (0..3)
			.map(|_| streams.open(Dir::Bi, Default::default()).unwrap())
			.collect();
		let (busy, starved) = (&ids[..2], ids[2]);

		for (&id, priority) in ids.iter().zip([5, 5, 0]) {
			let mut stream = SendStream {
				id,
				state: &mut server,
				pending: &mut pending,
				conn_state: &state,
			};
			stream.set_priority(priority).unwrap();
			stream.write(&[0; 200]).unwrap();
		}

		let mut buf = Vec::with_capacity(1000);
		for _ in 0..4 {
			let start = buf.len();
			let meta = server.write_frames(&mut buf, start..start + 40, true).streams;
			assert!(busy.contains(&meta[0].id), "{meta:?}");
		}

		SendStream {
			id: starved,
			state: &mut server,
			pending: &mut pending,
			conn_state: &state,
		}
		.set_priority(10)
		.unwrap();
		let start = buf.len();
		let meta = server.write_frames(&mut buf, start..start + 40, true).streams;
		assert_eq!(meta[0].id, starved);
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
			let id_a = streams.open(Dir::Bi, Default::default()).unwrap();
			let id_b = streams.open(Dir::Bi, Default::default()).unwrap();
			let id_c = streams.open(Dir::Bi, Default::default()).unwrap();

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
				let meta = server.write_frames(&mut buf, buf_len..buf_len + 40, fair).streams;
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
				assert_eq!(stream_ids, vec![id_a, id_b, id_c, id_a, id_b, id_c, id_a, id_b, id_c]);
			} else {
				// When fairness is disabled the stream is re-queued before all the other streams
				// with the same priority.
				assert_eq!(stream_ids, vec![id_a, id_a, id_a, id_b, id_b, id_b, id_c, id_c, id_c]);
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
		let id_a = streams.open(Dir::Bi, Default::default()).unwrap();
		let id_b = streams.open(Dir::Bi, Default::default()).unwrap();
		let id_c = streams.open(Dir::Bi, Default::default()).unwrap();

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
		let meta = server.write_frames(&mut buf, buf_len..buf_len + 40, false).streams;
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
			let meta = server.write_frames(&mut buf, buf_len..buf_len + 40, false).streams;
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

		let id = streams.open(Dir::Uni, Default::default()).unwrap();
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
		let mut chunks = recv.read().unwrap();
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
		let mut chunks = recv.read().unwrap();
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
			let mut chunks = recv.read().unwrap();
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
			assert_eq!(server.receive_window_shrink_debt, expected_receive_window_shrink_debt);
			assert_eq!(server.local_max_data, prev_local_max_data);
			assert!(!should_transmit.should_transmit());
		}

		// credit again which exceeds all remaining expected_receive_window_shrink_debt
		let credits = 1024 * 512;
		let prev_local_max_data = server.local_max_data;
		let expected_local_max_data = server.local_max_data + (credits - server.receive_window_shrink_debt);
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
		.open(Dir::Uni, Default::default())
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
		assert_eq!(stream.state.poll(), Some(StreamEvent::Writable { id: stream_id }));

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

		assert_eq!(stream.state.poll(), Some(StreamEvent::Writable { id: stream_id }));

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
		.open(Dir::Uni, Default::default())
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
		assert_eq!(stream.state.poll(), Some(StreamEvent::Writable { id: stream_id }));
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

	fn reset_at(id: StreamId, error_code: u32, final_offset: u32, reliable_size: Option<u32>) -> frame::ResetStream {
		frame::ResetStream {
			id,
			error_code: error_code.into(),
			final_offset: final_offset.into(),
			reliable_size: reliable_size.map(VarInt::from_u32),
		}
	}

	fn receive(state: &mut StreamsState, id: StreamId, data: Range<u64>) {
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
	fn drain(state: &mut StreamsState, id: StreamId) -> (u64, crate::ReadError) {
		let mut pending = Retransmits::default();
		let mut recv = RecvStream {
			id,
			state,
			pending: &mut pending,
		};
		let mut chunks = recv.read().unwrap();
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
		let _ = client.received_reset(reset_at(id, 7, 100, Some(10))).unwrap();
		assert_eq!(client.data_recvd, 100);
		assert_eq!(client.local_max_data - initial_max, 90);

		// Data past the Reliable Size is dropped without counting twice
		receive(&mut client, id, 4..20);
		assert_eq!(client.data_recvd, 100);
		assert_eq!(drain(&mut client, id), (10, crate::ReadError::Reset(7u32.into())));
		assert_eq!(client.local_max_data - initial_max, 100);
	}

	#[test]
	fn reset_at_withholds_error_until_prefix_arrives() {
		let mut client = make(Side::Client);
		let id = StreamId::new(Side::Server, Dir::Uni, 0);
		let initial_max = client.local_max_data;
		let _ = client.received_reset(reset_at(id, 7, 20, Some(10))).unwrap();
		receive(&mut client, id, 4..20);
		assert_eq!(drain(&mut client, id), (0, crate::ReadError::Blocked));
		receive(&mut client, id, 0..4);
		assert_eq!(drain(&mut client, id), (10, crate::ReadError::Reset(7u32.into())));
		assert_eq!(client.local_max_data - initial_max, 20);
	}

	#[test]
	fn reset_at_shrinks_reliable_size() {
		let mut client = make(Side::Client);
		let id = StreamId::new(Side::Server, Dir::Uni, 0);
		let initial_max = client.local_max_data;
		let _ = client.received_reset(reset_at(id, 7, 20, Some(10))).unwrap();
		let _ = client.received_reset(reset_at(id, 7, 20, Some(4))).unwrap();
		assert_eq!(client.local_max_data - initial_max, 16);
		// A reordered, larger Reliable Size is ignored
		assert_eq!(
			client.received_reset(reset_at(id, 7, 20, Some(8))),
			Ok(ShouldTransmit(false))
		);
		assert_eq!(client.local_max_data - initial_max, 16);
		receive(&mut client, id, 0..20);
		assert_eq!(drain(&mut client, id), (4, crate::ReadError::Reset(7u32.into())));
		assert_eq!(client.local_max_data - initial_max, 20);
	}

	/// A RESET_STREAM after a RESET_STREAM_AT abandons the prefix too.
	#[test]
	fn reset_after_reset_at() {
		let mut client = make(Side::Client);
		let id = StreamId::new(Side::Server, Dir::Uni, 0);
		let initial_max = client.local_max_data;
		receive(&mut client, id, 0..2);
		let _ = client.received_reset(reset_at(id, 7, 20, Some(10))).unwrap();
		let _ = client.received_reset(reset_at(id, 7, 20, None)).unwrap();
		assert_eq!(drain(&mut client, id), (0, crate::ReadError::Reset(7u32.into())));
		assert_eq!(client.local_max_data - initial_max, 20);
	}

	#[test]
	fn reset_at_error_code_change() {
		let mut client = make(Side::Client);
		let id = StreamId::new(Side::Server, Dir::Uni, 0);
		let _ = client.received_reset(reset_at(id, 7, 20, Some(10))).unwrap();
		assert_eq!(
			client.received_reset(reset_at(id, 8, 20, Some(4))).map_err(|e| e.code),
			Err(TransportErrorCode::STREAM_STATE_ERROR)
		);
	}

	#[test]
	fn reset_at_final_size_change() {
		let mut client = make(Side::Client);
		let id = StreamId::new(Side::Server, Dir::Uni, 0);
		let _ = client.received_reset(reset_at(id, 7, 20, Some(10))).unwrap();
		assert_eq!(
			client.received_reset(reset_at(id, 7, 21, Some(4))).map_err(|e| e.code),
			Err(TransportErrorCode::FINAL_SIZE_ERROR)
		);
		let mut client = make(Side::Client);
		let _ = client.received_reset(reset_at(id, 7, 20, Some(10))).unwrap();
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
			let _ = client.received_reset(reset_at(id, 7, 20, reliable_size)).unwrap();
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
		.open(Dir::Uni, Default::default())
		.unwrap();
		let mut stream = SendStream {
			id,
			state: &mut server,
			pending: &mut pending,
			conn_state: &state,
		};
		stream.write(b"hello world").unwrap();
		let mut buf = Vec::new();
		let sent = stream.state.write_frames(&mut buf, 0..100, true).streams;
		stream.reset_at(4, 7u32.into()).unwrap();
		assert!(pending.reset_stream.is_empty());
		assert_eq!(server.buffered_data, 4);

		// The prefix is lost and resent, without what followed it
		server.retransmit(sent[0].clone());
		buf.clear();
		let resent = server.write_frames(&mut buf, 0..100, true).streams;
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
		.open(Dir::Uni, Default::default())
		.unwrap();
		let mut stream = SendStream {
			id,
			state: &mut server,
			pending: &mut pending,
			conn_state: &state,
		};
		stream.write(b"hello world").unwrap();
		stream.reset_at(4, 7u32.into()).unwrap();
		assert_eq!(pending.reset_stream, &[(id, 4)]);
		assert_eq!(server.received_stop_sending(id, 9u32.into()), Ok(Some(0)));

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
		.open(Dir::Uni, Default::default())
		.unwrap();
		let mut stream = SendStream {
			id,
			state: &mut server,
			pending: &mut pending,
			conn_state: &state,
		};
		stream.write(b"hello world").unwrap();
		let mut buf = Vec::new();
		let sent = stream.state.write_frames(&mut buf, 0..100, true).streams;
		stream.reset_at(4, 7u32.into()).unwrap();

		server.reset_acked(id, 4);
		assert_eq!(server.received_stop_sending(id, 9u32.into()), Ok(Some(0)));
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
		let err = client.received_reset(reset_at(id, 7, 9, Some(10))).unwrap_err();
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
			.open(Dir::Uni, Default::default())
			.unwrap();
			let mut stream = SendStream {
				id,
				state: &mut server,
				pending: &mut pending,
				conn_state: &state,
			};
			stream.write(b"hello world").unwrap();
			stream.reset_at(4, 7u32.into()).unwrap();
			assert_eq!(pending.reset_stream.is_empty(), !reset_stream_at);

			assert_eq!(server.received_stop_sending(id, 9u32.into()), Ok(Some(0)));
			assert_eq!(server.buffered_data, 0);
			assert!(!server.can_send_stream_data());
			let send = server.send[&id].as_ref().unwrap();
			assert!(send.pending.is_fully_acked());
			let frame = send.reset_frame(id, reset_stream_at).unwrap();
			assert_eq!(frame.reliable_size, None);
			assert_eq!(frame.final_offset, VarInt(11));
			// Abandoned once, so a repeated STOP_SENDING sends nothing more
			assert_eq!(server.received_stop_sending(id, 9u32.into()), Ok(None));
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
		.open(Dir::Uni, Default::default())
		.unwrap();
		let mut stream = SendStream {
			id,
			state: &mut server,
			pending: &mut pending,
			conn_state: &state,
		};
		stream.write(b"hello world").unwrap();
		let mut buf = Vec::new();
		let sent = stream.state.write_frames(&mut buf, 0..100, true).streams;
		assert_eq!(sent[0].offsets, 0..11);
		stream.reset_at(4, 7u32.into()).unwrap();
		assert_eq!(server.buffered_data, 4);

		// Only the lost prefix is retransmitted, without the FIN of a stream that was never finished
		server.retransmit(sent[0].clone());
		buf.clear();
		let resent = server.write_frames(&mut buf, 0..100, true).streams;
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
		.open(Dir::Uni, Default::default())
		.unwrap();
		let mut stream = SendStream {
			id,
			state: &mut server,
			pending: &mut pending,
			conn_state: &state,
		};
		stream.write(b"hello world").unwrap();
		stream.state.received_stop_sending(id, 9u32.into()).unwrap();
		stream.reset_at(4, 7u32.into()).unwrap();

		let send = server.send[&id].as_ref().unwrap();
		let frame = send.reset_frame(id, true).unwrap();
		assert_eq!(frame.reliable_size, None);
		assert_eq!(frame.final_offset, VarInt(11));
		assert!(send.pending.is_fully_acked());
		assert_eq!(server.buffered_data, 0);
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

	fn recv<'a>(client: &'a mut StreamsState, pending: &'a mut Retransmits, id: StreamId) -> RecvStream<'a> {
		RecvStream {
			id,
			state: client,
			pending,
		}
	}

	/// Lease whatever has arrived on `id`, returning how many bytes are leased.
	fn lease(client: &mut StreamsState, id: StreamId) -> usize {
		let mut pending = Retransmits::default();
		let _ = recv(client, &mut pending, id).lease(0).unwrap();
		client.check_leases();
		client.leased(id).unwrap().len()
	}

	/// Wait on `id` for more than it has leased, as a reader decoding a message would.
	fn wait(client: &mut StreamsState, id: StreamId, want: usize) -> Result<(), LeaseError> {
		let mut pending = Retransmits::default();
		let result = recv(client, &mut pending, id).lease(want).map(|_| ());
		client.check_leases();
		result
	}

	fn release(client: &mut StreamsState, id: StreamId, n: usize) {
		let mut pending = Retransmits::default();
		let mut recv = recv(client, &mut pending, id);
		recv.advance(n);
		let _ = recv.release();
		client.check_leases();
	}

	fn stop(client: &mut StreamsState, id: StreamId, code: u32) -> Retransmits {
		let mut pending = Retransmits::default();
		let _ = recv(client, &mut pending, id).stop(code.into());
		client.check_leases();
		pending
	}

	/// Leased bytes keep their connection credit until released, even across a gap, and so do
	/// bytes taken or advanced past.
	#[test]
	fn lease_holds_credit_until_released() {
		let mut client = make(Side::Client);
		let initial = client.local_max_data;
		let id = StreamId::new(Side::Server, Dir::Uni, 0);
		data(&mut client, id, 0, 32, false);
		data(&mut client, id, 64, 16, false);

		assert_eq!(lease(&mut client, id), 32);
		assert_eq!(client.credit_stats().leased, 32);
		assert_eq!(client.local_max_data, initial, "leased, not returned");
		assert_eq!(lease(&mut client, id), 32, "a second lease sees the same bytes");

		let mut pending = Retransmits::default();
		let taken = recv(&mut client, &mut pending, id).take(10);
		assert_eq!(taken.len(), 10);
		assert_eq!(client.leased(id).unwrap().len(), 22);
		assert_eq!(client.local_max_data, initial, "taken, not released");

		release(&mut client, id, 2);
		assert_eq!(client.local_max_data, initial + 12);
		assert_eq!(client.credit_stats().leased, 20);
		assert_eq!(
			client.credit_stats().peak_spent,
			80,
			"the gap counts, and the peak outlives the release"
		);
	}

	/// A finished stream keeps its state, and its stream ID, until its reader stops it: a lease
	/// after the last release still sees the FIN, and the stop frees it without telling the peer.
	#[test]
	fn finished_lease_keeps_its_stream_until_stop() {
		let mut client = make(Side::Client);
		let initial = client.local_max_data;
		let id = StreamId::new(Side::Server, Dir::Uni, 0);
		data(&mut client, id, 0, 32, true);

		assert_eq!(lease(&mut client, id), 32);
		assert!(client.leased(id).unwrap().is_fin());
		assert_eq!(wait(&mut client, id, 64), Ok(()), "a FIN ends the wait short");

		release(&mut client, id, 32);
		assert_eq!(client.local_max_data, initial + 32);
		assert_eq!(client.credit_stats().leased, 0);
		assert_eq!(wait(&mut client, id, 1), Ok(()));
		assert!(client.leased(id).unwrap().is_fin(), "still finished");

		let pending = stop(&mut client, id, 0);
		assert!(pending.stop_sending.is_empty(), "the peer is done");
		assert!(!client.recv.contains_key(&id), "freed");
		assert!(client.leased(id).is_none());
	}

	/// Every byte returns its credit exactly once, whether a stop or a reset ends the stream,
	/// with leased bytes outstanding and a gap the transport accounts for itself.
	#[test]
	fn lease_credit_returns_once() {
		for reset_first in [false, true] {
			let mut client = make(Side::Client);
			let initial = client.local_max_data;
			let id = StreamId::new(Side::Server, Dir::Uni, 0);
			data(&mut client, id, 0, 32, false);
			data(&mut client, id, 64, 16, false);
			assert_eq!(lease(&mut client, id), 32);

			if reset_first {
				reset(&mut client, id, 128);
				assert_eq!(wait(&mut client, id, 33), Err(LeaseError::Reset(0u32.into())));
				assert_eq!(client.local_max_data, initial + 128 - 32, "all but the lease");
			}
			let _ = stop(&mut client, id, 0);
			if !reset_first {
				// Late data below and beyond the stop is credited on arrival.
				data(&mut client, id, 32, 32, false);
				reset(&mut client, id, 128);
			}
			assert!(!client.recv.contains_key(&id));
			assert!(client.leased(id).is_none());
			assert_eq!(client.local_max_data, initial + 128, "reset_first: {reset_first}");
			assert_eq!(client.credit_stats().leased, 0);
		}
	}

	#[test]
	fn lease_pays_shrink_debt() {
		let mut client = make(Side::Client);
		let initial = client.local_max_data;
		let id = StreamId::new(Side::Server, Dir::Uni, 0);
		data(&mut client, id, 0, 32, false);
		assert_eq!(lease(&mut client, id), 32);
		client.set_receive_window((client.receive_window - 16).try_into().unwrap());
		release(&mut client, id, 32);
		assert_eq!(client.local_max_data, initial + 16);
	}

	/// A want the connection window can never hold fails rather than waits forever.
	#[test]
	fn want_beyond_the_window_is_too_large() {
		let mut client = make(Side::Client);
		let id = StreamId::new(Side::Server, Dir::Uni, 0);
		data(&mut client, id, 0, 32, false);
		let window = client.receive_window as usize;
		assert!(matches!(wait(&mut client, id, window), Err(LeaseError::Blocked(_))));
		assert_eq!(wait(&mut client, id, window + 1), Err(LeaseError::TooLarge));

		// Taken bytes do not: a frame past the window waits, and gives way to eviction.
		let mut pending = Retransmits::default();
		let _ = recv(&mut client, &mut pending, id).take(10);
		assert!(matches!(wait(&mut client, id, window - 9), Err(LeaseError::Blocked(_))));
	}

	/// A connection with a 2 * `window` receive window, and its streams 0 and 1.
	fn stuck(window: u64) -> (StreamsState, [StreamId; 2]) {
		let client = StreamsState::new(
			Side::Client,
			128u32.into(),
			128u32.into(),
			1024 * 1024,
			VarInt::from_u64(2 * window).unwrap(),
			VarInt::from_u64(2 * window).unwrap(),
		);
		let ids = [0, 1].map(|index| StreamId::new(Side::Server, Dir::Uni, index));
		(client, ids)
	}

	fn readable(client: &mut StreamsState) -> Vec<StreamId> {
		client
			.events
			.drain(..)
			.filter_map(|event| match event {
				StreamEvent::Readable { id } => Some(id),
				_ => None,
			})
			.collect()
	}

	/// Credit an eviction frees pays a shrunk window's debt first, so eviction goes on until the
	/// peer has credit again; stopping at one victim would leave the other readers and the peer
	/// waiting on each other.
	#[test]
	fn stuck_window_evicts_past_shrink_debt() {
		const WINDOW: u64 = 1000;
		let mut client = StreamsState::new(
			Side::Client,
			128u32.into(),
			128u32.into(),
			1024 * 1024,
			VarInt::from_u64(3 * WINDOW).unwrap(),
			VarInt::from_u64(3 * WINDOW).unwrap(),
		);
		let ids = [0, 1, 2].map(|index| StreamId::new(Side::Server, Dir::Uni, index));
		for id in ids {
			data(&mut client, id, 0, WINDOW as usize, false);
			assert_eq!(lease(&mut client, id), WINDOW as usize);
		}
		assert_eq!(readable(&mut client), ids);
		let max_data = client.local_max_data;
		client.set_receive_window(VarInt::from_u64(3 * WINDOW / 2).unwrap());

		let want = WINDOW as usize + 1;
		assert!(matches!(wait(&mut client, ids[0], want), Err(LeaseError::Blocked(_))));
		assert_eq!(client.credit_stats().evicted, 2);
		assert_eq!(readable(&mut client), [ids[2], ids[1]], "newest first");
		assert_eq!(client.local_max_data, max_data + WINDOW / 2);
		assert!(!client.leased(ids[0]).unwrap().evicted);
	}

	/// However its reader consumes tiny frames, and however many arrive, the lease holds copies
	/// of their bytes, not the receive buffers they arrived in, so a byte of credit cannot pin a
	/// whole batch of datagrams, and the copies stay few chunks.
	#[test]
	fn lease_releases_datagram_buffers() {
		#[derive(Debug, Clone, Copy)]
		enum Reader {
			/// Waits for more than has arrived, as a reader decoding a message does.
			Wait,
			/// Leases what arrived and parks without consuming it.
			Park,
			/// Takes each byte as it arrives and keeps it, as a reader handing payload on does.
			Take,
		}
		for (reader, frames) in [Reader::Wait, Reader::Park, Reader::Take]
			.into_iter()
			.flat_map(|reader| [(reader, 8), (reader, 1024)])
		{
			let mut client = make(Side::Client);
			let id = StreamId::new(Side::Server, Dir::Uni, 0);
			let buffers: Vec<Bytes> = (0..frames).map(|_| Bytes::from(vec![0u8; 4096])).collect();
			let mut taken = Vec::new();
			for (offset, buffer) in buffers.iter().enumerate() {
				let _ = client
					.received(
						frame::Stream {
							id,
							offset: offset as u64,
							fin: false,
							data: buffer.slice(..1),
						},
						// One datagram of a receive batch: too little over-allocation for the
						// assembler to copy it out.
						1200,
					)
					.unwrap();
				let mut pending = Retransmits::default();
				let mut recv = recv(&mut client, &mut pending, id);
				match reader {
					Reader::Wait => assert!(matches!(recv.lease(buffers.len() + 1), Err(LeaseError::Blocked(_)))),
					Reader::Park => {
						let _ = recv.lease(1).unwrap();
					}
					Reader::Take => {
						let _ = recv.lease(1).unwrap();
						taken.push(recv.take(usize::MAX));
					}
				}
			}
			assert!(
				buffers.into_iter().all(|buffer| buffer.is_unique()),
				"{reader:?} pins a receive buffer of {frames}"
			);
			let leased = client.leased(id).unwrap();
			assert_eq!(leased.len() + taken.len(), frames);
			assert!(
				leased.chunks().count() <= frames.ilog2() as usize + 1,
				"{reader:?} keeps {} chunks of {frames}",
				leased.chunks().count()
			);
		}
	}

	/// A peer can spend the window across many streams before any reader looks, so every reader
	/// then waits on a stuck window. Each wait checks it in amortized constant time, so a hostile
	/// peer cannot make the stuck check quadratic.
	#[test]
	fn stuck_check_stays_linear() {
		const STREAMS: u64 = 512;
		let mut client = StreamsState::new(
			Side::Client,
			1024u32.into(),
			1024u32.into(),
			1024 * 1024,
			VarInt::from_u64(STREAMS).unwrap(),
			VarInt::from_u64(STREAMS).unwrap(),
		);
		let ids: Vec<_> = (0..STREAMS)
			.map(|index| StreamId::new(Side::Server, Dir::Uni, index))
			.collect();
		for &id in &ids {
			data(&mut client, id, 0, 1, false);
		}
		for &id in &ids {
			assert!(matches!(wait(&mut client, id, 2), Err(LeaseError::Blocked(_))));
		}
		assert_eq!(client.credit_stats().evicted, 1);
		assert!(
			client.scans <= 4 * STREAMS as usize,
			"{} streams looked at for {STREAMS} waits",
			client.scans
		);
	}

	/// A consume whose credit only pays a shrunk window's debt reruns the stuck pass: the
	/// readers that would have rerun it are already waiting, and the peer is still blocked.
	#[test]
	fn consume_into_shrink_debt_evicts() {
		const WINDOW: u64 = 1000;
		let mut client = StreamsState::new(
			Side::Client,
			128u32.into(),
			128u32.into(),
			1024 * 1024,
			VarInt::from_u64(3 * WINDOW).unwrap(),
			VarInt::from_u64(3 * WINDOW).unwrap(),
		);
		let ids = [0, 1, 2].map(|index| StreamId::new(Side::Server, Dir::Uni, index));
		for id in ids {
			data(&mut client, id, 0, WINDOW as usize, false);
		}
		assert_eq!(lease(&mut client, ids[1]), WINDOW as usize);
		assert_eq!(lease(&mut client, ids[2]), WINDOW as usize);
		let max_data = client.local_max_data;
		client.set_receive_window(VarInt::from_u64(2 * WINDOW).unwrap());

		// The first stream has data its reader has not looked at, so nothing is evicted yet.
		for id in [ids[1], ids[2]] {
			assert!(matches!(
				wait(&mut client, id, WINDOW as usize + 1),
				Err(LeaseError::Blocked(_))
			));
		}
		assert_eq!(client.credit_stats().evicted, 0);
		readable(&mut client);

		// Its reader finishes a unit and parks; the credit only pays the debt.
		assert_eq!(lease(&mut client, ids[0]), WINDOW as usize);
		release(&mut client, ids[0], WINDOW as usize);
		assert_eq!(client.credit_stats().evicted, 1);
		assert_eq!(readable(&mut client), [ids[2]]);
		assert_eq!(client.local_max_data, max_data + WINDOW);
	}

	/// A reader waiting behind a gap is not stuck: retransmitting the missing bytes spends no
	/// connection credit, so ordinary loss on a full window evicts nothing.
	#[test]
	fn gap_is_not_a_stuck_window() {
		const WINDOW: u64 = 1000;
		let (mut client, ids) = stuck(WINDOW);
		// A packet of the first stream is lost; the bytes after it spend the rest of its share.
		data(&mut client, ids[0], 0, 100, false);
		data(&mut client, ids[0], 200, (WINDOW - 200) as usize, false);
		assert_eq!(lease(&mut client, ids[0]), 100);
		assert!(matches!(
			wait(&mut client, ids[0], WINDOW as usize),
			Err(LeaseError::Blocked(_))
		));
		data(&mut client, ids[1], 0, WINDOW as usize, false);
		assert_eq!(lease(&mut client, ids[1]), WINDOW as usize);
		assert!(matches!(
			wait(&mut client, ids[1], WINDOW as usize + 1),
			Err(LeaseError::Blocked(_))
		));
		assert_eq!(client.credit_stats().evicted, 0);
		assert_eq!(client.local_max_data, client.data_recvd, "the peer is blocked");

		// The retransmission completes the first stream's unit, whose release frees its credit.
		data(&mut client, ids[0], 100, 100, false);
		assert_eq!(lease(&mut client, ids[0]), WINDOW as usize);
		release(&mut client, ids[0], WINDOW as usize);
		assert_eq!(client.credit_stats().evicted, 0);
		assert!(client.local_max_data > client.data_recvd, "the peer has credit");
	}

	/// Stopping a finished stream's lease returns credit that may only pay a shrunk window's debt,
	/// so the stop reruns the stuck pass as a release does.
	#[test]
	fn stop_finished_lease_into_shrink_debt_evicts() {
		const WINDOW: u64 = 1000;
		let mut client = StreamsState::new(
			Side::Client,
			128u32.into(),
			128u32.into(),
			1024 * 1024,
			VarInt::from_u64(3 * WINDOW).unwrap(),
			VarInt::from_u64(3 * WINDOW).unwrap(),
		);
		let ids = [0, 1, 2].map(|index| StreamId::new(Side::Server, Dir::Uni, index));
		data(&mut client, ids[0], 0, WINDOW as usize, true);
		for id in [ids[1], ids[2]] {
			data(&mut client, id, 0, WINDOW as usize, false);
			assert_eq!(lease(&mut client, id), WINDOW as usize);
		}
		let max_data = client.local_max_data;
		client.set_receive_window(VarInt::from_u64(2 * WINDOW).unwrap());
		for id in [ids[1], ids[2]] {
			assert!(matches!(
				wait(&mut client, id, WINDOW as usize + 1),
				Err(LeaseError::Blocked(_))
			));
		}
		readable(&mut client);

		// The first reader leases the whole finished stream, then stops it; the credit only pays
		// the debt.
		assert_eq!(lease(&mut client, ids[0]), WINDOW as usize);
		assert!(client.leased(ids[0]).unwrap().is_fin());
		assert_eq!(client.credit_stats().evicted, 0);
		let mut pending = Retransmits::default();
		assert!(recv(&mut client, &mut pending, ids[0]).stop(0u32.into()).is_ok());
		assert_eq!(client.credit_stats().evicted, 1);
		assert_eq!(readable(&mut client), [ids[2]]);
		assert_eq!(client.local_max_data, max_data + WINDOW);
	}

	/// Credit a stopped stream's arrival returns may only pay a shrunk window's debt, leaving the
	/// peer blocked behind readers that are already waiting, so the arrival reruns the stuck pass.
	#[test]
	fn stopped_arrival_into_shrink_debt_evicts() {
		const WINDOW: u64 = 1000;
		let mut client = StreamsState::new(
			Side::Client,
			128u32.into(),
			128u32.into(),
			1024 * 1024,
			VarInt::from_u64(3 * WINDOW).unwrap(),
			VarInt::from_u64(3 * WINDOW).unwrap(),
		);
		let ids = [0, 1, 2].map(|index| StreamId::new(Side::Server, Dir::Uni, index));
		data(&mut client, ids[0], 0, 1, false);
		let _ = stop(&mut client, ids[0], 0);
		for id in [ids[1], ids[2]] {
			data(&mut client, id, 0, WINDOW as usize, false);
			assert_eq!(lease(&mut client, id), WINDOW as usize);
			assert!(matches!(
				wait(&mut client, id, WINDOW as usize + 1),
				Err(LeaseError::Blocked(_))
			));
		}
		client.set_receive_window(VarInt::from_u64(2 * WINDOW).unwrap());
		readable(&mut client);

		// The peer spends the rest of its credit on the stopped stream; the debt absorbs it.
		data(&mut client, ids[0], 1, WINDOW as usize, false);
		assert_eq!(client.credit_stats().evicted, 1);
		assert_eq!(readable(&mut client), [ids[2]], "newest first");
		assert!(client.local_max_data > client.data_recvd, "the peer has credit");
	}

	/// Repeated frames at the peer's limit spend and return no credit, so they skip the stuck
	/// check rather than each paying for it.
	#[test]
	fn creditless_arrivals_skip_the_stuck_check() {
		const WINDOW: u64 = 1000;
		let (mut client, ids) = stuck(WINDOW);
		data(&mut client, ids[1], 0, 10, false);
		let _ = stop(&mut client, ids[1], 0);
		let room = client.local_max_data - client.data_recvd;
		data(&mut client, ids[0], 0, room as usize, false);
		assert_eq!(client.local_max_data, client.data_recvd, "the peer is at its limit");

		let scans = client.scans;
		for _ in 0..100 {
			data(&mut client, ids[0], 0, 10, false);
			data(&mut client, ids[1], 0, 10, false);
		}
		assert_eq!(client.scans, scans);
	}

	/// A reset that wakes a waiting reader leaves it ready to observe the reset and release its
	/// credit, so another reader's stuck check must not evict anyone in the meantime.
	#[test]
	fn reset_wake_is_not_stuck() {
		const WINDOW: u64 = 1000;
		let (mut client, ids) = stuck(WINDOW);
		data(&mut client, ids[0], 0, WINDOW as usize, false);
		assert_eq!(lease(&mut client, ids[0]), WINDOW as usize);
		assert!(matches!(
			wait(&mut client, ids[0], WINDOW as usize + 1),
			Err(LeaseError::Blocked(_))
		));
		data(&mut client, ids[1], 0, WINDOW as usize, false);
		assert_eq!(lease(&mut client, ids[1]), WINDOW as usize);

		reset(&mut client, ids[0], WINDOW as u32);
		assert!(matches!(
			wait(&mut client, ids[1], WINDOW as usize + 1),
			Err(LeaseError::Blocked(_))
		));
		assert_eq!(client.credit_stats().evicted, 0);
		assert_eq!(
			wait(&mut client, ids[0], WINDOW as usize + 1),
			Err(LeaseError::Reset(0u32.into()))
		);
	}

	/// A reset's final size spends credit like data does, so it counts toward the peak.
	#[test]
	fn reset_counts_toward_peak() {
		let mut client = make(Side::Client);
		let id = StreamId::new(Side::Server, Dir::Uni, 0);
		data(&mut client, id, 0, 10, false);
		reset(&mut client, id, 100);
		assert_eq!(client.credit_stats().peak_spent, 100);
	}

	/// Shrinking the window keeps the limit already advertised, so the peer's credit spent past
	/// the new window still counts toward the peak.
	#[test]
	fn shrink_debt_counts_toward_peak() {
		let mut client = make(Side::Client);
		let id = StreamId::new(Side::Server, Dir::Uni, 0);
		data(&mut client, id, 0, 500, false);
		client.set_receive_window(100u32.into());
		data(&mut client, id, 500, 500, false);
		assert_eq!(client.credit_stats().peak_spent, 1000);
	}

	/// Bytes a reader took outlive an eviction until it stops the stream, so their credit
	/// returns only then, and nothing else is evicted meanwhile.
	#[test]
	fn evicted_taken_bytes_keep_credit_until_stop() {
		const WINDOW: u64 = 1000;
		let (mut client, ids) = stuck(WINDOW);
		let mut pending = Retransmits::default();
		for id in ids {
			data(&mut client, id, 0, WINDOW as usize, false);
			assert_eq!(lease(&mut client, id), WINDOW as usize);
			let _ = recv(&mut client, &mut pending, id).take(WINDOW as usize);
		}
		assert_eq!(readable(&mut client), ids);
		let max_data = client.local_max_data;
		// Debt as large as one stream's bytes, so the first stop only pays it.
		client.set_receive_window(VarInt::from_u64(WINDOW).unwrap());

		assert!(matches!(wait(&mut client, ids[0], 1), Err(LeaseError::Blocked(_))));
		assert_eq!(readable(&mut client), [ids[1]]);
		assert_eq!(client.credit_stats().evicted, 1);
		assert_eq!(client.credit_stats().leased, 2 * WINDOW, "taken credit stays held");
		assert!(matches!(wait(&mut client, ids[0], 1), Err(LeaseError::Blocked(_))));
		assert_eq!(client.credit_stats().evicted, 1, "waits for the stop");
		assert_eq!(wait(&mut client, ids[1], 1), Err(LeaseError::Evicted));

		// The stop pays the debt alone, so eviction resumes with the other stream.
		let _ = stop(&mut client, ids[1], 0);
		assert_eq!(client.local_max_data, max_data);
		assert_eq!(client.credit_stats().evicted, 2);
		assert_eq!(readable(&mut client), [ids[0]]);
		let _ = stop(&mut client, ids[0], 0);
		assert_eq!(client.credit_stats().leased, 0);
		assert_eq!(client.local_max_data, max_data + WINDOW);
	}

	/// Once the peer has spent the window and every reader waits for more than it has, the
	/// lowest-priority stream is evicted: its credit returns, its reader wakes to an error, and
	/// the peer hears only when the application stops it.
	#[test]
	fn stuck_window_evicts_the_lowest_priority() {
		const WINDOW: u64 = 1000;
		let (mut client, [low, high]) = stuck(WINDOW);
		data(&mut client, high, 0, WINDOW as usize, false);
		data(&mut client, low, 0, WINDOW as usize - 1, false);
		let mut pending = Retransmits::default();
		recv(&mut client, &mut pending, low).set_priority(-1);
		recv(&mut client, &mut pending, high).set_priority(1);
		assert_eq!(readable(&mut client), [high, low]);
		assert_eq!(lease(&mut client, high), WINDOW as usize);
		assert_eq!(lease(&mut client, low), WINDOW as usize - 1);
		assert!(matches!(wait(&mut client, high, 2000), Err(LeaseError::Blocked(_))));
		assert!(matches!(wait(&mut client, low, 2000), Err(LeaseError::Blocked(_))));
		assert!(
			client.leased(low).is_some_and(|lease| !lease.evicted),
			"a byte of credit left"
		);
		assert_eq!(client.credit_stats().evicted, 0);

		data(&mut client, low, WINDOW - 1, 1, false);
		// The arrival woke the reader of `low`, which may yet consume.
		assert_eq!(readable(&mut client), [low]);
		assert!(!client.leased(low).unwrap().evicted);
		let blocked = wait(&mut client, low, 2000);
		assert_eq!(blocked, Err(LeaseError::Blocked(ShouldTransmit(true))));
		assert_eq!(readable(&mut client), [low], "the evicted reader wakes");
		assert_eq!(wait(&mut client, low, 1), Err(LeaseError::Evicted));
		assert_eq!(client.credit_stats().leased, WINDOW);
		assert_eq!(client.credit_stats().evicted, 1);
		assert_eq!(client.local_max_data, 3 * WINDOW, "the evicted credit returns");
		assert_eq!(lease(&mut client, high), WINDOW as usize, "the other keeps its bytes");

		let stopped = stop(&mut client, low, 7);
		assert_eq!(stopped.stop_sending.len(), 1, "the application's code tells the peer");
		assert_eq!(stopped.stop_sending[0].error_code, 7u32.into());
		// Late data on the evicted stream is credited on arrival.
		data(&mut client, low, WINDOW, 10, true);
		assert_eq!(client.local_max_data, 3 * WINDOW + 10);
	}

	/// A frame past the window: once its taken bytes fill the window, waiting for one more
	/// evicts it like any stuck holder, so its group reads as evicted rather than broken. The
	/// credit returns when the reader stops the stream and lets go of what it took.
	#[test]
	fn a_frame_past_the_window_is_evicted() {
		const WINDOW: u64 = 1000;
		let (mut client, [frame, _]) = stuck(WINDOW / 2);
		data(&mut client, frame, 0, WINDOW as usize, false);
		assert_eq!(lease(&mut client, frame), WINDOW as usize);
		let mut pending = Retransmits::default();
		let taken = recv(&mut client, &mut pending, frame).take(usize::MAX);
		assert_eq!(taken.len(), WINDOW as usize);

		assert_eq!(
			wait(&mut client, frame, 1),
			Err(LeaseError::Blocked(ShouldTransmit(true)))
		);
		assert_eq!(wait(&mut client, frame, 1), Err(LeaseError::Evicted));
		assert_eq!(client.local_max_data, WINDOW, "the taken bytes are still held");
		drop(taken);
		let _ = stop(&mut client, frame, 0);
		assert_eq!(client.local_max_data, 2 * WINDOW, "the evicted credit returns");
	}

	/// Ties go to the newest stream; a parked holder that never waits is a candidate too.
	#[test]
	fn stuck_window_evicts_the_newest_tie() {
		const WINDOW: u64 = 1000;
		let (mut client, [old, new]) = stuck(WINDOW);
		data(&mut client, old, 0, WINDOW as usize, false);
		// Never leased again after this: its owner parked.
		assert_eq!(lease(&mut client, old), WINDOW as usize);
		data(&mut client, new, 0, WINDOW as usize, false);
		let _ = readable(&mut client);
		assert!(matches!(wait(&mut client, new, 2000), Err(LeaseError::Blocked(_))));
		assert_eq!(wait(&mut client, new, 1), Err(LeaseError::Evicted));
		assert_eq!(lease(&mut client, old), WINDOW as usize);
	}

	/// A stream nobody has leased yet may have a reader about to run, so it holds eviction off
	/// until its reader looks.
	#[test]
	fn unleased_stream_holds_eviction_off() {
		const WINDOW: u64 = 1000;
		let (mut client, [waiting, fresh]) = stuck(WINDOW);
		data(&mut client, waiting, 0, WINDOW as usize, false);
		assert_eq!(lease(&mut client, waiting), WINDOW as usize);
		data(&mut client, fresh, 0, WINDOW as usize, false);
		let mut pending = Retransmits::default();
		recv(&mut client, &mut pending, fresh).set_priority(-1);
		assert!(matches!(wait(&mut client, waiting, 2000), Err(LeaseError::Blocked(_))));
		assert_eq!(client.credit_stats().evicted, 0);

		assert_eq!(lease(&mut client, fresh), WINDOW as usize);
		let blocked = wait(&mut client, fresh, 2000);
		assert_eq!(blocked, Err(LeaseError::Blocked(ShouldTransmit(true))));
		assert_eq!(wait(&mut client, fresh, 1), Err(LeaseError::Evicted));
		assert_eq!(lease(&mut client, waiting), WINDOW as usize);
	}

	/// An evicted stream whose final size is already known is freed by its reader's stop, which
	/// sends its stream ID credit and no STOP_SENDING, since the peer is done.
	#[test]
	fn evicted_stop_issues_stream_credit() {
		const WINDOW: usize = 1000;
		let mut client = StreamsState::new(
			Side::Client,
			2u32.into(),
			2u32.into(),
			1024 * 1024,
			VarInt::from_u64(2 * WINDOW as u64).unwrap(),
			VarInt::from_u64(2 * WINDOW as u64).unwrap(),
		);
		let [parked, waiting] = [0, 1].map(|index| StreamId::new(Side::Server, Dir::Uni, index));
		data(&mut client, parked, 0, WINDOW / 2, false);
		let mut pending = Retransmits::default();
		recv(&mut client, &mut pending, parked).set_priority(-1);
		assert_eq!(lease(&mut client, parked), WINDOW / 2);
		// The rest and the FIN arrive after its reader parked.
		data(&mut client, parked, WINDOW as u64 / 2, WINDOW / 2, true);
		data(&mut client, waiting, 0, WINDOW, false);
		assert_eq!(lease(&mut client, waiting), WINDOW);

		let mut pending = Retransmits::default();
		assert_eq!(
			recv(&mut client, &mut pending, waiting).lease(2 * WINDOW),
			Err(LeaseError::Blocked(ShouldTransmit(true)))
		);
		assert!(client.leased(parked).unwrap().evicted);
		assert!(!pending.max_stream_id[Dir::Uni as usize], "its reader has yet to hear");

		let pending = stop(&mut client, parked, 0);
		assert!(!client.recv.contains_key(&parked), "freed");
		assert!(pending.max_stream_id[Dir::Uni as usize]);
		assert!(pending.stop_sending.is_empty());
	}

	/// Credit left unadvertised is still credit: the peer is not stuck, it just has not heard.
	#[test]
	fn window_with_credit_is_not_stuck() {
		const WINDOW: u64 = 1000;
		let (mut client, [a, b]) = stuck(WINDOW);
		data(&mut client, a, 0, WINDOW as usize, false);
		data(&mut client, b, 0, WINDOW as usize, false);
		assert_eq!(lease(&mut client, a), WINDOW as usize);
		release(&mut client, a, 1);
		assert!(matches!(wait(&mut client, b, 2000), Err(LeaseError::Blocked(_))));
		assert!(matches!(wait(&mut client, a, 2000), Err(LeaseError::Blocked(_))));
		assert_eq!(client.credit_stats().evicted, 0);
	}

	/// A rank set on a stream read plainly to its end goes with the stream, and a closed stream
	/// takes no rank.
	#[test]
	fn priority_goes_with_a_plainly_read_stream() {
		let mut client = make(Side::Client);
		let id = StreamId::new(Side::Server, Dir::Uni, 0);
		let mut pending = Retransmits::default();
		data(&mut client, id, 0, 10, true);
		recv(&mut client, &mut pending, id).set_priority(1);
		let mut stream = recv(&mut client, &mut pending, id);
		let mut chunks = stream.read().unwrap();
		assert_eq!(chunks.next(usize::MAX).unwrap().unwrap().bytes.len(), 10);
		assert_eq!(chunks.next(usize::MAX), Ok(None));
		let _ = chunks.finalize();
		assert!(client.leased(id).is_none());

		recv(&mut client, &mut pending, id).set_priority(2);
		assert!(client.leased(id).is_none());
		assert!(!client.recv.contains_key(&id));
	}

	/// A stopped stream stays closed: leasing it again cannot revive it for a second stop.
	#[test]
	fn lease_after_stop_is_closed() {
		let mut client = make(Side::Client);
		let id = StreamId::new(Side::Server, Dir::Uni, 0);
		data(&mut client, id, 0, 10, true);
		assert_eq!(lease(&mut client, id), 10);
		let mut pending = Retransmits::default();
		assert!(recv(&mut client, &mut pending, id).stop(0u32.into()).is_ok());
		assert_eq!(wait(&mut client, id, 0), Err(LeaseError::ClosedStream));
		assert!(recv(&mut client, &mut pending, id).stop(0u32.into()).is_err());
		assert!(client.leased(id).is_none());
	}

	/// A reset already returned the credit of the data it abandoned, so a stream holding nothing
	/// else frees nothing when evicted: it is passed over, and its reader still sees the reset.
	#[test]
	fn reset_stream_ranks_by_held_credit() {
		const WINDOW: u64 = 1000;
		let (mut client, [waiting, reset_id]) = stuck(WINDOW);
		// Its reader looked and parked, then data it never leased arrives before the reset.
		data(&mut client, reset_id, 0, 1, false);
		assert_eq!(lease(&mut client, reset_id), 1);
		release(&mut client, reset_id, 1);
		data(&mut client, reset_id, 1, WINDOW as usize - 1, false);
		reset(&mut client, reset_id, WINDOW as u32);

		let room = (client.local_max_data - client.data_recvd) as usize;
		data(&mut client, waiting, 0, room, false);
		assert_eq!(lease(&mut client, waiting), room);
		// One byte taken, so the reader waits for a unit the window can hold.
		let mut pending = Retransmits::default();
		let _ = recv(&mut client, &mut pending, waiting).take(1);
		assert!(matches!(wait(&mut client, waiting, room), Err(LeaseError::Blocked(_))));
		assert_eq!(client.credit_stats().evicted, 1);
		assert_eq!(wait(&mut client, waiting, 1), Err(LeaseError::Evicted));
		assert_eq!(wait(&mut client, reset_id, 1), Err(LeaseError::Reset(0u32.into())));
	}

	/// A reset below what a lease already read and partly took leaves those bytes leased, and
	/// every byte of the final size returns its credit exactly once.
	#[test]
	fn reset_at_below_a_taken_lease_credits_once() {
		let mut client = make(Side::Client);
		let initial = client.local_max_data;
		let id = StreamId::new(Side::Server, Dir::Uni, 0);
		data(&mut client, id, 0, 100, false);
		assert_eq!(lease(&mut client, id), 100);
		let mut pending = Retransmits::default();
		assert_eq!(recv(&mut client, &mut pending, id).take(60).len(), 60);

		let _ = client.received_reset(reset_at(id, 7, 200, Some(50))).unwrap();
		assert_eq!(client.local_max_data, initial + 100, "the abandoned tail");
		assert_eq!(wait(&mut client, id, 41), Err(LeaseError::Reset(7u32.into())));
		assert_eq!(client.leased(id).unwrap().len(), 40, "read before the reset");

		release(&mut client, id, 0);
		assert_eq!(client.local_max_data, initial + 160);
		let _ = stop(&mut client, id, 0);
		assert_eq!(client.local_max_data, initial + 200);
		assert_eq!(client.credit_stats().leased, 0);
		assert!(!client.recv.contains_key(&id));
	}

	/// Growing the window back cancels the debt a shrink left, so the peer is credited once.
	#[test]
	fn shrink_then_grow_credits_once() {
		let mut client = make(Side::Client);
		let initial = client.local_max_data;
		let window = client.receive_window;
		let id = StreamId::new(Side::Server, Dir::Uni, 0);

		client.set_receive_window(VarInt::from_u64(window / 2).unwrap());
		client.set_receive_window(VarInt::from_u64(window).unwrap());
		assert_eq!(client.local_max_data, initial);

		// Debt partly paid by a consume before the window grows back.
		client.set_receive_window(VarInt::from_u64(window / 2).unwrap());
		data(&mut client, id, 0, 20, false);
		assert_eq!(drain(&mut client, id).0, 20);
		assert_eq!(client.local_max_data, initial);
		client.set_receive_window(VarInt::from_u64(window).unwrap());
		assert_eq!(client.local_max_data, initial + 20);
		assert_eq!(client.receive_window_shrink_debt, 0);
	}

	/// A peer that spends each credit an eviction frees on another incomplete unit forces one
	/// eviction per round trip, so each must cost less than a scan of every stream.
	#[test]
	fn repeated_evictions_stay_cheap() {
		const STREAMS: u64 = 512;
		const CYCLES: u64 = 64;
		let mut client = StreamsState::new(
			Side::Client,
			4096u32.into(),
			4096u32.into(),
			1024 * 1024,
			VarInt::from_u64(STREAMS).unwrap(),
			VarInt::from_u64(STREAMS).unwrap(),
		);
		for index in 0..STREAMS + CYCLES {
			let id = StreamId::new(Side::Server, Dir::Uni, index);
			data(&mut client, id, 0, 1, false);
			assert!(matches!(wait(&mut client, id, 2), Err(LeaseError::Blocked(_))));
			if index == STREAMS - 1 {
				client.scans = 0;
			}
		}
		assert_eq!(client.credit_stats().evicted, 1 + CYCLES);
		assert!(
			client.scans <= 4 * CYCLES as usize,
			"{} entries looked at for {CYCLES} evictions among {STREAMS} streams",
			client.scans
		);
	}

	/// A lease that fails reports nothing to transmit, so it must not queue anything either:
	/// the frames would wait for an unrelated wake.
	#[test]
	fn failed_lease_queues_nothing() {
		let mut client = StreamsState::new(
			Side::Client,
			2u32.into(),
			2u32.into(),
			1024 * 1024,
			1000u32.into(),
			64u32.into(),
		);
		let id = StreamId::new(Side::Server, Dir::Uni, 0);
		data(&mut client, id, 0, 64, false);
		let mut pending = Retransmits::default();
		assert_eq!(
			recv(&mut client, &mut pending, id).lease(1001),
			Err(LeaseError::TooLarge)
		);
		assert!(pending.max_stream_data.is_empty());
		assert_eq!(recv(&mut client, &mut pending, id).lease(0), Ok(ShouldTransmit(true)));
		assert!(pending.max_stream_data.contains(&id));

		let id = StreamId::new(Side::Server, Dir::Uni, 1);
		data(&mut client, id, 0, 10, false);
		reset(&mut client, id, 10);
		let mut pending = Retransmits::default();
		assert_eq!(
			recv(&mut client, &mut pending, id).lease(11),
			Err(LeaseError::Reset(0u32.into()))
		);
		assert!(!pending.max_stream_id[Dir::Uni as usize]);
		let pending = stop(&mut client, id, 0);
		assert!(pending.max_stream_id[Dir::Uni as usize], "the stop frees it");
		assert!(pending.stop_sending.is_empty(), "the peer already reset it");
	}

	/// Observing a reset frees the stream, so the credit its lease still held is announced then:
	/// nothing is left to release or stop that could announce it later.
	#[test]
	fn observed_reset_announces_leased_credit() {
		let mut client = make(Side::Client);
		let initial = client.local_max_data;
		let len = (client.receive_window / 8) as usize;
		let id = StreamId::new(Side::Server, Dir::Uni, 0);
		data(&mut client, id, 0, len, false);
		assert_eq!(lease(&mut client, id), len);
		reset(&mut client, id, len as u32);
		assert_eq!(client.credit_stats().leased, len as u64);

		let mut pending = Retransmits::default();
		assert_eq!(
			recv(&mut client, &mut pending, id).received_reset(),
			Ok(Some(0u32.into()))
		);
		client.check_leases();
		assert_eq!(client.local_max_data, initial + len as u64);
		assert!(pending.max_data);
	}

	/// An evicted stream tells the peer once, when its reader stops it, and frees its stream ID
	/// once its final size is known too, whichever comes first.
	#[test]
	fn evicted_stop_and_fin_tell_the_peer_once() {
		const WINDOW: u64 = 1000;
		for fin_first in [false, true] {
			let mut client = StreamsState::new(
				Side::Client,
				2u32.into(),
				2u32.into(),
				1024 * 1024,
				VarInt::from_u64(WINDOW).unwrap(),
				VarInt::from_u64(2 * WINDOW).unwrap(),
			);
			let id = StreamId::new(Side::Server, Dir::Uni, 0);
			data(&mut client, id, 0, WINDOW as usize, false);
			assert_eq!(lease(&mut client, id), WINDOW as usize);
			let mut pending = Retransmits::default();
			let _ = recv(&mut client, &mut pending, id).take(1);
			assert!(matches!(
				wait(&mut client, id, WINDOW as usize),
				Err(LeaseError::Blocked(_))
			));
			assert_eq!(wait(&mut client, id, 1), Err(LeaseError::Evicted));

			let mut frames = Retransmits::default();
			if fin_first {
				data(&mut client, id, WINDOW, 10, true);
				client.queue_max_stream_id(&mut frames);
			}
			frames |= stop(&mut client, id, 7);
			if !fin_first {
				data(&mut client, id, WINDOW, 10, true);
			}
			client.queue_max_stream_id(&mut frames);
			frames |= stop(&mut client, id, 7);

			let expected = usize::from(!fin_first);
			assert_eq!(frames.stop_sending.len(), expected, "fin_first: {fin_first}");
			assert!(frames.max_stream_id[Dir::Uni as usize], "fin_first: {fin_first}");
			assert!(!client.recv.contains_key(&id));
			assert_eq!(client.credit_stats().leased, 0);
		}
	}

	/// A group token indexes its own connection's schedule, so another connection refuses it
	/// rather than reach whichever of its own groups shares the index.
	#[test]
	#[should_panic(expected = "send group used on another connection")]
	fn send_group_from_another_connection() {
		let mut a = make(Side::Client);
		let mut b = make(Side::Client);
		let group = crate::SendGroups { state: &mut a }.open(0);
		let _other = crate::SendGroups { state: &mut b }.open(0);
		crate::SendGroups { state: &mut b }.set_priority(&group, 1);
	}

	/// Ingest and read rounds across many streams, with a plain read or a lease released whole.
	/// The lease cost must stay flat as the stream count grows.
	#[test]
	#[ignore = "release benchmark, run by nightly"]
	fn benchmark_receive_credit() {
		const BYTES: usize = 1 << 30;
		println!("streams,payload,plain_ns_per_read,lease_ns_per_read");
		for streams in [1u64, 64, 4096] {
			for payload in [1200usize, 64 * 1024] {
				let rounds = (BYTES / payload / streams as usize).max(1) as u64;
				let chunk = Bytes::from(vec![0; payload]);
				let mut ns = [0f64; 2];
				for (mode, leased) in [false, true].into_iter().enumerate() {
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
							if leased {
								let _ = recv.lease(1).unwrap();
								std::hint::black_box(recv.take(usize::MAX));
								let _ = recv.release();
							} else {
								let mut chunks = recv.read().unwrap();
								std::hint::black_box(chunks.next(usize::MAX).unwrap());
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

	#[test]
	fn max_stream_data_over_stream_limit_uni() {
		let mut server = make(Side::Server);
		let id = StreamId::new(Side::Client, Dir::Uni, 1 << 40);
		let result = server.received_max_stream_data(id, 1);
		assert_eq!(result.map_err(|e| e.code), Err(TransportErrorCode::STREAM_LIMIT_ERROR),);
	}

	#[test]
	fn max_stream_data_recv_only() {
		let mut server = make(Side::Server);
		let id = StreamId::new(Side::Client, Dir::Uni, 0);
		let result = server.received_max_stream_data(id, 1);
		assert_eq!(result.map_err(|e| e.code), Err(TransportErrorCode::STREAM_STATE_ERROR),);
	}

	#[test]
	fn max_stream_data_local_unopened() {
		let mut server = make(Side::Server);
		let id = StreamId::new(Side::Server, Dir::Uni, 0);
		let result = server.received_max_stream_data(id, 1);
		assert_eq!(result.map_err(|e| e.code), Err(TransportErrorCode::STREAM_STATE_ERROR),);
	}

	#[test]
	fn stop_sending_over_stream_limit_bidi() {
		let mut server = make(Side::Server);
		let id = StreamId::new(Side::Client, Dir::Bi, 1 << 40);
		let result = server.received_stop_sending(id, 0u32.into());
		assert_eq!(result.map_err(|e| e.code), Err(TransportErrorCode::STREAM_LIMIT_ERROR),);
	}

	#[test]
	fn stop_sending_over_stream_limit_uni() {
		let mut server = make(Side::Server);
		let id = StreamId::new(Side::Client, Dir::Uni, 1 << 40);
		let result = server.received_stop_sending(id, 0u32.into());
		assert_eq!(result.map_err(|e| e.code), Err(TransportErrorCode::STREAM_LIMIT_ERROR),);
	}

	#[test]
	fn stop_sending_recv_only() {
		let mut server = make(Side::Server);
		let id = StreamId::new(Side::Client, Dir::Uni, 0);
		let result = server.received_stop_sending(id, 0u32.into());
		assert_eq!(result.map_err(|e| e.code), Err(TransportErrorCode::STREAM_STATE_ERROR),);
	}

	#[test]
	fn stream_limit_checked_against_announced_value() {
		let mut client = make(Side::Client);
		client.set_max_concurrent(Dir::Bi, 256u32.into());
		let id = StreamId::new(Side::Server, Dir::Bi, 200);

		// Raised locally, but no MAX_STREAMS has told the peer yet
		assert_eq!(
			client.received_stop_sending(id, 0u32.into()).map_err(|e| e.code),
			Err(TransportErrorCode::STREAM_LIMIT_ERROR),
		);
		assert_eq!(
			client.received_max_stream_data(id, 1).map_err(|e| e.code),
			Err(TransportErrorCode::STREAM_LIMIT_ERROR),
		);

		let mut pending = Retransmits::default();
		assert!(client.queue_max_stream_id(&mut pending));
		let mut buf = Vec::new();
		client.write_control_frames(
			&mut buf,
			&mut pending,
			&mut ThinRetransmits::default(),
			&mut FrameStats::default(),
			1200,
		);
		assert!(!buf.is_empty());

		assert_eq!(client.received_max_stream_data(id, 1), Ok(()));
		assert!(client.received_stop_sending(id, 0u32.into()).is_ok());
	}

	#[test]
	fn stop_sending_local_unopened() {
		let mut server = make(Side::Server);
		let id = StreamId::new(Side::Server, Dir::Uni, 0);
		let result = server.received_stop_sending(id, 0u32.into());
		assert_eq!(result.map_err(|e| e.code), Err(TransportErrorCode::STREAM_STATE_ERROR),);
	}
}
