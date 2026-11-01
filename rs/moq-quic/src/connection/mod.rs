//! A single QUIC connection: its state machine, the events it reports, and why it ends.

use std::{
	cmp,
	collections::VecDeque,
	convert::TryFrom,
	fmt, io, mem,
	net::{IpAddr, SocketAddr},
	sync::Arc,
	task::Poll,
};

use bytes::{Bytes, BytesMut};
use frame::StreamMetaVec;

use rand::{RngExt, SeedableRng, rngs::StdRng};
use thiserror::Error;
use tracing::{debug, error, trace, trace_span, warn};

use crate::{
	Dir, Duration, Frame, Instant, Limits, LimitsError, MAX_CID_SIZE, MAX_STREAM_COUNT, MIN_INITIAL_SIZE, Rate, Side,
	StreamId, TIMER_GRANULARITY, TokenStore, Transmit, TransportError, TransportErrorCode, VarInt, Version,
	cid_queue::CidQueue,
	coding::BufMutExt,
	connection::spaces::LostPacket,
	crypto::{self, KeyPair, Keys, PacketKey},
	frame::{self, Close, FrameStruct, NewConnectionId, NewToken},
	packet::{
		FixedLengthConnectionIdParser, Header, InitialHeader, InitialPacket, LongType, Packet, PacketNumber,
		PartialDecode, SpaceId,
	},
	range_set::ArrayRangeSet,
	server,
	shared::{
		ConnectionEvent, ConnectionEventInner, ConnectionId, DatagramConnectionEvent, EcnCodepoint, EndpointEvent,
		EndpointEventInner,
	},
	token::{ResetToken, Token, TokenPayload},
	transport,
	transport_parameters::TransportParameters,
};

mod ack_frequency;
use ack_frequency::AckFrequencyState;

mod assembler;

mod bandwidth_probe;
use bandwidth_probe::BandwidthProbe;

mod cid_state;
use cid_state::CidState;

mod datagrams;
use datagrams::DatagramState;
pub use datagrams::Datagrams;

mod mtud;
#[cfg(feature = "mux")]
#[doc(hidden)]
pub mod mux;

mod packet_builder;
use packet_builder::PacketBuilder;

mod packet_crypto;
use packet_crypto::PrevCrypto;

mod paths;
use paths::{PathData, PathResponses};

pub(crate) mod qlog;
use qlog::QlogSink;

mod rate;

mod send_buffer;

mod sent_packets;
#[cfg(feature = "fuzz")]
#[doc(hidden)]
pub use sent_packets::Bench as BenchSentPackets;
mod slots;
mod spaces;
#[cfg(any(test, feature = "fuzz"))]
pub(crate) use spaces::Retransmits;
use spaces::{PacketNumberFilter, PacketSpace, SendableFrames, SentPacket, ThinRetransmits};

mod stats;

/// Identifies one connection, so state handed out by it cannot be returned to another.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ConnectionTag(u64);

impl ConnectionTag {
	/// A tag no other connection in this process has.
	pub(crate) fn next() -> Self {
		static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
		Self(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
	}
}
pub use stats::{CreditStats, FrameStats, PathStats, Stats, UdpStats};

mod streams;
pub(crate) use streams::StreamsState;
pub use streams::{ShouldTransmit, StreamEvent, Streams, recv, send};

mod timer;

pub(crate) mod waits;
use crate::congestion::{self, Controller};
use timer::{Timer, TimerTable};

/// Protocol state and logic for a single QUIC connection
///
/// Objects of this type receive [`ConnectionEvent`]s and emit [`EndpointEvent`]s and application
/// [`Event`]s to make progress. To handle timeouts, a `Connection` returns timer updates and
/// expects timeouts through various methods. A number of simple getter methods are exposed
/// to allow callers to inspect some of the connection state.
///
/// `Connection` has roughly 4 types of methods:
///
/// - A. Simple getters, taking `&self`
/// - B. Handlers for incoming events from the network or system, named `handle_*`.
/// - C. State machine mutators, for incoming commands from the application. For convenience we
///   refer to this as "performing I/O" below, however as per the design of this library none of the
///   functions actually perform system-level I/O. For example, [`read`](recv::Stream::read) and
///   [`write`](send::Stream::write), but also things like [`reset`](send::Stream::reset).
/// - D. Polling functions for outgoing events or actions for the caller to
///   take, named `poll_*`.
///
/// The simplest way to use this API correctly is to call (B) and (C) whenever
/// appropriate, then after each of those calls, as soon as feasible call all
/// polling methods (D) and deal with their outputs appropriately, e.g. by
/// passing it to the application or by making a system-level I/O call. You
/// should call the polling functions in this order:
///
/// 1. [`poll_transmit`](Self::poll_transmit)
/// 2. [`poll_timeout`](Self::poll_timeout)
/// 3. [`poll_endpoint_events`](Self::poll_endpoint_events)
/// 4. [`poll`](Self::poll)
///
/// Currently the only actual dependency is from (2) to (1), however additional
/// dependencies may be added in future, so the above order is recommended.
///
/// (A) may be called whenever desired.
///
/// Care should be made to ensure that the input events represent monotonically
/// increasing time. Specifically, calling [`handle_timeout`](Self::handle_timeout)
/// with events of the same [`Instant`] may be interleaved in any order with a
/// call to [`handle_event`](Self::handle_event) at that same instant; however
/// events or timeouts with different instants must not be interleaved.
pub struct Connection {
	/// Whether the endpoint accepts any value for the fixed bit
	grease_quic_bit: bool,
	config: Arc<transport::Config>,
	/// Where qlog events go, from [`transport::Config::qlog`]
	qlog: QlogSink,
	rng: StdRng,
	crypto: Box<dyn crypto::Session>,
	/// The CID we initially chose, for use during the handshake
	handshake_cid: ConnectionId,
	/// The CID the peer initially chose, for use during the handshake
	rem_handshake_cid: ConnectionId,
	/// The "real" local IP address which was was used to receive the initial packet.
	/// This is only populated for the server case, and if known
	local_ip: Option<IpAddr>,
	path: PathData,
	/// Incremented every time we see a new path
	///
	/// Stored separately from `path.generation` to account for aborted migrations
	path_counter: u64,
	/// Whether MTU detection is supported in this environment
	allow_mtud: bool,
	prev_path: Option<(ConnectionId, PathData)>,
	state: State,
	side: ConnectionSide,
	key_phase: bool,
	/// How many packets are in the current key phase. Used only for `Data` space.
	key_phase_size: u64,
	/// Transport parameters set by the peer
	peer_params: TransportParameters,
	/// Source ConnectionId of the first packet received from the peer
	orig_rem_cid: ConnectionId,
	/// Destination ConnectionId sent by the client on the first Initial
	initial_dst_cid: ConnectionId,
	/// The value that the server included in the Source Connection ID field of a Retry packet, if
	/// one was received
	retry_src_cid: Option<ConnectionId>,
	events: VecDeque<Event>,
	endpoint_events: VecDeque<EndpointEventInner>,
	/// Whether `EndpointEventInner::Draining` was emitted, which happens once.
	draining_reported: bool,
	/// Whether the spin bit is in use for this connection
	spin_enabled: bool,
	/// Outgoing spin bit state
	spin: bool,
	/// Packet number spaces: initial, handshake, 1-RTT
	spaces: [PacketSpace; 3],
	/// Highest usable packet number space
	highest_space: SpaceId,
	/// 1-RTT keys used prior to a key update
	prev_crypto: Option<PrevCrypto>,
	/// 1-RTT keys to be used for the next key update
	///
	/// These are generated in advance to prevent timing attacks and/or DoS by third-party attackers
	/// spoofing key updates.
	next_crypto: Option<KeyPair<Box<dyn PacketKey>>>,
	/// Whether the idle timer should be reset the next time an ack-eliciting packet is transmitted.
	permit_idle_reset: bool,
	/// Negotiated idle timeout
	idle_timeout: Option<Duration>,
	/// Keep-alive interval, derived from [`Self::idle_timeout`] unless configured
	keep_alive: Option<Duration>,
	/// The time we send next bundled ACK
	///
	/// The goal is to wait long enough for the peer to acknowledge our previous
	/// bundled ACK (see `next_bundled_ack_delay`).
	/// A packet-count threshold would over- or under-shoot this depending on how fast we happen
	/// to be sending, so a time threshold is used instead.
	next_bundled_ack_time: Option<Instant>,
	timers: TimerTable,
	/// Number of packets received which could not be authenticated
	authentication_failures: u64,
	/// Authenticated 1-RTT packets that arrived before the handshake completed, to be processed
	/// once it has
	early_1rtt: Vec<EarlyPacket>,
	/// Why the connection was lost, if it has been
	error: Option<Error>,
	/// Identifies Data-space packet numbers to skip. Not used in earlier spaces.
	packet_number_filter: PacketNumberFilter,

	//
	// Queued non-retransmittable 1-RTT data
	//
	/// Responses to PATH_CHALLENGE frames
	path_responses: PathResponses,
	close: bool,

	//
	// ACK frequency
	//
	ack_frequency: AckFrequencyState,

	//
	// Loss Detection
	//
	/// The number of times a PTO has been sent without receiving an ack.
	pto_count: u32,
	/// Whether a packet from the peer has authenticated since the last ack-eliciting 1-RTT packet
	/// went out. Kept in event order: a packet handled and one sent in the same poll share an
	/// instant.
	heard_since_sent: bool,
	/// A probe timed out with the peer unheard since the packet it probed for went out, so the
	/// path may have been down: the next packet heard brings the next probe forward.
	probe_when_heard: bool,

	//
	// Congestion Control
	//
	/// Whether the most recently received packet had an ECN codepoint set
	receiving_ecn: bool,
	/// Whether the last `poll_transmit` call ran out of data because there was
	/// no outgoing application data.
	app_limited: bool,
	/// Reused to hand the congestion controller the packets an ACK acknowledged
	acked: Vec<congestion::Acked>,
	/// Resends in-flight data to probe for bandwidth while `app_limited`
	bandwidth_probe: BandwidthProbe,
	/// Holds sending to a cap below whatever the congestion controller allows
	send_rate: Option<rate::Bucket>,

	streams: StreamsState,
	/// Surplus remote CIDs for future use on new paths
	rem_cids: CidQueue,
	// Attributes of CIDs generated by local peer
	local_cid_state: CidState,
	/// State of the unreliable datagram extension
	datagrams: DatagramState,
	/// Connection level statistics
	stats: Stats,
	/// QUIC version used for the connection.
	version: Version,
}

impl Connection {
	pub(crate) fn new(crypto: Box<dyn crypto::Session>, args: ConnectionArgs) -> Self {
		let ConnectionArgs {
			grease_quic_bit,
			transport_config: config,
			init_cid,
			loc_cid,
			rem_cid,
			remote,
			local_ip,
			local_cid_len,
			local_cid_lifetime,
			now,
			version,
			allow_mtud,
			rng_seed,
			side_args,
		} = args;
		let pref_addr_cid = side_args.pref_addr_cid();
		let path_validated = side_args.path_validated();
		let connection_side = ConnectionSide::from(side_args);
		let side = connection_side.side();
		let initial_space = PacketSpace {
			crypto: Some(crypto.initial_keys(init_cid, side)),
			..PacketSpace::new(now)
		};
		let state = State::Handshake(state::Handshake {
			rem_cid_set: side.is_server(),
			expected_token: Bytes::new(),
			client_hello: None,
		});
		let mut rng = StdRng::from_seed(rng_seed);
		let mut this = Self {
			grease_quic_bit,
			qlog: config.qlog_sink(),
			crypto,
			handshake_cid: loc_cid,
			rem_handshake_cid: rem_cid,
			local_cid_state: CidState::new(
				local_cid_len,
				local_cid_lifetime,
				now,
				if pref_addr_cid.is_some() { 2 } else { 1 },
			),
			path: PathData::new(remote, allow_mtud, None, 0, now, &config),
			path_counter: 0,
			allow_mtud,
			local_ip,
			prev_path: None,
			state,
			side: connection_side,
			key_phase: false,
			// A small initial key phase size ensures peers that don't handle key updates correctly
			// fail sooner rather than later. It's okay for both peers to do this, as the first one
			// to perform an update will reset the other's key phase size in `update_keys`, and a
			// simultaneous key update by both is just like a regular key update with a really fast
			// response. Inspired by quic-go's similar behavior of performing the first key update
			// at the 100th short-header packet.
			key_phase_size: rng.random_range(10..1000),
			peer_params: TransportParameters::default(),
			orig_rem_cid: rem_cid,
			initial_dst_cid: init_cid,
			retry_src_cid: None,
			events: VecDeque::new(),
			endpoint_events: VecDeque::new(),
			draining_reported: false,
			spin_enabled: config.allow_spin && rng.random_ratio(7, 8),
			spin: false,
			spaces: [initial_space, PacketSpace::new(now), PacketSpace::new(now)],
			highest_space: SpaceId::Initial,
			prev_crypto: None,
			next_crypto: None,
			permit_idle_reset: true,
			idle_timeout: match config.max_idle_timeout() {
				None | Some(VarInt(0)) => None,
				Some(dur) => Some(Duration::from_millis(dur.0)),
			},
			// Armed only once established, after the peer's parameters set it.
			keep_alive: None,
			timers: TimerTable::default(),
			authentication_failures: 0,
			early_1rtt: Vec::new(),
			error: None,
			#[cfg(test)]
			packet_number_filter: match config.deterministic_packet_numbers {
				false => PacketNumberFilter::new(&mut rng),
				true => PacketNumberFilter::disabled(),
			},
			#[cfg(not(test))]
			packet_number_filter: PacketNumberFilter::new(&mut rng),

			path_responses: PathResponses::default(),
			close: false,

			ack_frequency: AckFrequencyState::new(get_max_ack_delay(&TransportParameters::default())),
			next_bundled_ack_time: None,

			pto_count: 0,
			heard_since_sent: false,
			probe_when_heard: false,

			app_limited: false,
			send_rate: None,
			acked: Vec::new(),
			bandwidth_probe: BandwidthProbe::new(&config, now),
			receiving_ecn: false,

			streams: StreamsState::new(
				side,
				config.max_streams(),
				config.max_streams(),
				config.send_window(),
				config.receive_window(),
				config.stream_receive_window(),
			),
			datagrams: DatagramState::default(),
			config,
			rem_cids: CidQueue::new(rem_cid),
			rng,
			stats: Stats::default(),
			version,
		};
		// The caps take the live path, so the initial window counts as credit already granted.
		let rates = Limits {
			receive_rate: this.config.limits.receive_rate,
			send_rate: this.config.limits.send_rate,
			..Limits::default()
		};
		this.set_limits(rates).expect("rate caps are always valid");
		if path_validated {
			this.on_path_validated();
		}
		if side.is_client() {
			// Kick off the connection
			this.write_crypto();
		}
		this
	}

	/// Returns the next time at which `handle_timeout` should be called
	///
	/// The value returned may change after:
	/// - the application performed some I/O on the connection
	/// - a call was made to `handle_event`
	/// - a call to `poll_transmit` returned `Some`
	/// - a call was made to `handle_timeout`
	#[must_use]
	pub fn poll_timeout(&self) -> Option<Instant> {
		let credit = match self.state.is_closed() {
			true => None,
			false => self.streams.receive_rate_deadline(),
		};
		match (self.timers.next_timeout(), credit) {
			(Some(timer), Some(credit)) => Some(timer.min(credit)),
			(timer, credit) => timer.or(credit),
		}
	}

	/// Wait for the handshake to complete
	///
	/// Ready once it did, even if the connection was lost since, or with the reason the
	/// connection was lost first.
	pub fn poll_connected(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		self.streams.waits.poll_connected(waiter)
	}

	/// Wait for the handshake to be confirmed, as [`poll_connected`](Self::poll_connected) does
	pub fn poll_handshake_confirmed(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		self.streams.waits.poll_confirmed(waiter)
	}

	/// Wait for the connection to be lost, returning why
	///
	/// A local [`close()`](Self::close) reports [`Error::LocallyClosed`].
	pub fn poll_closed(&mut self, waiter: &kio::Waiter) -> Poll<Error> {
		self.streams.waits.poll_closed(waiter)
	}

	/// Returns application-facing events
	///
	/// Connections should be polled for events after:
	/// - a call was made to `handle_event`
	/// - a call was made to `handle_timeout`
	#[must_use]
	pub fn poll(&mut self) -> Option<Event> {
		if let Some(x) = self.events.pop_front() {
			return Some(x);
		}

		if let Some(event) = self.streams.poll() {
			return Some(Event::Stream(event));
		}

		if let Some(err) = self.error.take() {
			return Some(Event::ConnectionLost { reason: err });
		}

		None
	}

	/// Return endpoint-facing events
	#[must_use]
	pub fn poll_endpoint_events(&mut self) -> Option<EndpointEvent> {
		self.endpoint_events.pop_front().map(EndpointEvent)
	}

	/// Provide control over streams
	#[must_use]
	pub fn streams(&mut self) -> Streams<'_> {
		Streams {
			state: &mut self.streams,
			conn_state: &self.state,
		}
	}

	/// Provide control over the send groups that schedule outgoing streams
	#[must_use]
	pub fn send_groups(&mut self) -> send::Groups<'_> {
		send::Groups {
			state: &mut self.streams,
		}
	}

	/// Provide control over streams
	#[must_use]
	pub fn recv_stream(&mut self, id: StreamId) -> recv::Stream<'_> {
		assert!(id.dir() == Dir::Bi || id.initiator() != self.side.side());
		recv::Stream {
			id,
			state: &mut self.streams,
			pending: &mut self.spaces[SpaceId::Data].pending,
		}
	}

	/// Provide control over streams
	#[must_use]
	pub fn send_stream(&mut self, id: StreamId) -> send::Stream<'_> {
		assert!(id.dir() == Dir::Bi || id.initiator() == self.side.side());
		send::Stream {
			id,
			state: &mut self.streams,
			pending: &mut self.spaces[SpaceId::Data].pending,
			conn_state: &self.state,
		}
	}

	/// Returns packets to transmit
	///
	/// Connections should be polled for transmit after:
	/// - the application performed some I/O on the connection
	/// - a call was made to `handle_event`
	/// - a call was made to `handle_timeout`
	///
	/// `max_datagrams` specifies how many datagrams can be returned inside a
	/// single Transmit using GSO. This must be at least 1.
	#[must_use]
	pub fn poll_transmit(&mut self, now: Instant, max_datagrams: usize, buf: &mut Vec<u8>) -> Option<Transmit> {
		assert!(max_datagrams != 0);
		let max_datagrams = match self.config.segmentation_offload {
			false => 1,
			true => max_datagrams,
		};
		// `C.send_quantum` bounds one aggregate scheduled and transmitted together as a unit,
		// which for a GSO batch is its datagram count. Controllers that don't compute one leave
		// the batch bounded only by what the caller offered.
		// <https://www.ietf.org/archive/id/draft-ietf-ccwg-bbr-06.html#section-5.6.3>
		let max_datagrams = match self.path.congestion.burst() {
			Some(send_quantum) => {
				let datagrams = send_quantum / u64::from(self.path.current_mtu());
				max_datagrams.min(usize::try_from(datagrams).unwrap_or(usize::MAX).max(1))
			}
			None => max_datagrams,
		};

		let mut num_datagrams = 0;
		// Position in `buf` of the first byte of the current UDP datagram. When coalescing QUIC
		// packets, this can be earlier than the start of the current QUIC packet.
		let mut datagram_start = 0;
		let mut segment_size = usize::from(self.path.current_mtu());
		// The peer's maximum UDP payload size can reduce the path MTU below our configured minimum.
		let min_mtu = Ord::min(self.config.min_mtu, self.path.current_mtu());

		if let Some(challenge) = self.send_path_challenge(now, buf) {
			return Some(challenge);
		}

		// If we need to send a probe, make sure we have something to send.
		for space in SpaceId::iter() {
			if space != SpaceId::Data {
				self.spaces[space].maybe_queue_probe(false, false, &self.streams);
				continue;
			}

			let has_ack_eliciting_data =
				self.can_send_1rtt(Ord::min(segment_size, usize::from(min_mtu)).saturating_sub(
					self.predict_1rtt_overhead(Some(self.packet_number_filter.peek(&self.spaces[SpaceId::Data]))),
				));
			let request_immediate_ack = self.peer_supports_ack_frequency();

			self.spaces[space].maybe_queue_probe(request_immediate_ack, has_ack_eliciting_data, &self.streams);
		}

		// Check whether we need to send a close message
		let close = match self.state {
			State::Drained => {
				self.app_limited = true;
				return None;
			}
			State::Draining | State::Closed(_) => {
				// self.close is only reset once the associated packet had been
				// encoded successfully
				if !self.close {
					self.app_limited = true;
					return None;
				}
				true
			}
			_ => false,
		};

		// A client is `Connected` before the server has its Finished: it may still be queued
		// behind the pacer, or lost. A close that replaces it leaves the server unable to complete
		// its handshake, so it only reads an Initial or Handshake close, which carries no
		// application code (RFC 9000 §10.2.3). So the Handshake packet carries the unacknowledged
		// Finished instead, and the server reads the 1-RTT close behind it.
		let finish_first = close
			&& self.side.is_client()
			&& self.highest_space == SpaceId::Data
			&& self.spaces[SpaceId::Handshake].crypto.is_some()
			&& self.spaces[SpaceId::Handshake].unacked_crypto().next().is_some();

		// Check whether we need to send an ACK_FREQUENCY frame
		if let Some(config) = &self.config.ack_frequency {
			self.spaces[SpaceId::Data].pending.ack_frequency =
				self.ack_frequency
					.should_send_ack_frequency(self.path.rtt.get(), config, &self.peer_params)
					&& self.highest_space == SpaceId::Data
					&& self.peer_supports_ack_frequency();
		}

		// Reserving capacity can provide more capacity than we asked for. However, we are not
		// allowed to write more than `segment_size`. Therefore the maximum capacity is tracked
		// separately.
		let mut buf_capacity = 0;

		let mut coalesce = true;
		let mut builder_storage: Option<PacketBuilder> = None;
		let mut sent_frames = None;
		let mut pad_datagram = false;
		let mut pad_datagram_to_mtu = false;
		// Set when either the congestion window or the pacer held a send back; drives
		// `app_limited`, since neither case means the application ran dry.
		let mut send_blocked = false;
		// Set only when the congestion window itself was full. This is the spec's
		// `C.is_cwnd_limited`, which a pacing delay must not stand in for.
		let mut cwnd_blocked = false;
		// Set when the send rate cap held a send back: a local policy, not the path, so the
		// controller hears it as application-limited.
		let mut rate_blocked = false;

		// Iterate over all spaces and find data to send
		let mut space_idx = 0;
		let spaces = [SpaceId::Initial, SpaceId::Handshake, SpaceId::Data];
		// This loop will potentially spend multiple iterations in the same `SpaceId`,
		// so we cannot trivially rewrite it to take advantage of `SpaceId::iter()`.
		while space_idx < spaces.len() {
			let space_id = spaces[space_idx];
			// Number of bytes available for frames if this is a 1-RTT packet. We're guaranteed to
			// be able to send an individual frame at least this large in the next 1-RTT
			// packet. This could be generalized to support every space, but it's only needed to
			// handle large fixed-size frames, which only exist in 1-RTT (application datagrams). We
			// don't account for coalesced packets potentially occupying space because frames can
			// always spill into the next datagram.
			let pn = self.packet_number_filter.peek(&self.spaces[SpaceId::Data]);
			let frame_space_1rtt = segment_size.saturating_sub(self.predict_1rtt_overhead(Some(pn)));

			// Is there data or a close message to send in this space?
			let can_send = self.space_can_send(space_id, frame_space_1rtt);
			// The packet still being built isn't in flight yet; assume it gets padded to fill
			// the segment
			let untracked_bytes = if let Some(builder) = &builder_storage {
				buf_capacity - builder.partial_encode.start
			} else {
				0
			} as u64;
			// With nothing else to send, resend in-flight data while the controller probes
			let bandwidth_probe =
				space_id == SpaceId::Data
					&& can_send.is_empty()
					&& !close && match self.bandwidth_probe_ready(now, segment_size as u64 + untracked_bytes) {
					bandwidth_probe::Ready::Idle => false,
					bandwidth_probe::Ready::Blocked(until) => {
						if let Some(until) = until {
							self.timers.set(Timer::Pacing, until);
						}
						send_blocked = true;
						false
					}
					bandwidth_probe::Ready::Send => true,
				};
			if (can_send.is_empty() && !bandwidth_probe && (!close || self.spaces[space_id].crypto.is_none()))
				|| (finish_first && space_id == SpaceId::Initial)
			{
				space_idx += 1;
				continue;
			}

			let mut ack_eliciting = !self.spaces[space_id].pending.is_empty(&self.streams)
				|| self.spaces[space_id].ping_pending
				|| self.spaces[space_id].immediate_ack_pending;
			if space_id == SpaceId::Data {
				ack_eliciting |= self.can_send_1rtt(frame_space_1rtt) || bandwidth_probe;
			}

			pad_datagram_to_mtu |= space_id == SpaceId::Data && self.config.pad_to_mtu;

			// Can we append more data into the current buffer?
			// It is not safe to assume that `buf.len()` is the end of the data,
			// since the last packet might not have been finished.
			let buf_end = if let Some(builder) = &builder_storage {
				buf.len().max(builder.min_size) + builder.tag_len
			} else {
				buf.len()
			};

			let tag_len = self.spaces[space_id].crypto.as_ref().unwrap().packet.local.tag_len();
			if !coalesce || buf_capacity - buf_end < MIN_PACKET_SPACE + tag_len {
				// We need to send 1 more datagram and extend the buffer for that.

				// Is 1 more datagram allowed?
				if num_datagrams >= max_datagrams {
					// No more datagrams allowed
					break;
				}

				// Anti-amplification is only based on `total_sent`, which gets
				// updated at the end of this method. Therefore we pass the amount
				// of bytes for datagrams that are already created, as well as 1 byte
				// for starting another datagram. If there is any anti-amplification
				// budget left, we always allow a full MTU to be sent
				// (see https://github.com/quinn-rs/quinn/issues/1082)
				if self
					.path
					.anti_amplification_blocked(segment_size as u64 * (num_datagrams as u64) + 1)
				{
					trace!("blocked by anti-amplification");
					break;
				}

				// Congestion control and pacing checks
				// Tail loss probes must not be blocked by congestion, or a deadlock could arise.
				// Close packets contain only ACKs and CONNECTION_CLOSE, neither of which is
				// congestion controlled, and must not be blocked either: `ack_eliciting` reflects
				// pending frames that will never be sent once closing, and a closed connection no
				// longer processes ACKs, so the window could never drain
				// (see https://github.com/quinn-rs/quinn/issues/2785)
				if ack_eliciting && self.spaces[space_id].loss_probes == 0 && !close {
					debug_assert!(untracked_bytes <= segment_size as u64);

					let bytes_to_send = segment_size as u64 + untracked_bytes;
					if self.path.in_flight.bytes + bytes_to_send > self.path.congestion.window() {
						space_idx += 1;
						send_blocked = true;
						cwnd_blocked = true;
						// We continue instead of breaking here in order to avoid
						// blocking loss probes queued for higher spaces.
						trace!("blocked by congestion control");
						continue;
					}

					// Check whether the next datagram is blocked by pacing
					if let Some(delay) = self.path.congestion.pace(now, bytes_to_send) {
						self.timers.set(Timer::Pacing, delay);
						send_blocked = true;
						// Loss probes should be subject to pacing, even though
						// they are not congestion controlled.
						trace!("blocked by pacing");
						break;
					}
				}

				// Loss probes are held to the send rate cap too: it refills with time, unlike the
				// congestion window, so a probe only waits, and a peer faking loss cannot draw
				// probes past the cap.
				if ack_eliciting
					&& !close && let Some(delay) = self
					.send_rate
					.as_mut()
					.and_then(|cap| cap.delay(now, segment_size as u64 + untracked_bytes))
				{
					self.timers.set(Timer::Pacing, delay);
					rate_blocked = true;
					trace!("blocked by the send rate cap");
					break;
				}

				// Finish current packet
				if let Some(mut builder) = builder_storage.take() {
					if pad_datagram {
						builder.pad_to(min_mtu);
					}

					if num_datagrams > 1 || pad_datagram_to_mtu {
						// If too many padding bytes would be required to continue the GSO batch
						// after this packet, end the GSO batch here. Ensures that fixed-size frames
						// with heterogeneous sizes (e.g. application datagrams) won't inadvertently
						// waste large amounts of bandwidth. The exact threshold is a bit arbitrary
						// and might benefit from further tuning, though there's no universally
						// optimal value.
						//
						// Additionally, if this datagram is a loss probe and `segment_size` is
						// larger than `min_mtu`, then padding it to `segment_size` to continue
						// the GSO batch would risk failure to recover from a reduction in path
						// MTU. Loss probes are the only packets for which we might grow
						// `buf_capacity` by less than `segment_size`.
						const MAX_PADDING: usize = 16;
						let packet_len_unpadded =
							cmp::max(builder.min_size, buf.len()) - datagram_start + builder.tag_len;
						if (packet_len_unpadded + MAX_PADDING < segment_size && !pad_datagram_to_mtu)
							|| datagram_start + segment_size > buf_capacity
						{
							trace!(
								"GSO truncated by demand for {} padding bytes or loss probe",
								segment_size - packet_len_unpadded
							);
							builder_storage = Some(builder);
							break;
						}

						// Pad the current datagram to GSO segment size so it can be included in the
						// GSO batch.
						builder.pad_to(segment_size as u16);
					}

					builder.finish_and_track(now, self, sent_frames.take(), buf);

					if num_datagrams == 1 {
						// Set the segment size for this GSO batch to the size of the first UDP
						// datagram in the batch. Larger data that cannot be fragmented
						// (e.g. application datagrams) will be included in a future batch. When
						// sending large enough volumes of data for GSO to be useful, we expect
						// packet sizes to usually be consistent, e.g. populated by max-size STREAM
						// frames or uniformly sized datagrams.
						segment_size = buf.len();
						// Clip the unused capacity out of the buffer so future packets don't
						// overrun
						buf_capacity = buf.len();

						// Every later segment is capped at the first one's size. If that is
						// too small to be worth sending, end the batch so the rest goes out
						// at full size.
						if segment_size < MIN_PACKET_SPACE {
							break;
						}

						// Check whether the data we planned to send will fit in the reduced segment
						// size. If not, bail out and leave it for the next GSO batch so we don't
						// end up trying to send an empty packet. We can't easily compute the right
						// segment size before the original call to `space_can_send`, because at
						// that time we haven't determined whether we're going to coalesce with the
						// first datagram or potentially pad it to `min_mtu`.
						// A probe's duplicates split to fit any segment.
						if space_id == SpaceId::Data && !bandwidth_probe {
							let frame_space_1rtt = segment_size.saturating_sub(self.predict_1rtt_overhead(Some(pn)));
							if self.space_can_send(space_id, frame_space_1rtt).is_empty() {
								break;
							}
						}
					}
				}

				// Allocate space for another datagram
				let next_datagram_size_limit = match self.spaces[space_id].loss_probes {
					0 => segment_size,
					_ => {
						self.spaces[space_id].loss_probes -= 1;
						// Clamp the datagram to at most the minimum MTU to ensure that loss probes
						// can get through and enable recovery even if the path MTU has shrank
						// unexpectedly.
						cmp::min(segment_size, usize::from(min_mtu))
					}
				};
				buf_capacity += next_datagram_size_limit;
				if buf.capacity() < buf_capacity {
					// We reserve the maximum space for sending `max_datagrams` upfront
					// to avoid any reallocations if more datagrams have to be appended later on.
					// Benchmarks have shown shown a 5-10% throughput improvement
					// compared to continuously resizing the datagram buffer.
					// While this will lead to over-allocation for small transmits
					// (e.g. purely containing ACKs), modern memory allocators
					// (e.g. mimalloc and jemalloc) will pool certain allocation sizes
					// and therefore this is still rather efficient.
					buf.reserve(max_datagrams * segment_size);
				}
				num_datagrams += 1;
				coalesce = true;
				pad_datagram = false;
				datagram_start = buf.len();

				debug_assert_eq!(
					datagram_start % segment_size,
					0,
					"datagrams in a GSO batch must be aligned to the segment size"
				);
			} else {
				// We can append/coalesce the next packet into the current
				// datagram.
				// Finish current packet without adding extra padding
				if let Some(builder) = builder_storage.take() {
					builder.finish_and_track(now, self, sent_frames.take(), buf);
				}
			}

			debug_assert!(buf_capacity - buf.len() >= MIN_PACKET_SPACE);

			//
			// From here on, we've determined that a packet will definitely be sent.
			//

			if self.spaces[SpaceId::Initial].crypto.is_some() && space_id == SpaceId::Handshake && self.side.is_client()
			{
				// A client stops both sending and processing Initial packets when it
				// sends its first Handshake packet.
				self.discard_space(now, SpaceId::Initial);
			}
			if let Some(prev) = &mut self.prev_crypto {
				prev.update_unacked = false;
			}

			debug_assert!(
				builder_storage.is_none() && sent_frames.is_none(),
				"Previous packet must have been finished"
			);

			let builder = builder_storage.insert(PacketBuilder::new(
				now,
				space_id,
				self.rem_cids.active(),
				buf,
				buf_capacity,
				datagram_start,
				ack_eliciting,
				self,
			)?);
			coalesce = coalesce && !builder.short_header;

			// https://tools.ietf.org/html/draft-ietf-quic-transport-34#section-14.1
			pad_datagram |= space_id == SpaceId::Initial && (self.side.is_client() || ack_eliciting);

			if close {
				trace!("sending CONNECTION_CLOSE");
				// Encode ACKs before the ConnectionClose message, to give the receiver
				// a better approximate on what data has been processed. This is
				// especially important with ack delay, since the peer might not
				// have gotten any other ACK for the data earlier on.
				if !self.spaces[space_id].pending_acks.ranges().is_empty() {
					// Reserve room for CONNECTION_CLOSE even when an Initial token leaves
					// little space after the header.
					Self::try_populate_acks(
						now,
						self.receiving_ecn,
						&mut SentFrames::default(),
						&mut self.spaces[space_id],
						buf,
						&mut self.stats,
						builder.max_size - frame::ConnectionClose::SIZE_BOUND,
					);
				}

				// All or nothing: part of the flight would not complete the handshake. A flight
				// too big for one packet (a large client certificate chain) keeps the plain close.
				// It must also leave room to coalesce the 1-RTT close, or a one-datagram budget
				// resends this packet on every poll and never reaches the close. Both spaces share
				// the negotiated suite, so the 1-RTT tag is as long as this one.
				// Nothing is dequeued, so a close resent for a lost flight carries it again.
				let finished = &self.spaces[space_id];
				let carry_finished = finish_first
					&& space_id == SpaceId::Handshake
					&& buf.len()
						+ finished
							.unacked_crypto()
							.map(|frame| frame::Crypto::SIZE_BOUND + frame.data.len())
							.sum::<usize>() + MIN_PACKET_SPACE
						+ builder.tag_len <= builder.max_size;
				if carry_finished {
					for frame in finished.unacked_crypto() {
						trace!("CRYPTO: off {} len {}", frame.offset, frame.data.len());
						frame.encode(buf);
						self.stats.frame_tx.crypto += 1;
					}
				} else if buf.len() + frame::ConnectionClose::SIZE_BOUND <= builder.max_size {
					let max_frame_size = builder.max_size - buf.len();
					match &self.state {
						State::Closed(state::Closed { reason }) => {
							if space_id == SpaceId::Data || reason.is_transport_layer() {
								reason.encode(buf, max_frame_size)
							} else {
								frame::ConnectionClose {
									error_code: TransportErrorCode::APPLICATION_ERROR,
									frame_type: None,
									reason: String::new(),
								}
								.encode(buf, max_frame_size)
							}
						}
						State::Draining => frame::ConnectionClose {
							error_code: TransportErrorCode::NO_ERROR,
							frame_type: None,
							reason: String::new(),
						}
						.encode(buf, max_frame_size),
						_ => unreachable!("tried to make a close packet when the connection wasn't closed"),
					}
				}
				if space_id == self.highest_space {
					// Don't send another close packet
					self.close = false;
					// `CONNECTION_CLOSE` is the final packet
					break;
				} else {
					// Send a close frame in every possible space for robustness, per RFC9000
					// "Immediate Close during the Handshake". Don't bother trying to send anything
					// else.
					space_idx += 1;
					continue;
				}
			}

			// Send an off-path PATH_RESPONSE. Prioritized over on-path data to ensure that path
			// validation can occur while the link is saturated.
			if space_id == SpaceId::Data
				&& num_datagrams == 1
				&& let Some((token, remote)) = self.path_responses.pop_off_path(self.path.remote)
			{
				// `unwrap` guaranteed to succeed because `builder_storage` was populated just
				// above.
				let mut builder = builder_storage.take().unwrap();
				trace!("PATH_RESPONSE {:08x} (off-path)", token);
				buf.write(frame::FrameType::PATH_RESPONSE);
				buf.write(token);
				self.stats.frame_tx.path_response += 1;
				builder.pad_to(MIN_INITIAL_SIZE);
				builder.finish_and_track(
					now,
					self,
					Some(SentFrames {
						non_retransmits: true,
						..SentFrames::default()
					}),
					buf,
				);
				self.stats.udp_tx.on_sent(1, buf.len());
				return Some(Transmit {
					remote,
					size: buf.len(),
					ecn: None,
					segment_size: None,
					local_ip: self.local_ip,
				});
			}

			let sent = self.populate_packet(
				now,
				space_id,
				buf,
				builder.max_size,
				builder.exact_number,
				bandwidth_probe,
			);

			// ACK-only packets should only be sent when explicitly allowed. If we write them due to
			// any other reason, there is a bug which leads to one component announcing write
			// readiness while not writing any data. This degrades performance. The condition is
			// only checked if the full MTU is available and when potentially large fixed-size
			// frames aren't queued, so that lack of space in the datagram isn't the reason for just
			// writing ACKs.
			debug_assert!(
				!(sent.is_ack_only(&self.streams)
					&& !can_send.acks
					&& can_send.other
					&& (buf_capacity - builder.datagram_start) == self.path.current_mtu() as usize
					&& self.streams.datagram_bytes() == 0),
				"SendableFrames was {can_send:?}, but only ACKs have been written"
			);
			pad_datagram |= sent.requires_padding;

			if sent.largest_acked.is_some() {
				self.spaces[space_id].pending_acks.acks_sent();
				self.timers.stop(Timer::MaxAckDelay);
				self.next_bundled_ack_time = Some(now + self.next_bundled_ack_delay());
			}

			// Keep information about the packet around until it gets finalized
			sent_frames = Some(sent);

			// Don't increment space_idx.
			// We stay in the current space and check if there is more data to send.
		}

		// Finish the last packet
		if let Some(mut builder) = builder_storage {
			if pad_datagram {
				builder.pad_to(min_mtu);
			}

			// If this datagram is a loss probe and `segment_size` is larger than `min_mtu`,
			// then padding it to `segment_size` would risk failure to recover from a reduction in
			// path MTU.
			// Loss probes are the only packets for which we might grow `buf_capacity`
			// by less than `segment_size`.
			if pad_datagram_to_mtu && buf_capacity >= datagram_start + segment_size {
				builder.pad_to(segment_size as u16);
			}

			builder.finish_and_track(now, self, sent_frames, buf);

			self.qlog
				.emit_recovery_metrics(self.pto_count, &mut self.path, now, self.orig_rem_cid);
		}

		// Exhausting the packet spaces also catches a partial poll that drains the source.
		// A full local send buffer prevents writes from reaching this queue; it is a sender
		// limit, not evidence that the application has no more data.
		self.app_limited =
			rate_blocked || (space_idx == spaces.len() && !send_blocked && !self.streams.send_window_full());
		if self.app_limited {
			self.path.congestion.on_app_limited(self.path.in_flight.bytes);
		}

		if cwnd_blocked {
			self.path.congestion.on_cwnd_limited();
		}

		// Send MTU probe if necessary
		if buf.is_empty() && self.state.is_established() {
			let space_id = SpaceId::Data;
			let probe_size = self
				.path
				.mtud
				.poll_transmit(now, self.packet_number_filter.peek(&self.spaces[space_id]))?;

			let buf_capacity = probe_size as usize;
			buf.reserve(buf_capacity);

			let mut builder =
				PacketBuilder::new(now, space_id, self.rem_cids.active(), buf, buf_capacity, 0, true, self)?;

			// We implement MTU probes as ping packets padded up to the probe size
			buf.write(frame::FrameType::PING);
			self.stats.frame_tx.ping += 1;

			// If supported by the peer, we want no delays to the probe's ACK
			if self.peer_supports_ack_frequency() {
				buf.write(frame::FrameType::IMMEDIATE_ACK);
				self.stats.frame_tx.immediate_ack += 1;
			}

			builder.pad_to(probe_size);
			let sent_frames = SentFrames {
				non_retransmits: true,
				..Default::default()
			};
			builder.finish_and_track(now, self, Some(sent_frames), buf);

			self.stats.path.sent_plpmtud_probes += 1;
			num_datagrams = 1;

			trace!(?probe_size, "writing MTUD probe");
		}

		if buf.is_empty() {
			return None;
		}

		trace!("sending {} bytes in {} datagrams", buf.len(), num_datagrams);
		self.path.total_sent = self.path.total_sent.saturating_add(buf.len() as u64);

		self.stats.udp_tx.on_sent(num_datagrams as u64, buf.len());

		Some(Transmit {
			remote: self.path.remote,
			size: buf.len(),
			ecn: if self.path.sending_ecn {
				Some(EcnCodepoint::Ect0)
			} else {
				None
			},
			segment_size: match num_datagrams {
				1 => None,
				_ => Some(segment_size),
			},
			local_ip: self.local_ip,
		})
	}

	/// Send PATH_CHALLENGE for a previous path if necessary
	fn send_path_challenge(&mut self, now: Instant, buf: &mut Vec<u8>) -> Option<Transmit> {
		let (prev_cid, prev_path) = self.prev_path.as_mut()?;
		if !prev_path.challenge_pending {
			return None;
		}
		prev_path.challenge_pending = false;
		let token = prev_path
			.challenge
			.expect("previous path challenge pending without token");
		let remote = prev_path.remote;
		debug_assert_eq!(
			self.highest_space,
			SpaceId::Data,
			"PATH_CHALLENGE queued without 1-RTT keys"
		);
		buf.reserve(MIN_INITIAL_SIZE as usize);

		let buf_capacity = buf.capacity();

		// Use the previous CID to avoid linking the new path with the previous path. We
		// don't bother accounting for possible retirement of that prev_cid because this is
		// sent once, immediately after migration, when the CID is known to be valid. Even
		// if a post-migration packet caused the CID to be retired, it's fair to pretend
		// this is sent first.
		let mut builder = PacketBuilder::new(now, SpaceId::Data, *prev_cid, buf, buf_capacity, 0, false, self)?;
		trace!("validating previous path with PATH_CHALLENGE {:08x}", token);
		buf.write(frame::FrameType::PATH_CHALLENGE);
		buf.write(token);
		self.stats.frame_tx.path_challenge += 1;

		// An endpoint MUST expand datagrams that contain a PATH_CHALLENGE frame
		// to at least the smallest allowed maximum datagram size of 1200 bytes,
		// unless the anti-amplification limit for the path does not permit
		// sending a datagram of this size
		builder.pad_to(MIN_INITIAL_SIZE);

		builder.finish(self, now, buf);
		self.stats.udp_tx.on_sent(1, buf.len());

		Some(Transmit {
			remote,
			size: buf.len(),
			ecn: None,
			segment_size: None,
			local_ip: self.local_ip,
		})
	}

	/// Indicate what types of frames are ready to send for the given space
	fn space_can_send(&self, space_id: SpaceId, frame_space_1rtt: usize) -> SendableFrames {
		if self.spaces[space_id].crypto.is_none() {
			// No keys available for this space
			return SendableFrames::empty();
		}
		let mut can_send = self.spaces[space_id].can_send(&self.streams);
		if space_id == SpaceId::Data {
			can_send.other |= self.can_send_1rtt(frame_space_1rtt);
		}
		can_send
	}

	/// The delay to wait after sending an ACK before bundling the next one.
	///
	/// This delay prevents waste of peer's resources with processing bundled
	/// ACKs unnecessarily frequently.
	///
	/// If we receive an ack-eliciting packet while this delay is still pending,
	/// `next_bundled_ack_time` is reset to `now`, which means this delay will be ignored.
	/// So this delay only matters when we keep sending but stop receiving ack-eliciting
	/// packets for a while.
	///
	/// This should be at least `RTT + peer's max_ack_delay`: since a bundled ACK frame rides
	/// along with an ack-eliciting frame, the packet carrying it is itself ack-eliciting.
	/// We should give the peer enough time to acknowledge it.
	/// Otherwise, we risk bundling another ACK before the peer has even had a chance
	/// to acknowledge the previous one, which is a waste of remote peer's resources.
	fn next_bundled_ack_delay(&self) -> Duration {
		self.path.rtt.get() + self.ack_frequency.peer_max_ack_delay + TIMER_GRANULARITY
	}

	/// Process `ConnectionEvent`s generated by the associated `Endpoint`
	///
	/// Will execute protocol logic upon receipt of a connection event, in turn preparing signals
	/// (including application `Event`s, `EndpointEvent`s and outgoing datagrams) that should be
	/// extracted through the relevant methods.
	pub fn handle_event(&mut self, event: ConnectionEvent) {
		use ConnectionEventInner::*;
		match event.0 {
			Datagram(DatagramConnectionEvent {
				now,
				remote,
				ecn,
				first_decode,
				remaining,
			}) => {
				// If this packet could initiate a migration and we're a client or a server that
				// forbids migration, drop the datagram. This could be relaxed to heuristically
				// permit NAT-rebinding-like migration.
				if remote != self.path.remote && !self.side.remote_may_migrate() {
					trace!("discarding packet from unrecognized peer {}", remote);
					return;
				}

				let was_anti_amplification_blocked = self.path.anti_amplification_blocked(1);

				self.stats.udp_rx.datagrams += 1;
				self.stats.udp_rx.bytes += first_decode.len() as u64;
				let data_len = first_decode.len();

				self.handle_decode(now, remote, ecn, first_decode);
				// The current `path` might have changed inside `handle_decode`,
				// since the packet could have triggered a migration. Make sure
				// the data received is accounted for the most recent path by accessing
				// `path` after `handle_decode`.
				self.path.total_recvd = self.path.total_recvd.saturating_add(data_len as u64);

				if let Some(data) = remaining {
					self.stats.udp_rx.bytes += data.len() as u64;
					self.handle_coalesced(now, remote, ecn, data);
				}

				self.qlog
					.emit_recovery_metrics(self.pto_count, &mut self.path, now, self.orig_rem_cid);

				if was_anti_amplification_blocked {
					// A prior attempt to set the loss detection timer may have failed due to
					// anti-amplification, so ensure it's set now. Prevents a handshake deadlock if
					// the server's first flight is lost.
					self.set_loss_detection_timer(now);
				}
			}
			NewIdentifiers(ids, now) => {
				self.local_cid_state.new_cids(&ids, now);
				ids.into_iter().rev().for_each(|frame| {
					self.spaces[SpaceId::Data].pending.new_cids.push(frame);
				});
				// Update Timer::PushNewCid
				if self.timers.get(Timer::PushNewCid).is_none_or(|x| x <= now) {
					self.reset_cid_retirement();
				}
			}
		}
	}

	/// Process timer expirations
	///
	/// Executes protocol logic, potentially preparing signals (including application `Event`s,
	/// `EndpointEvent`s and outgoing datagrams) that should be extracted through the relevant
	/// methods.
	///
	/// It is most efficient to call this immediately after the system clock reaches the latest
	/// `Instant` that was output by `poll_timeout`; however spurious extra calls will simply
	/// no-op and therefore are safe.
	pub fn handle_timeout(&mut self, now: Instant) {
		if !self.state.is_closed()
			&& self
				.streams
				.receive_rate_deadline()
				.is_some_and(|deadline| deadline <= now)
			&& self.streams.grant_receive_rate(now).should_transmit()
		{
			self.spaces[SpaceId::Data].pending.max_data = true;
		}
		for &timer in &Timer::VALUES {
			if !self.timers.is_expired(timer, now) {
				continue;
			}
			self.timers.stop(timer);
			trace!(timer = ?timer, "timeout");
			match timer {
				Timer::Close => {
					self.state = State::Drained;
					self.report_draining();
					self.endpoint_events.push_back(EndpointEventInner::Drained);
				}
				Timer::Idle => {
					self.kill(match self.state.is_handshake() {
						true => Error::HandshakeTimeout,
						false => Error::TimedOut,
					});
				}
				Timer::KeepAlive => {
					trace!("sending keep-alive");
					self.ping();
				}
				Timer::LossDetection => {
					self.on_loss_detection_timeout(now);

					self.qlog
						.emit_recovery_metrics(self.pto_count, &mut self.path, now, self.orig_rem_cid);
				}
				Timer::KeyDiscard => {
					self.prev_crypto = None;
				}
				Timer::PathValidation => {
					debug!("path validation failed");
					if let Some((_, prev)) = self.prev_path.take() {
						self.path = prev;
						self.set_loss_detection_timer(now);
						self.events.push_back(Event::PathUpdated);
					}
					self.path.challenge = None;
					self.path.challenge_pending = false;
				}
				Timer::Pacing => trace!("pacing timer expired"),
				Timer::PushNewCid => {
					// Update `retire_prior_to` field in NEW_CONNECTION_ID frame
					let num_new_cid = self.local_cid_state.on_cid_timeout().into();
					if !self.state.is_closed() {
						trace!(
							"push a new cid to peer RETIRE_PRIOR_TO field {}",
							self.local_cid_state.retire_prior_to()
						);
						self.endpoint_events
							.push_back(EndpointEventInner::NeedIdentifiers(now, num_new_cid));
					}
				}
				Timer::MaxAckDelay => {
					trace!("max ack delay reached");
					// This timer is only armed in the Data space
					self.spaces[SpaceId::Data].pending_acks.on_max_ack_delay_timeout()
				}
			}
		}
	}

	/// Close a connection immediately
	///
	/// This does not ensure delivery of outstanding data. It is the application's responsibility to
	/// call this only when all important communications have been completed, e.g. by calling
	/// [`send::Stream::finish`] on outstanding streams and waiting for the corresponding
	/// [`StreamEvent::Finished`] event.
	///
	/// If [`Streams::send_streams`] returns 0, all outstanding stream data has been
	/// delivered. There may still be data from the peer that has not been received.
	///
	/// A connection not already closed reports [`Error::LocallyClosed`] with `error_code` and
	/// `reason` as its [`ConnectionLost`](Event::ConnectionLost).
	///
	/// [`StreamEvent::Finished`]: crate::StreamEvent::Finished
	pub fn close(&mut self, now: Instant, error_code: VarInt, reason: String) {
		if !self.state.is_closed() {
			self.lose(Error::LocallyClosed {
				code: error_code,
				reason: reason.clone(),
			});
		}
		self.close_inner(now, Close::Application(frame::ApplicationClose { error_code, reason }))
	}

	/// Report that a send to the peer found no route, such as `ENETUNREACH`
	///
	/// A handshake can never finish, so it fails at once with `NO_VIABLE_PATH`, sending no
	/// CONNECTION_CLOSE the peer could not receive anyway. An established connection is left
	/// alone, since a route that drops for a moment (a client switching networks) comes back, so
	/// the datagram is just lost.
	pub fn handle_unreachable(&mut self) {
		if self.state.is_handshake() {
			self.kill(TransportError::NO_VIABLE_PATH("no route to the peer").into());
		}
	}

	fn close_inner(&mut self, now: Instant, reason: Close) {
		let was_closed = self.state.is_closed();
		if !was_closed {
			// A peer that never answered the handshake will not answer the close either, so
			// only the close timer would end the connection's activity. The closing period
			// still answers a late reply; it just no longer holds up a shutdown.
			let unanswered = matches!(&self.state, State::Handshake(state) if !state.rem_cid_set);
			self.close_common();
			self.set_close_timer(now);
			self.close = true;
			self.state = State::Closed(state::Closed { reason });
			if unanswered {
				self.report_draining();
			}
		}
	}

	/// Control datagrams
	pub fn datagrams(&mut self) -> Datagrams<'_> {
		Datagrams { conn: self }
	}

	/// Returns connection statistics
	pub fn stats(&self) -> Stats {
		let mut stats = self.stats;
		stats.path.rtt = self.path.rtt.get();
		stats.path.min_rtt = self.path.rtt.min();
		stats.path.cwnd = self.path.congestion.window();
		stats.path.bandwidth_estimate = self.path.congestion.metrics().bandwidth_estimate;
		stats.path.current_mtu = self.path.mtud.current_mtu();
		stats.credit = self.streams.credit_stats();

		stats
	}

	/// The streams with a waiter entry, to prove the map stays bounded
	#[cfg(test)]
	pub(crate) fn waiting_streams(&self) -> usize {
		self.streams.waits.stream_entries()
	}

	/// Assert that the receive lease books balance, for the fuzz target.
	#[cfg(any(test, feature = "fuzz"))]
	pub(crate) fn check_leases(&self) {
		self.streams.check_leases();
	}

	/// Ping the remote endpoint
	///
	/// Causes an ACK-eliciting packet to be transmitted.
	pub(crate) fn ping(&mut self) {
		self.spaces[self.highest_space].ping_pending = true;
	}

	/// Update traffic keys spontaneously
	///
	/// This can be useful for testing key updates, as they otherwise only happen infrequently.
	pub(crate) fn force_key_update(&mut self) {
		if !self.state.is_established() {
			debug!("ignoring forced key update in illegal state");
			return;
		}
		if self.prev_crypto.is_some() {
			// We already just updated, or are currently updating, the keys. Concurrent key updates
			// are illegal.
			debug!("ignoring redundant forced key update");
			return;
		}
		self.update_keys(None, false);
	}

	/// Get a session reference
	pub fn crypto_session(&self) -> &dyn crypto::Session {
		&*self.crypto
	}

	/// Whether the connection is in the process of being established
	///
	/// If this returns `false`, the connection may be either established or closed, signaled by the
	/// emission of a [`Connected`](Event::Connected) or [`ConnectionLost`](Event::ConnectionLost)
	/// event respectively.
	pub fn is_handshaking(&self) -> bool {
		self.state.is_handshake()
	}

	/// Whether the connection is closed
	///
	/// Closed connections cannot transport any further data. A connection becomes closed when
	/// either peer application intentionally closes it, or when either transport layer detects an
	/// error such as a time-out or certificate validation failure.
	///
	/// A [`ConnectionLost`](Event::ConnectionLost) event is emitted with details however the
	/// connection closes, with [`Error::LocallyClosed`] when the local application closed it via
	/// [`close()`](Self::close).
	pub fn is_closed(&self) -> bool {
		self.state.is_closed()
	}

	/// Whether there is no longer any need to keep the connection around
	///
	/// Closed connections become drained after a brief timeout to absorb any remaining in-flight
	/// packets from the peer. All drained connections have been closed.
	pub fn is_drained(&self) -> bool {
		self.state.is_drained()
	}

	/// Look up whether we're the client or server of this Connection
	pub fn side(&self) -> Side {
		self.side.side()
	}

	/// The latest socket address for this connection's peer
	pub fn remote_address(&self) -> SocketAddr {
		self.path.remote
	}

	/// Current best estimate of this connection's latency (round-trip-time)
	pub fn rtt(&self) -> Duration {
		self.path.rtt.get()
	}

	/// Current state of this connection's congestion controller, for debugging purposes
	pub fn congestion_state(&self) -> &dyn Controller {
		self.path.congestion.as_ref()
	}

	/// Probe the path's bandwidth up to `bytes_per_second` while the application sends less
	///
	/// A congestion controller stops raising its estimate once the application sends less than
	/// the path carries. While the controller probes for bandwidth below this target, the sender
	/// fills the inflight it allows
	/// ([`probe_inflight`](crate::congestion::ControllerMetrics::probe_inflight)) with duplicates
	/// of STREAM data still in flight, sending no more than the target in all, so the probe
	/// measures the path. Duplicates count against congestion control, as
	/// [`PathStats::sent_bandwidth_probe_bytes`]. A reset stream's data is never sent again. An
	/// attempt that misses the target sits out a few seconds before retrying it, and a changed
	/// target is retried at once. A controller that never probes ([`Controller::probes`])
	/// ignores the target, and zero, the default, never probes.
	pub fn set_probe_target(&mut self, bytes_per_second: u64) {
		self.bandwidth_probe.set_target(bytes_per_second);
	}

	/// Resets path-specific settings.
	///
	/// This will force-reset several subsystems related to a specific network path.
	/// Currently this is the congestion controller, round-trip estimator, pacer, and MTU
	/// discovery.
	///
	/// This is useful when it is known the underlying network path has changed and the old
	/// state of these subsystems is no longer valid or optimal. In this case it might be
	/// faster or reduce loss to settle on optimal values by restarting from the initial
	/// configuration in the [`transport::Config`].
	#[cfg(test)]
	pub(crate) fn path_changed(&mut self, now: Instant) {
		self.path.reset(now, &self.config);
		// The rebuilt controller never sees state the old one gave packets still in flight.
		for space in SpaceId::iter() {
			for info in self.spaces[space].sent_packets.values_mut() {
				info.congestion = None;
			}
		}
		self.datagrams().drop_oversized();
	}

	/// Modify the number of remotely initiated streams that may be concurrently open
	///
	/// No streams may be opened by the peer unless fewer than `count` are already open. Large
	/// `count`s increase both minimum and worst-case memory consumption.
	pub(crate) fn set_max_concurrent_streams(&mut self, dir: Dir, count: VarInt) {
		self.streams.set_max_concurrent(dir, count);
		// If the limit was reduced, then a flow control update previously deemed insignificant may
		// now be significant.
		let pending = &mut self.spaces[SpaceId::Data].pending;
		self.streams.queue_max_stream_id(pending);
	}

	/// Current number of remotely initiated streams that may be concurrently open
	///
	/// If the target for this limit is reduced using [`set_max_concurrent_streams`](Self::set_max_concurrent_streams),
	/// it will not change immediately, even if fewer streams are open. Instead, it will
	/// decrement by one for each time a remotely initiated stream of matching directionality is closed.
	#[cfg(test)]
	pub(crate) fn max_concurrent_streams(&self, dir: Dir) -> u64 {
		self.streams.max_concurrent(dir)
	}

	/// See [`Limits::send_window`]
	pub(crate) fn set_send_window(&mut self, send_window: u64) {
		self.streams.set_send_window(send_window);
	}

	/// See [`Limits::receive_window`]
	pub(crate) fn set_receive_window(&mut self, receive_window: VarInt) {
		if self.streams.set_receive_window(receive_window) {
			self.spaces[SpaceId::Data].pending.max_data = true;
		}
	}

	/// Change every limit `limits` sets, leaving the others as they are
	///
	/// Errors, changing nothing, on limits [`Limits::validate`] refuses.
	pub fn set_limits(&mut self, limits: Limits) -> Result<(), LimitsError> {
		limits.validate()?;
		// The cap first, so a window opened with it is paced rather than granted at once.
		if let Some(rate) = limits.receive_rate
			&& self.streams.set_receive_rate(rate).should_transmit()
		{
			self.spaces[SpaceId::Data].pending.max_data = true;
		}
		if let Some(window) = limits.receive_window {
			self.set_receive_window(VarInt::from_u64(window.get()).expect("validated"));
		}
		if let Some(window) = limits.stream_receive_window {
			self.streams.set_stream_receive_window(window.get());
		}
		if let Some(rate) = limits.send_rate {
			match (rate, &mut self.send_rate) {
				(Rate::Unlimited, bucket) => *bucket = None,
				(Rate::BytesPerSecond(rate), Some(bucket)) => bucket.set_rate(rate),
				(Rate::BytesPerSecond(rate), bucket @ None) => *bucket = Some(rate::Bucket::new(rate)),
			}
		}
		if let Some(window) = limits.send_window {
			self.set_send_window(window.get());
		}
		if let Some(streams) = limits.max_streams {
			let streams = VarInt::from_u64(streams.get()).expect("validated");
			self.set_max_concurrent_streams(Dir::Bi, streams);
			self.set_max_concurrent_streams(Dir::Uni, streams);
		}
		Ok(())
	}

	fn on_ack_received(&mut self, now: Instant, space: SpaceId, ack: frame::Ack) -> Result<(), TransportError> {
		if ack.largest >= self.spaces[space].next_packet_number {
			return Err(TransportError::PROTOCOL_VIOLATION("unsent packet acked"));
		}
		// Whether the largest acknowledged packet is new, and whether we still know when it was
		// sent. A forgotten ACK-only packet, or one declared lost, has no send time, so it yields
		// no RTT sample.
		let (new_largest, largest_tracked) = {
			let space = &mut self.spaces[space];
			if space.largest_acked_packet.is_none_or(|pn| ack.largest > pn) {
				space.largest_acked_packet = Some(ack.largest);
				let info = space.sent_packets.get(ack.largest);
				if let Some(info) = info {
					space.largest_acked_packet_sent = info.time_sent;
				}
				(true, info.is_some())
			} else {
				(false, false)
			}
		};

		if self.detect_spurious_loss(&ack, space) {
			self.stats.path.spurious_congestion_events += 1;
			self.path.congestion.on_spurious_loss();
		}

		// Avoid DoS from unreasonably huge ack ranges by filtering out just the new acks.
		let mut newly_acked = ArrayRangeSet::new();
		for range in ack.iter() {
			self.packet_number_filter.check_ack(space, range.clone())?;
			for (pn, _) in self.spaces[space].sent_packets.range(range) {
				newly_acked.insert_one(pn);
			}
		}

		if newly_acked.is_empty() {
			return Ok(());
		}

		let mut ack_eliciting_acked = false;
		let mut acked = mem::take(&mut self.acked);
		acked.clear();
		for packet in newly_acked.elts() {
			if let Some(info) = self.spaces[space].sent_packets.remove(packet) {
				if let Some(acked) = info.largest_acked {
					// Assume ACKs for all packets below the largest acknowledged in `packet` have
					// been received. This can cause the peer to spuriously retransmit if some of
					// our earlier ACKs were lost, but allows for simpler state tracking. See
					// discussion at
					// https://www.rfc-editor.org/rfc/rfc9000.html#name-limiting-ranges-by-tracking
					self.spaces[space].pending_acks.subtract_below(acked);
				}
				ack_eliciting_acked |= info.ack_eliciting;

				// Notify MTU discovery that a packet was acked, because it might be an MTU probe
				let mtu_updated = self.path.mtud.on_acked(space, packet, info.size);
				if mtu_updated {
					self.path.congestion.on_mtu_update(self.path.mtud.current_mtu());
				}

				// Notify ack frequency that a packet was acked, because it might contain an ACK_FREQUENCY frame
				self.ack_frequency.on_acked(packet);

				acked.extend(self.on_packet_acked(info));
			}
		}

		if largest_tracked && ack_eliciting_acked {
			let ack_delay = if space != SpaceId::Data {
				Duration::from_micros(0)
			} else {
				cmp::min(
					self.ack_frequency.peer_max_ack_delay,
					Duration::from_micros(ack.delay << self.peer_params.ack_delay_exponent.0),
				)
			};
			let rtt = now.saturating_duration_since(self.spaces[space].largest_acked_packet_sent);
			self.path.rtt.update(ack_delay, rtt);
			if self.path.first_packet_after_rtt_sample.is_none() {
				self.path.first_packet_after_rtt_sample = Some((space, self.spaces[space].next_packet_number));
			}
		}

		// After the RTT update, as RFC 9002's OnAckReceived orders it.
		self.path.congestion.on_ack(congestion::Ack {
			now,
			packets: &acked,
			in_flight: self.path.in_flight.bytes,
			app_limited: self.app_limited,
			rtt: self.path.rtt.get(),
		});
		self.acked = acked;

		// Must be called before crypto/pto_count are clobbered
		self.detect_lost_packets(now, space, true);

		if self.peer_completed_address_validation() {
			self.pto_count = 0;
		}

		// Explicit congestion notification
		if self.path.sending_ecn {
			if let Some(ecn) = ack.ecn {
				// We only examine ECN counters from ACKs that we are certain we received in transmit
				// order, allowing us to compute an increase in ECN counts to compare against the number
				// of newly acked packets that remains well-defined in the presence of arbitrary packet
				// reordering.
				if new_largest {
					let sent = self.spaces[space].largest_acked_packet_sent;
					self.process_ecn(now, space, newly_acked.len() as u64, ecn, sent);
				}
			} else {
				// We always start out sending ECN, so any ack that doesn't acknowledge it disables it.
				debug!("ECN not acknowledged by peer");
				self.path.sending_ecn = false;
			}
		}

		self.set_loss_detection_timer(now);
		Ok(())
	}

	fn detect_spurious_loss(&mut self, ack: &frame::Ack, space: SpaceId) -> bool {
		let lost_packets = &mut self.spaces[space].lost_packets;

		if lost_packets.is_empty() {
			return false;
		}

		for range in ack.iter() {
			let spurious_losses: Vec<u64> = lost_packets.range(range.clone()).map(|(pn, _info)| pn).collect();

			for pn in spurious_losses {
				lost_packets.remove(pn);
			}
		}

		// If this ACK frame acknowledged all deemed lost packets,
		// then we have raised a spurious congestion event in the past.
		// We cannot conclude when there are remaining packets,
		// but future ACK frames might indicate a spurious loss detection.
		lost_packets.is_empty()
	}

	/// Drain lost packets that we reasonably think will never arrive
	///
	/// The current criterion is copied from `msquic`:
	/// discard packets that were sent earlier than 2 probe timeouts ago.
	fn drain_lost_packets(&mut self, now: Instant, space: SpaceId) {
		let two_pto = 2 * self.path.rtt.pto_base();

		// Packets are sent, and so declared lost, in send-time order.
		let lost_packets = &mut self.spaces[space].lost_packets;
		while let Some((_, info)) = lost_packets.first()
			&& now.saturating_duration_since(info.time_sent) > two_pto
		{
			lost_packets.pop_first();
		}
	}

	/// Process a new ECN block from an in-order ACK
	fn process_ecn(
		&mut self,
		now: Instant,
		space: SpaceId,
		newly_acked: u64,
		ecn: frame::EcnCounts,
		largest_sent_time: Instant,
	) {
		match self.spaces[space].detect_ecn(newly_acked, ecn) {
			Err(e) => {
				debug!("halting ECN due to verification failure: {}", e);
				self.path.sending_ecn = false;
				// Wipe out the existing value because it might be garbage and could interfere with
				// future attempts to use ECN on new paths.
				self.spaces[space].ecn_feedback = frame::EcnCounts::ZERO;
			}
			Ok(false) => {}
			Ok(true) => {
				self.stats.path.congestion_events += 1;
				self.path.congestion.on_ce(congestion::Ce {
					now,
					sent: largest_sent_time,
				});
			}
		}
	}

	/// Account for `info` being acknowledged, returning what the congestion controller should hear
	/// about it.
	///
	/// Not timing-aware, so it's safe to call this for inferred acks, such as arise from
	/// high-latency handshakes.
	fn on_packet_acked(&mut self, info: SentPacket) -> Option<congestion::Acked> {
		self.remove_in_flight(&info);
		// Only pass ACKs to the congestion controller if we are not validating the current
		// path, so as to ignore any ACKs from older paths still coming in. A packet from another
		// path carries another controller's state.
		let acked = info
			.congestion
			.filter(|_| self.path.challenge.is_none() && info.path_generation == self.path.generation())
			.map(|packet| congestion::Acked {
				sent: info.time_sent,
				bytes: info.size,
				packet,
			});

		// Update state for confirmed delivery of frames
		if let Some(retransmits) = info.retransmits.get() {
			for &(id, reliable_size) in retransmits.reset_stream.iter() {
				self.streams.reset_acked(id, reliable_size);
			}
		}

		for frame in info.stream_frames {
			let id = frame.id;
			// A reset held for a peer without RESET_STREAM_AT goes out once it holds the data
			if let Some(reliable_size) = self.streams.received_ack_of(frame) {
				self.spaces[SpaceId::Data]
					.pending
					.reset_stream
					.push((id, reliable_size));
			}
		}
		acked
	}

	fn set_key_discard_timer(&mut self, space: SpaceId) {
		let start = self
			.prev_crypto
			.as_ref()
			.expect("no previous keys")
			.end_packet
			.as_ref()
			.expect("update not acknowledged yet")
			.1;
		self.timers.set(Timer::KeyDiscard, start + self.pto(space) * 3);
	}

	fn on_loss_detection_timeout(&mut self, now: Instant) {
		if let Some((_, pn_space)) = self.loss_time_and_space() {
			// Time threshold loss Detection
			self.detect_lost_packets(now, pn_space, false);
			self.set_loss_detection_timer(now);
			return;
		}

		let Some((_, space)) = self.pto_time_and_space(now) else {
			error!("PTO expired while unset");
			return;
		};
		trace!(
			in_flight = self.path.in_flight.bytes,
			count = self.pto_count,
			?space,
			"PTO fired"
		);

		let count = match self.path.in_flight.ack_eliciting {
			// A PTO when we're not expecting any ACKs must be due to handshake anti-amplification
			// deadlock preventions
			0 => {
				debug_assert!(!self.peer_completed_address_validation());
				1
			}
			// Conventional loss probe
			_ => 2,
		};
		self.spaces[space].loss_probes = self.spaces[space].loss_probes.saturating_add(count);
		self.probe_when_heard = space == SpaceId::Data
			&& self.spaces[space].time_of_last_ack_eliciting_packet.is_some()
			&& !self.heard_since_sent;
		self.pto_count = self.pto_count.saturating_add(1);
		self.set_loss_detection_timer(now);
	}

	/// Send the next probe now if the peer, silent through the last probe timeout, is heard
	/// again ([`Self::probe_when_heard`]). The timeout doubles with each probe (RFC 9002 §6.2.1),
	/// up to 2^[`MAX_BACKOFF_EXPONENT`] times its base, so after an outage the backed-off timer
	/// can leave a path that is back idle for seconds.
	/// A packet that acknowledged anything new has already reset the backoff, and one that
	/// migrated the connection left a path with nothing in flight to probe for.
	fn probe_on_heard(&mut self, now: Instant) {
		if mem::take(&mut self.probe_when_heard)
			&& self.pto_count > 0
			&& self.path.in_flight.ack_eliciting > 0
			&& !self.state.is_closed()
		{
			self.timers.set(Timer::LossDetection, now);
		}
	}

	fn detect_lost_packets(&mut self, now: Instant, pn_space: SpaceId, due_to_ack: bool) {
		let mut lost_packets = Vec::<u64>::new();
		let mut lost_mtu_probe = None;
		let in_flight_mtu_probe = self.path.mtud.in_flight_mtu_probe();
		let rtt = self.path.rtt.conservative();
		let loss_delay = cmp::max(rtt.mul_f32(self.config.time_threshold), TIMER_GRANULARITY);

		let largest_acked_packet = self.spaces[pn_space].largest_acked_packet.unwrap();
		let packet_threshold = self.config.packet_threshold as u64;
		let mut size_of_lost_packets = 0u64;

		// InPersistentCongestion: Determine if all packets in the time period before the newest
		// lost packet, including the edges, are marked lost. PTO computation must always
		// include max ACK delay, i.e. operate as if in Data space (see RFC9001 §7.6.1).
		let congestion_period = self
			.pto(SpaceId::Data)
			.saturating_mul(self.config.persistent_congestion_threshold);
		let mut persistent_congestion_start: Option<Instant> = None;
		let mut prev_packet = None;
		let mut in_persistent_congestion = false;

		let space = &mut self.spaces[pn_space];
		space.loss_time = None;

		for (packet, info) in space.sent_packets.range(0..largest_acked_packet) {
			if prev_packet != Some(packet.wrapping_sub(1)) {
				// An intervening packet was acknowledged
				persistent_congestion_start = None;
			}

			// Packets sent before now - loss_delay are deemed lost.
			// However, we avoid this subtraction as it can panic and there's no
			// saturating equivalent of this substraction operation with a Duration.
			let packet_too_old = now.saturating_duration_since(info.time_sent) >= loss_delay;
			if packet_too_old || largest_acked_packet >= packet + packet_threshold {
				if Some(packet) == in_flight_mtu_probe {
					// Lost MTU probes are not included in `lost_packets`, because they should not
					// trigger a congestion control response
					lost_mtu_probe = in_flight_mtu_probe;
				} else {
					lost_packets.push(packet);
					size_of_lost_packets += info.size as u64;
					if info.ack_eliciting && due_to_ack {
						match persistent_congestion_start {
							// Two ACK-eliciting packets lost more than congestion_period apart, with no
							// ACKed packets in between
							Some(start) if info.time_sent - start > congestion_period => {
								in_persistent_congestion = true;
							}
							// Persistent congestion must start after the first RTT sample
							None if self
								.path
								.first_packet_after_rtt_sample
								.is_some_and(|x| x < (pn_space, packet)) =>
							{
								persistent_congestion_start = Some(info.time_sent);
							}
							_ => {}
						}
					}
				}
			} else {
				let next_loss_time = info.time_sent + loss_delay;
				space.loss_time = Some(space.loss_time.map_or(next_loss_time, |x| cmp::min(x, next_loss_time)));
				persistent_congestion_start = None;
			}

			prev_packet = Some(packet);
		}

		self.drain_lost_packets(now, pn_space);

		// OnPacketsLost
		if !lost_packets.is_empty() {
			let mut lost = Vec::new();
			self.stats.path.lost_packets += lost_packets.len() as u64;
			self.stats.path.lost_bytes += size_of_lost_packets;
			trace!("packets lost: {:?}, bytes lost: {}", lost_packets, size_of_lost_packets);

			for &packet in &lost_packets {
				let info = self.spaces[pn_space].sent_packets.remove(packet).unwrap(); // safe: lost_packets is populated just above
				// A packet from another path carries another controller's state.
				if let Some(state) = info
					.congestion
					.filter(|_| info.path_generation == self.path.generation())
				{
					let new_burst = packet
						.checked_sub(1)
						.is_none_or(|prev| self.spaces[pn_space].lost_packets.get(prev).is_none());
					lost.push(congestion::Lost {
						sent: info.time_sent,
						bytes: info.size,
						packet: state,
						new_burst,
					});
				}
				self.qlog
					.emit_packet_lost(packet, &info, loss_delay, pn_space, now, self.orig_rem_cid);

				self.remove_in_flight(&info);
				// A probe's duplicates leave the loss of their data to the originals
				if !info.bandwidth_probe {
					for frame in info.stream_frames {
						self.streams.retransmit(frame);
					}
				}
				self.spaces[pn_space].pending |= info.retransmits;
				self.path.mtud.on_non_probe_lost(packet, info.size);

				self.spaces[pn_space].lost_packets.insert(
					packet,
					LostPacket {
						time_sent: info.time_sent,
					},
				);
			}

			if self.path.mtud.black_hole_detected(now) {
				self.stats.path.black_holes_detected += 1;
				self.path.congestion.on_mtu_update(self.path.mtud.current_mtu());
				self.datagrams().drop_oversized();
			}

			// Don't apply congestion penalty for lost ack-only packets
			if !lost.is_empty() {
				self.stats.path.congestion_events += 1;
				self.path.congestion.on_loss(congestion::Loss {
					now,
					packets: &lost,
					in_flight: self.path.in_flight.bytes,
					persistent: in_persistent_congestion,
				});
			}
		}

		// Handle a lost MTU probe
		if let Some(packet) = lost_mtu_probe {
			let info = self.spaces[SpaceId::Data].sent_packets.remove(packet).unwrap(); // safe: lost_mtu_probe is omitted from lost_packets, and therefore must not have been removed yet
			self.remove_in_flight(&info);
			self.path.mtud.on_probe_lost();
			self.stats.path.lost_plpmtud_probes += 1;
		}
	}

	fn loss_time_and_space(&self) -> Option<(Instant, SpaceId)> {
		SpaceId::iter()
			.filter_map(|id| Some((self.spaces[id].loss_time?, id)))
			.min_by_key(|&(time, _)| time)
	}

	fn pto_time_and_space(&self, now: Instant) -> Option<(Instant, SpaceId)> {
		let backoff = 2u32.pow(self.pto_count.min(MAX_BACKOFF_EXPONENT));
		let mut duration = self.path.rtt.pto_base() * backoff;

		if self.path.in_flight.ack_eliciting == 0 {
			debug_assert!(!self.peer_completed_address_validation());
			let space = match self.highest_space {
				SpaceId::Handshake => SpaceId::Handshake,
				_ => SpaceId::Initial,
			};
			return Some((now + duration, space));
		}

		let mut result = None;
		for space in SpaceId::iter() {
			if !self.spaces[space].has_in_flight() {
				continue;
			}
			if space == SpaceId::Data {
				// Skip ApplicationData until handshake completes.
				if self.is_handshaking() {
					return result;
				}
				// Include max_ack_delay and backoff for ApplicationData.
				duration += self.ack_frequency.max_ack_delay_for_pto() * backoff;
			}
			let Some(last_ack_eliciting) = self.spaces[space].time_of_last_ack_eliciting_packet else {
				continue;
			};
			let pto = last_ack_eliciting + duration;
			if result.is_none_or(|(earliest_pto, _)| pto < earliest_pto) {
				result = Some((pto, space));
			}
		}
		result
	}

	fn peer_completed_address_validation(&self) -> bool {
		if self.side.is_server() || self.state.is_closed() {
			return true;
		}
		// The server is guaranteed to have validated our address if any of our handshake or 1-RTT
		// packets are acknowledged or we've seen HANDSHAKE_DONE and discarded handshake keys.
		self.spaces[SpaceId::Handshake].largest_acked_packet.is_some()
			|| self.spaces[SpaceId::Data].largest_acked_packet.is_some()
			|| (self.spaces[SpaceId::Data].crypto.is_some() && self.spaces[SpaceId::Handshake].crypto.is_none())
	}

	fn set_loss_detection_timer(&mut self, now: Instant) {
		if self.state.is_closed() {
			// No loss detection takes place on closed connections, and `close_common` already
			// stopped time timer. Ensure we don't restart it inadvertently, e.g. in response to a
			// reordered packet being handled by state-insensitive code.
			return;
		}

		if let Some((loss_time, _)) = self.loss_time_and_space() {
			// Time threshold loss detection.
			self.timers.set(Timer::LossDetection, loss_time);
			return;
		}

		if self.path.anti_amplification_blocked(1) {
			// We wouldn't be able to send anything, so don't bother.
			self.timers.stop(Timer::LossDetection);
			return;
		}

		if self.path.in_flight.ack_eliciting == 0 && self.peer_completed_address_validation() {
			// There is nothing to detect lost, so no timer is set. However, the client needs to arm
			// the timer if the server might be blocked by the anti-amplification limit.
			self.timers.stop(Timer::LossDetection);
			return;
		}

		// Determine which PN space to arm PTO for.
		// Calculate PTO duration
		if let Some((timeout, _)) = self.pto_time_and_space(now) {
			self.timers.set(Timer::LossDetection, timeout);
		} else {
			self.timers.stop(Timer::LossDetection);
		}
	}

	/// Probe Timeout
	fn pto(&self, space: SpaceId) -> Duration {
		let max_ack_delay = match space {
			SpaceId::Initial | SpaceId::Handshake => Duration::ZERO,
			SpaceId::Data => self.ack_frequency.max_ack_delay_for_pto(),
		};
		self.path.rtt.pto_base() + max_ack_delay
	}

	fn on_packet_authenticated(
		&mut self,
		now: Instant,
		space_id: SpaceId,
		ecn: Option<EcnCodepoint>,
		packet: Option<u64>,
		spin: bool,
	) {
		self.reset_keep_alive(now);
		self.reset_idle_timeout(now, space_id);
		self.permit_idle_reset = true;
		self.receiving_ecn |= ecn.is_some();
		if let Some(x) = ecn {
			let space = &mut self.spaces[space_id];
			space.ecn_counters += x;

			if x.is_ce() {
				space.pending_acks.set_immediate_ack_required();
			}
		}

		let Some(packet) = packet else {
			return;
		};
		if self.side.is_server() && self.spaces[SpaceId::Initial].crypto.is_some() && space_id == SpaceId::Handshake {
			// A server stops sending and processing Initial packets when it receives its first Handshake packet.
			self.discard_space(now, SpaceId::Initial);
		}
		let space = &mut self.spaces[space_id];
		space.pending_acks.insert_one(packet, now);
		if packet >= space.rx_packet {
			space.rx_packet = packet;
			// Update outgoing spin bit, inverting iff we're the client
			self.spin = self.side.is_client() ^ spin;
		}

		self.qlog.emit_packet_received(packet, space_id, now, self.orig_rem_cid);
	}

	fn reset_idle_timeout(&mut self, now: Instant, space: SpaceId) {
		let timeout = match &self.state {
			state if state.is_closed() => None,
			State::Handshake(_) => Some(self.config.handshake_timeout),
			_ => self.idle_timeout,
		};
		// A deadline too far out to represent never fires.
		let deadline = timeout.and_then(|timeout| now.checked_add(cmp::max(timeout, 3 * self.pto(space))));
		match deadline {
			Some(deadline) => self.timers.set(Timer::Idle, deadline),
			None => self.timers.stop(Timer::Idle),
		}
	}

	fn reset_keep_alive(&mut self, now: Instant) {
		let interval = match self.keep_alive {
			Some(x) if self.state.is_established() => x,
			_ => return,
		};
		// A peer may advertise an idle timeout whose derived interval is too far out to represent;
		// that keep-alive never fires.
		match now.checked_add(interval) {
			Some(deadline) => self.timers.set(Timer::KeepAlive, deadline),
			None => self.timers.stop(Timer::KeepAlive),
		}
	}

	fn reset_cid_retirement(&mut self) {
		if let Some(t) = self.local_cid_state.next_timeout() {
			self.timers.set(Timer::PushNewCid, t);
		}
	}

	/// Handle the already-decrypted first packet from the client
	///
	/// Decrypting the first packet in the `Endpoint` allows stateless packet handling to be more
	/// efficient.
	pub(crate) fn handle_first_packet(
		&mut self,
		now: Instant,
		remote: SocketAddr,
		ecn: Option<EcnCodepoint>,
		packet_number: u64,
		packet: InitialPacket,
		remaining: Option<BytesMut>,
	) -> Result<(), Error> {
		let span = trace_span!("first recv");
		let _guard = span.enter();
		debug_assert!(self.side.is_server());
		let len = packet.header_data.len() + packet.payload.len();
		self.path.total_recvd = len as u64;

		match &mut self.state {
			State::Handshake(state) => {
				state.expected_token = packet.header.token.clone();
			}
			_ => unreachable!("first packet must be delivered in Handshake state"),
		}

		self.on_packet_authenticated(now, SpaceId::Initial, ecn, Some(packet_number), false);

		self.process_decrypted_packet(now, remote, Some(packet_number), packet.into())?;
		if let Some(data) = remaining {
			self.handle_coalesced(now, remote, ecn, data);
		}

		self.qlog
			.emit_recovery_metrics(self.pto_count, &mut self.path, now, self.orig_rem_cid);

		Ok(())
	}

	fn read_crypto(
		&mut self,
		space: SpaceId,
		crypto: &frame::Crypto,
		payload_len: usize,
	) -> Result<(), TransportError> {
		let expected = if !self.state.is_handshake() {
			SpaceId::Data
		} else if self.highest_space == SpaceId::Initial {
			SpaceId::Initial
		} else {
			// On the server, self.highest_space can be Data after receiving the client's first
			// flight, but we expect Handshake CRYPTO until the handshake is complete.
			SpaceId::Handshake
		};
		// Handshake keys follow Initial keys, and 1-RTT payloads wait for the handshake,
		// so CRYPTO data cannot arrive from a later-than-expected space.
		debug_assert!(space <= expected, "received out-of-order CRYPTO data");

		let end = crypto.offset + crypto.data.len() as u64;
		if space < expected && end > self.spaces[space].crypto_stream.bytes_read() {
			warn!("received new {:?} CRYPTO data when expecting {:?}", space, expected);
			return Err(TransportError::PROTOCOL_VIOLATION(
				"new data at unexpected encryption level",
			));
		}

		let space = &mut self.spaces[space];
		let max = end.saturating_sub(space.crypto_stream.bytes_read());
		if max > self.config.crypto_buffer_size as u64 {
			return Err(TransportError::CRYPTO_BUFFER_EXCEEDED(""));
		}

		space
			.crypto_stream
			.insert(crypto.offset, crypto.data.clone(), payload_len)
			.map_err(|_| TransportError::INTERNAL_ERROR("too many gaps in crypto stream buffer"))?;

		while let Some(chunk) = space.crypto_stream.read(usize::MAX) {
			trace!("consumed {} CRYPTO bytes", chunk.bytes.len());
			if self.crypto.read_handshake(&chunk.bytes)? {
				self.events.push_back(Event::HandshakeDataReady);
			}
		}

		Ok(())
	}

	fn write_crypto(&mut self) {
		loop {
			let space = self.highest_space;
			let mut outgoing = Vec::new();
			if let Some(crypto) = self.crypto.write_handshake(&mut outgoing) {
				match space {
					SpaceId::Initial => {
						self.upgrade_crypto(SpaceId::Handshake, crypto);
					}
					SpaceId::Handshake => {
						self.upgrade_crypto(SpaceId::Data, crypto);
					}
					_ => unreachable!("got updated secrets during 1-RTT"),
				}
			}
			if outgoing.is_empty() {
				if space == self.highest_space {
					break;
				} else {
					// Keys updated, check for more data to send
					continue;
				}
			}
			let offset = self.spaces[space].crypto_offset;
			let outgoing = Bytes::from(outgoing);
			if let State::Handshake(state) = &mut self.state
				&& space == SpaceId::Initial
				&& offset == 0
				&& self.side.is_client()
			{
				state.client_hello = Some(outgoing.clone());
			}
			self.spaces[space].crypto_offset += outgoing.len() as u64;
			trace!("wrote {} {:?} CRYPTO bytes", outgoing.len(), space);
			self.spaces[space]
				.pending
				.crypto
				.push_back(frame::Crypto { offset, data: outgoing });
		}
	}

	/// Switch to stronger cryptography during handshake
	fn upgrade_crypto(&mut self, space: SpaceId, crypto: Keys) {
		debug_assert!(
			self.spaces[space].crypto.is_none(),
			"already reached packet space {space:?}"
		);
		trace!("{:?} keys ready", space);
		if space == SpaceId::Data {
			// Precompute the first key update
			self.next_crypto = Some(self.crypto.next_1rtt_keys().expect("handshake should be complete"));
		}

		self.spaces[space].crypto = Some(crypto);
		debug_assert!(space as usize > self.highest_space as usize);
		self.highest_space = space;
	}

	fn discard_space(&mut self, now: Instant, space_id: SpaceId) {
		debug_assert!(space_id != SpaceId::Data);
		trace!("discarding {:?} keys", space_id);
		if space_id == SpaceId::Initial {
			// No longer needed
			if let ConnectionSide::Client { token, .. } = &mut self.side {
				*token = Bytes::new();
			}
		}
		let space = &mut self.spaces[space_id];
		space.crypto = None;
		space.time_of_last_ack_eliciting_packet = None;
		space.loss_time = None;
		let sent_packets = mem::take(&mut space.sent_packets);
		for packet in sent_packets.into_values() {
			self.remove_in_flight(&packet);
		}
		// Discarding keys is progress, so the backoff from probing the old space
		// must not delay the next one (RFC 9002 A.11). A client never resets it on
		// an Initial ACK, so this is where its handshake backoff ends.
		self.pto_count = 0;
		self.set_loss_detection_timer(now)
	}

	fn handle_coalesced(&mut self, now: Instant, remote: SocketAddr, ecn: Option<EcnCodepoint>, data: BytesMut) {
		self.path.total_recvd = self.path.total_recvd.saturating_add(data.len() as u64);
		let mut remaining = Some(data);
		while let Some(data) = remaining {
			match PartialDecode::new(
				data,
				&FixedLengthConnectionIdParser::new(self.local_cid_state.cid_len()),
				self.grease_quic_bit,
			) {
				Ok((partial_decode, rest)) => {
					remaining = rest;
					self.handle_decode(now, remote, ecn, partial_decode);
				}
				Err(e) => {
					trace!("malformed header: {}", e);
					return;
				}
			}
		}
	}

	fn handle_decode(
		&mut self,
		now: Instant,
		remote: SocketAddr,
		ecn: Option<EcnCodepoint>,
		partial_decode: PartialDecode,
	) {
		if let Some(decoded) =
			packet_crypto::unprotect_header(partial_decode, &self.spaces, self.peer_params.stateless_reset_token)
		{
			self.handle_packet(now, remote, ecn, decoded.packet, decoded.stateless_reset);
		}
	}

	fn handle_packet(
		&mut self,
		now: Instant,
		remote: SocketAddr,
		ecn: Option<EcnCodepoint>,
		packet: Option<Packet>,
		stateless_reset: bool,
	) {
		self.stats.udp_rx.ios += 1;
		if let Some(packet) = &packet {
			trace!(
				"got {:?} packet ({} bytes) from {} using id {}",
				packet.header.space(),
				packet.payload.len() + packet.header_data.len(),
				remote,
				packet.header.dst_cid(),
			);
		}

		if self.is_handshaking() && remote != self.path.remote {
			debug!("discarding packet with unexpected remote during handshake");
			return;
		}

		let was_closed = self.state.is_closed();
		let was_drained = self.state.is_drained();

		let decrypted = match packet {
			None => Err(None),
			Some(mut packet) => self
				.decrypt_packet(now, &mut packet)
				.map(move |number| (packet, number)),
		};
		let result = match decrypted {
			_ if stateless_reset => {
				debug!("got stateless reset");
				Err(Error::Reset)
			}
			Err(Some(e)) => {
				warn!("illegal packet: {}", e);
				Err(e.into())
			}
			Err(None) => {
				debug!("failed to authenticate packet");
				self.authentication_failures += 1;
				let integrity_limit = self.spaces[self.highest_space]
					.crypto
					.as_ref()
					.unwrap()
					.packet
					.local
					.integrity_limit();
				if self.authentication_failures > integrity_limit {
					Err(TransportError::AEAD_LIMIT_REACHED("integrity limit violated").into())
				} else {
					return;
				}
			}
			Ok((packet, number)) => {
				let span = match number {
					Some(pn) => trace_span!("recv", space = ?packet.header.space(), pn),
					None => trace_span!("recv", space = ?packet.header.space()),
				};
				let _guard = span.enter();

				let is_duplicate = |n| self.spaces[packet.header.space()].dedup.insert(n);
				if number.is_some_and(is_duplicate) {
					debug!("discarding possible duplicate packet");
					return;
				} else if self.state.is_handshake() && packet.header.is_short() {
					// A server has 1-RTT keys before the handshake completes, but must not
					// process 1-RTT packets until it does (RFC 9001 section 5.7). A client's
					// first 1-RTT packets, such as a request sent right after its Finished, can
					// overtake the Finished, so keep a few instead of making the client wait for
					// a retransmission.
					if let Some(number) = number
						&& self.early_1rtt.len() < MAX_EARLY_1RTT_PACKETS
					{
						trace!("buffering short packet during handshake");
						self.early_1rtt.push(EarlyPacket {
							remote,
							ecn,
							number,
							packet,
						});
					} else {
						trace!("dropping short packet during handshake");
					}
					return;
				} else {
					if let Header::Initial(InitialHeader { token, .. }) = &packet.header
						&& let State::Handshake(hs) = &self.state
						&& self.side.is_server()
						&& token != &hs.expected_token
					{
						// Clients must send the same retry token in every Initial. Initial
						// packets can be spoofed, so we discard rather than killing the
						// connection.
						warn!("discarding Initial with invalid retry token");
						return;
					}

					self.process_authenticated_packet(now, remote, ecn, number, packet)
				}
			}
		};

		self.finish_packet(now, remote, result, was_closed, was_drained);

		if !self.state.is_handshake() {
			// The handshake is over. Process the 1-RTT packets that arrived during it, in order.
			for early in mem::take(&mut self.early_1rtt) {
				if self.state.is_closed() {
					break;
				}
				let _guard = trace_span!("recv", space = ?SpaceId::Data, pn = early.number).entered();
				trace!("processing short packet buffered during handshake");
				let was_closed = self.state.is_closed();
				let was_drained = self.state.is_drained();
				let result =
					self.process_authenticated_packet(now, early.remote, early.ecn, Some(early.number), early.packet);
				self.finish_packet(now, early.remote, result, was_closed, was_drained);
			}
		}
	}

	/// Process a packet that has been decrypted and authenticated
	fn process_authenticated_packet(
		&mut self,
		now: Instant,
		remote: SocketAddr,
		ecn: Option<EcnCodepoint>,
		number: Option<u64>,
		packet: Packet,
	) -> Result<(), Error> {
		if !self.state.is_closed() {
			let spin = match packet.header {
				Header::Short { spin, .. } => spin,
				_ => false,
			};
			self.on_packet_authenticated(now, packet.header.space(), ecn, number, spin);
		}
		let result = self.process_decrypted_packet(now, remote, number, packet);
		// Only a protected packet is the peer's word: anyone on the path can send a Retry or a
		// Version Negotiation packet. One from another address, such as a probe of a new path,
		// says nothing of the active one.
		if number.is_some() && remote == self.path.remote {
			self.heard_since_sent = true;
			self.probe_on_heard(now);
		}
		result
	}

	/// Apply the state transitions that follow processing a packet
	fn finish_packet(
		&mut self,
		now: Instant,
		remote: SocketAddr,
		result: Result<(), Error>,
		was_closed: bool,
		was_drained: bool,
	) {
		// State transitions for error cases
		if let Err(Error::Reset) = result
			&& matches!(self.state, State::Closed(_) | State::Draining | State::Drained)
		{
			// Preserve the original close reason once the connection has closed.
			self.state = State::Drained;
		} else if let Err(conn_err) = result {
			self.lose(conn_err.clone());
			self.state = match conn_err {
				Error::ApplicationClosed(reason) => State::closed(reason),
				Error::ConnectionClosed(reason) => State::closed(reason),
				Error::Reset
				| Error::TransportError(TransportError {
					code: TransportErrorCode::AEAD_LIMIT_REACHED,
					..
				}) => State::Drained,
				Error::TimedOut | Error::HandshakeTimeout => {
					unreachable!("timeouts aren't generated by packet processing");
				}
				Error::TransportError(err) => {
					debug!("closing connection due to transport error: {}", err);
					State::closed(err)
				}
				Error::VersionMismatch => State::Draining,
				Error::LocallyClosed { .. } => {
					unreachable!("LocallyClosed isn't generated by packet processing");
				}
				Error::CidsExhausted => {
					unreachable!("CidsExhausted isn't generated by packet processing");
				}
			};
		}

		if !was_closed && self.state.is_closed() {
			self.close_common();
			if !self.state.is_drained() {
				self.set_close_timer(now);
			}
		}
		if matches!(self.state, State::Draining | State::Drained) {
			self.report_draining();
		}
		if !was_drained && self.state.is_drained() {
			self.endpoint_events.push_back(EndpointEventInner::Drained);
			// Close timer may have been started previously, e.g. if we sent a close and got a
			// stateless reset in response
			self.timers.stop(Timer::Close);
		}

		// Transmit CONNECTION_CLOSE if necessary
		if let State::Closed(_) = self.state {
			self.close = remote == self.path.remote;
		}
	}

	fn process_decrypted_packet(
		&mut self,
		now: Instant,
		remote: SocketAddr,
		number: Option<u64>,
		packet: Packet,
	) -> Result<(), Error> {
		let state = match &mut self.state {
			State::Established => {
				match packet.header.space() {
					SpaceId::Data => self.process_payload(now, remote, number.unwrap(), packet)?,
					_ if packet.header.has_frames() => self.process_early_payload(now, packet)?,
					_ => {
						trace!("discarding unexpected pre-handshake packet");
					}
				}
				return Ok(());
			}
			State::Closed(_) => {
				for result in frame::Iter::new(packet.payload.freeze())? {
					let frame = match result {
						Ok(frame) => frame,
						Err(err) => {
							debug!("frame decoding error: {err:?}");
							continue;
						}
					};

					if let Frame::Padding = frame {
						continue;
					};

					self.stats.frame_rx.record(&frame);

					if let Frame::Close(_) = frame {
						trace!("draining");
						self.state = State::Draining;
						break;
					}
				}
				return Ok(());
			}
			State::Draining | State::Drained => return Ok(()),
			State::Handshake(state) => state,
		};
		// Whether a server Initial was processed or a Retry accepted. Packets the client discards
		// never count, or a forged Retry would make it ignore the genuine one.
		let processed_server_packet = state.rem_cid_set || self.retry_src_cid.is_some();

		match packet.header {
			Header::Retry { src_cid: rem_cid, .. } => {
				if self.side.is_server() {
					return Err(TransportError::PROTOCOL_VIOLATION("client sent Retry").into());
				}

				if processed_server_packet
                            || packet.payload.len() <= 16 // token + 16 byte tag
                            || !self.crypto.is_valid_retry(
                                self.rem_cids.active(),
                                &packet.header_data,
                                &packet.payload,
                            )
                            || rem_cid == self.initial_dst_cid
				{
					trace!("discarding invalid Retry");
					// - After the client has received and processed an Initial or Retry
					//   packet from the server, it MUST discard any subsequent Retry
					//   packets that it receives.
					// - A client MUST discard a Retry packet with a zero-length Retry Token
					//   field.
					// - Clients MUST discard Retry packets that have a Retry Integrity Tag
					//   that cannot be validated
					return Ok(());
				}

				trace!("retrying with CID {}", rem_cid);
				let client_hello = state.client_hello.take().unwrap();
				self.retry_src_cid = Some(rem_cid);
				self.rem_cids.update_initial_cid(rem_cid);
				self.rem_handshake_cid = rem_cid;

				let space = &mut self.spaces[SpaceId::Initial];
				if let Some(info) = space.sent_packets.remove(0)
					&& let Some(acked) = self.on_packet_acked(info)
				{
					self.path.congestion.on_ack(congestion::Ack {
						now,
						packets: &[acked],
						in_flight: self.path.in_flight.bytes,
						app_limited: self.app_limited,
						rtt: self.path.rtt.get(),
					});
				};

				self.discard_space(now, SpaceId::Initial); // Make sure we clean up after any retransmitted Initials
				self.spaces[SpaceId::Initial] = PacketSpace {
					crypto: Some(self.crypto.initial_keys(rem_cid, self.side.side())),
					next_packet_number: self.spaces[SpaceId::Initial].next_packet_number,
					crypto_offset: client_hello.len() as u64,
					..PacketSpace::new(now)
				};
				self.spaces[SpaceId::Initial].pending.crypto.push_back(frame::Crypto {
					offset: 0,
					data: client_hello,
				});

				let token_len = packet.payload.len() - 16;
				let ConnectionSide::Client { token, .. } = &mut self.side else {
					unreachable!("we already short-circuited if we're server");
				};
				*token = packet.payload.freeze().split_to(token_len);
				self.state = State::Handshake(state::Handshake {
					expected_token: Bytes::new(),
					rem_cid_set: false,
					client_hello: None,
				});
				Ok(())
			}
			Header::Long {
				ty: LongType::Handshake,
				src_cid: rem_cid,
				..
			} => {
				if rem_cid != self.rem_handshake_cid {
					debug!(
						"discarding packet with mismatched remote CID: {} != {}",
						self.rem_handshake_cid, rem_cid
					);
					return Ok(());
				}
				self.on_path_validated();

				self.process_early_payload(now, packet)?;
				if self.state.is_closed() {
					return Ok(());
				}

				if self.crypto.is_handshaking() {
					trace!("handshake ongoing");
					return Ok(());
				}

				if self.side.is_client() {
					// Client-only because server params were set from the client's Initial
					let params = self.crypto.transport_parameters()?.ok_or_else(|| {
						TransportError::new(
							TransportErrorCode::crypto(0x6d),
							"transport parameters missing".to_owned(),
						)
					})?;

					if let Some(token) = params.stateless_reset_token {
						self.endpoint_events
							.push_back(EndpointEventInner::ResetToken(self.path.remote, token));
					}
					self.handle_peer_params(params)?;
					self.issue_first_cids(now);
				} else {
					// Server-only
					self.spaces[SpaceId::Data].pending.handshake_done = true;
					self.discard_space(now, SpaceId::Handshake);
					self.events.push_back(Event::HandshakeConfirmed);
					self.streams.waits.confirmed();
					trace!("handshake confirmed");
				}

				self.events.push_back(Event::Connected);
				self.streams.waits.connected();
				self.state = State::Established;
				// Swap the handshake idle timeout for the negotiated one.
				self.reset_idle_timeout(now, SpaceId::Data);
				trace!("established");
				Ok(())
			}
			Header::Initial(InitialHeader { src_cid: rem_cid, .. }) => {
				if !state.rem_cid_set {
					trace!("switching remote CID to {}", rem_cid);
					let mut state = state.clone();
					self.rem_cids.update_initial_cid(rem_cid);
					self.rem_handshake_cid = rem_cid;
					self.orig_rem_cid = rem_cid;
					state.rem_cid_set = true;
					self.state = State::Handshake(state);
				} else if rem_cid != self.rem_handshake_cid {
					debug!(
						"discarding packet with mismatched remote CID: {} != {}",
						self.rem_handshake_cid, rem_cid
					);
					return Ok(());
				}

				let starting_space = self.highest_space;
				self.process_early_payload(now, packet)?;

				if self.side.is_server() && starting_space == SpaceId::Initial && self.highest_space != SpaceId::Initial
				{
					let params = self.crypto.transport_parameters()?.ok_or_else(|| {
						TransportError::new(
							TransportErrorCode::crypto(0x6d),
							"transport parameters missing".to_owned(),
						)
					})?;
					self.handle_peer_params(params)?;
					self.issue_first_cids(now);
				}
				Ok(())
			}
			Header::Long {
				ty: LongType::ZeroRtt, ..
			} => Ok(()),
			Header::VersionNegotiate { .. } => {
				// RFC 9000 6.2: discard once any other packet was processed
				if processed_server_packet {
					return Ok(());
				}
				let supported = packet.payload.chunks(4).any(|x| match <[u8; 4]>::try_from(x) {
					Ok(version) => self.version.wire() == u32::from_be_bytes(version),
					Err(_) => false,
				});
				if supported {
					return Ok(());
				}
				debug!("remote doesn't support our version");
				Err(Error::VersionMismatch)
			}
			Header::Short { .. } => {
				unreachable!("short packets received during handshake are discarded in handle_packet")
			}
		}
	}

	/// Process an Initial or Handshake packet payload
	fn process_early_payload(&mut self, now: Instant, packet: Packet) -> Result<(), TransportError> {
		debug_assert_ne!(packet.header.space(), SpaceId::Data);
		let payload_len = packet.payload.len();
		let mut ack_eliciting = false;
		for result in frame::Iter::new(packet.payload.freeze())? {
			let frame = result?;
			let span = match frame {
				Frame::Padding => continue,
				_ => Some(trace_span!("frame", ty = %frame.ty())),
			};

			self.stats.frame_rx.record(&frame);

			let _guard = span.as_ref().map(|x| x.enter());
			ack_eliciting |= frame.is_ack_eliciting();

			// Process frames
			match frame {
				Frame::Padding | Frame::Ping => {}
				Frame::Crypto(frame) => {
					self.read_crypto(packet.header.space(), &frame, payload_len)?;
				}
				Frame::Ack(ack) => {
					self.on_ack_received(now, packet.header.space(), ack)?;
				}
				// Per RFC 9000 §12.4 Table 3, only a CONNECTION_CLOSE frame of type 0x1c may
				// appear in Initial or Handshake packets. An application close (0x1d) falls
				// through to the catch-all arm below.
				Frame::Close(reason @ Close::Connection(_)) => {
					self.lose(reason.into());
					self.state = State::Draining;
					return Ok(());
				}
				_ => {
					let mut err = TransportError::PROTOCOL_VIOLATION("illegal frame type in handshake");
					err.frame = Some(frame.ty());
					return Err(err);
				}
			}
		}

		if ack_eliciting {
			// In the initial and handshake spaces, ACKs must be sent immediately
			self.spaces[packet.header.space()]
				.pending_acks
				.set_immediate_ack_required();
		}

		self.write_crypto();
		Ok(())
	}

	fn process_payload(
		&mut self,
		now: Instant,
		remote: SocketAddr,
		number: u64,
		packet: Packet,
	) -> Result<(), TransportError> {
		let payload = packet.payload.freeze();
		// Earned before anything this packet carries is charged against the cap, and granted now:
		// the refill retires the credit deadline, so a later timeout would not grant it.
		if self.streams.grant_receive_rate(now).should_transmit() {
			self.spaces[SpaceId::Data].pending.max_data = true;
		}
		let mut is_probing_packet = true;
		let mut close = None;
		let payload_len = payload.len();
		let mut ack_eliciting = false;
		let mut frames = frame::Iter::new(payload)?;
		while let Some((result, frame_size)) = frames.next_with_size() {
			let frame = result?;
			let span = match frame {
				Frame::Padding => continue,
				_ => Some(trace_span!("frame", ty = %frame.ty())),
			};

			self.stats.frame_rx.record(&frame);
			// Crypto, Stream and Datagram frames are special cased in order no pollute
			// the log with payload data
			match &frame {
				Frame::Crypto(f) => {
					trace!(offset = f.offset, len = f.data.len(), "got crypto frame");
				}
				Frame::Stream(f) => {
					trace!(id = %f.id, offset = f.offset, len = f.data.len(), fin = f.fin, "got stream frame");
				}
				Frame::Datagram(f) => {
					trace!(len = f.data.len(), "got datagram frame");
				}
				f => {
					trace!("got frame {:?}", f);
				}
			}

			let _guard = span.as_ref().map(|x| x.enter());
			ack_eliciting |= frame.is_ack_eliciting();
			// Check whether this could be a probing packet
			match frame {
				Frame::Padding | Frame::PathChallenge(_) | Frame::PathResponse(_) | Frame::NewConnectionId(_) => {}
				_ => {
					is_probing_packet = false;
				}
			}
			match frame {
				Frame::Crypto(frame) => {
					self.read_crypto(SpaceId::Data, &frame, payload_len)?;
				}
				Frame::Stream(frame) => {
					if self.streams.received(frame, payload_len)?.should_transmit() {
						self.spaces[SpaceId::Data].pending.max_data = true;
					}
				}
				Frame::Ack(ack) => {
					self.on_ack_received(now, SpaceId::Data, ack)?;
				}
				Frame::Padding | Frame::Ping => {}
				Frame::Close(reason) => {
					close = Some(reason);
				}
				Frame::PathChallenge(token) => {
					self.path_responses.push(number, token, remote);
					if remote == self.path.remote {
						// PATH_CHALLENGE on active path, possible off-path packet forwarding
						// attack. Send a non-probing packet to recover the active path.
						match self.peer_supports_ack_frequency() {
							true => self.immediate_ack(),
							false => self.ping(),
						}
					}
				}
				Frame::PathResponse(token) => {
					if self.path.challenge == Some(token) && remote == self.path.remote {
						trace!("new path validated");
						self.timers.stop(Timer::PathValidation);
						self.path.challenge = None;
						self.path.validated = true;
						if let Some((_, prev_path)) = &mut self.prev_path {
							prev_path.challenge = None;
							prev_path.challenge_pending = false;
						}
					} else {
						debug!(token, "ignoring invalid PATH_RESPONSE");
					}
				}
				Frame::MaxData(bytes) => {
					self.streams.received_max_data(bytes);
				}
				Frame::MaxStreamData { id, offset } => {
					self.streams.received_max_stream_data(id, offset)?;
				}
				Frame::MaxStreams { dir, count } => {
					self.streams.received_max_streams(dir, count)?;
				}
				Frame::ResetStream(frame) => {
					if frame.reliable_size.is_some() && !self.config.reset_stream_at {
						// We never advertised it, so it is an unknown frame type
						let mut err = TransportError::FRAME_ENCODING_ERROR("unsupported RESET_STREAM_AT");
						err.frame = Some(frame::FrameType::RESET_STREAM_AT);
						return Err(err);
					}
					if self.streams.received_reset(frame)?.should_transmit() {
						self.spaces[SpaceId::Data].pending.max_data = true;
					}
				}
				Frame::DataBlocked { offset } => {
					debug!(offset, "peer claims to be blocked at connection level");
				}
				Frame::StreamDataBlocked { id, offset } => {
					if id.initiator() == self.side.side() && id.dir() == Dir::Uni {
						debug!("got STREAM_DATA_BLOCKED on send-only {}", id);
						return Err(TransportError::STREAM_STATE_ERROR(
							"STREAM_DATA_BLOCKED on send-only stream",
						));
					}
					debug!(
						stream = %id,
						offset, "peer claims to be blocked at stream level"
					);
				}
				Frame::StreamsBlocked { dir, limit } => {
					if limit > MAX_STREAM_COUNT {
						return Err(TransportError::FRAME_ENCODING_ERROR("unrepresentable stream limit"));
					}
					debug!("peer claims to be blocked opening more than {} {} streams", limit, dir);
				}
				Frame::StopSending(frame::StopSending { id, error_code }) => {
					if let Some(reliable_size) = self.streams.received_stop_sending(id, error_code)? {
						self.spaces[SpaceId::Data]
							.pending
							.reset_stream
							.push((id, reliable_size));
					}
				}
				Frame::RetireConnectionId { sequence } => {
					let allow_more_cids = self
						.local_cid_state
						.on_cid_retirement(sequence, self.peer_params.issue_cids_limit())?;
					self.endpoint_events.push_back(EndpointEventInner::RetireConnectionId(
						now,
						sequence,
						allow_more_cids,
					));
				}
				Frame::NewConnectionId(frame) => {
					trace!(
						sequence = frame.sequence,
						id = %frame.id,
						retire_prior_to = frame.retire_prior_to,
					);
					if self.rem_cids.active().is_empty() {
						return Err(TransportError::PROTOCOL_VIOLATION(
							"NEW_CONNECTION_ID when CIDs aren't in use",
						));
					}
					if frame.retire_prior_to > frame.sequence {
						return Err(TransportError::PROTOCOL_VIOLATION(
							"NEW_CONNECTION_ID retiring unissued CIDs",
						));
					}

					use crate::cid_queue::InsertError;
					match self.rem_cids.insert(frame) {
						Ok(None) => {}
						Ok(Some((retired, reset_token))) => {
							self.spaces[SpaceId::Data].pending.retire_cids(retired)?;
							self.set_reset_token(reset_token);
						}
						Err(InsertError::ExceedsLimit) => {
							return Err(TransportError::CONNECTION_ID_LIMIT_ERROR(""));
						}
						Err(InsertError::Retired) => {
							trace!("discarding already-retired");
							// RETIRE_CONNECTION_ID might not have been previously sent if e.g. a
							// range of connection IDs larger than the active connection ID limit
							// was retired all at once via retire_prior_to.
							self.spaces[SpaceId::Data]
								.pending
								.retire_cids(frame.sequence..frame.sequence.saturating_add(1))?;
							continue;
						}
					};

					if self.side.is_server() && self.rem_cids.active_seq() == 0 {
						// We're a server still using the initial remote CID for the client, so
						// let's switch immediately to enable clientside stateless resets.
						self.update_rem_cid();
					}
				}
				Frame::NewToken(NewToken { token }) => {
					let ConnectionSide::Client {
						token_store,
						server_name,
						..
					} = &self.side
					else {
						return Err(TransportError::PROTOCOL_VIOLATION("client sent NEW_TOKEN"));
					};
					if token.is_empty() {
						return Err(TransportError::FRAME_ENCODING_ERROR("empty token"));
					}
					trace!("got new token");
					token_store.insert(server_name, token);
				}
				Frame::Datagram(datagram) => {
					// The transport parameter limits the encoded frame size, not its heap usage.
					if self
						.config
						.datagram
						.receive_buffer
						.is_some_and(|window| frame_size > window.min(u16::MAX.into()))
					{
						return Err(TransportError::PROTOCOL_VIOLATION("oversized datagram"));
					}
					if !self.streams.admit_datagram(datagram.data.len()) {
						debug!("dropping datagram past the receive rate cap");
						continue;
					}
					if self
						.datagrams
						.received(datagram, &self.config.datagram.receive_buffer)?
					{
						self.events.push_back(Event::DatagramReceived);
						self.streams.waits.wake_datagram();
					}
				}
				Frame::AckFrequency(ack_frequency) => {
					// This frame can only be sent in the Data space
					let space = &mut self.spaces[SpaceId::Data];

					if !self
						.ack_frequency
						.ack_frequency_received(&ack_frequency, &mut space.pending_acks)?
					{
						// The AckFrequency frame is stale (we have already received a more recent one)
						continue;
					}

					// Our `max_ack_delay` has been updated, so we may need to adjust its associated
					// timeout
					if let Some(timeout) = space
						.pending_acks
						.max_ack_delay_timeout(self.ack_frequency.max_ack_delay)
					{
						self.timers.set(Timer::MaxAckDelay, timeout);
					}
				}
				Frame::ImmediateAck => {
					// This frame can only be sent in the Data space
					self.spaces[SpaceId::Data].pending_acks.set_immediate_ack_required();
				}
				Frame::HandshakeDone => {
					if self.side.is_server() {
						return Err(TransportError::PROTOCOL_VIOLATION("client sent HANDSHAKE_DONE"));
					}
					if self.spaces[SpaceId::Handshake].crypto.is_some() {
						self.discard_space(now, SpaceId::Handshake);
					}
					self.events.push_back(Event::HandshakeConfirmed);
					self.streams.waits.confirmed();
					trace!("handshake confirmed");
				}
			}
		}

		let space = &mut self.spaces[SpaceId::Data];
		if space
			.pending_acks
			.packet_received(now, number, ack_eliciting, &space.dedup)
		{
			self.timers
				.set(Timer::MaxAckDelay, now + self.ack_frequency.max_ack_delay);
			self.next_bundled_ack_time = Some(now);
		}

		// Issue stream ID credit due to ACKs of outgoing finish/resets and incoming finish/resets
		// on stopped streams. Incoming finishes/resets on open streams are not handled here as they
		// are only freed, and hence only issue credit, once the application has been notified
		// during a read on the stream.
		let pending = &mut self.spaces[SpaceId::Data].pending;
		self.streams.queue_max_stream_id(pending);

		if let Some(reason) = close {
			self.lose(reason.into());
			self.state = State::Draining;
			self.close = true;
		}

		if remote != self.path.remote && !is_probing_packet && number == self.spaces[SpaceId::Data].rx_packet {
			let ConnectionSide::Server { server_config } = &self.side else {
				panic!("packets from unknown remote should be dropped by clients");
			};
			debug_assert!(
				server_config.migration,
				"migration-initiating packets should have been dropped immediately"
			);
			self.migrate(now, remote);
			// Break linkability, if possible
			self.update_rem_cid();
			self.spin = false;
		}

		Ok(())
	}

	fn migrate(&mut self, now: Instant, remote: SocketAddr) {
		trace!(%remote, "migration initiated");
		self.path_counter = self.path_counter.wrapping_add(1);
		// Reset rtt/congestion state for new path unless it looks like a NAT rebinding.
		// Note that the congestion window will not grow until validation terminates. Helps mitigate
		// amplification attacks performed by spoofing source addresses.
		let mut new_path = if remote.is_ipv4() && remote.ip() == self.path.remote.ip() {
			PathData::from_previous(remote, &self.path, self.path_counter)
		} else {
			let peer_max_udp_payload_size =
				u16::try_from(self.peer_params.max_udp_payload_size.into_inner()).unwrap_or(u16::MAX);
			PathData::new(
				remote,
				self.allow_mtud,
				Some(peer_max_udp_payload_size),
				self.path_counter,
				now,
				&self.config,
			)
		};
		new_path.challenge = Some(self.rng.random());
		new_path.challenge_pending = true;
		let prev_pto = self.pto(SpaceId::Data);

		let mut prev = mem::replace(&mut self.path, new_path);
		self.datagrams().drop_oversized();
		self.events.push_back(Event::PathUpdated);

		// Don't clobber the original path if the previous one hasn't been validated yet
		if prev.challenge.is_none() {
			prev.challenge = Some(self.rng.random());
			prev.challenge_pending = true;
			// We haven't updated the remote CID yet, this captures the remote CID we were using on
			// the previous path.
			self.prev_path = Some((self.rem_cids.active(), prev));
		}

		self.timers.set(
			Timer::PathValidation,
			now + 3 * cmp::max(self.pto(SpaceId::Data), prev_pto),
		);
	}

	/// Handle a change in the local address, i.e. an active migration
	pub fn local_address_changed(&mut self) {
		self.update_rem_cid();
		self.ping();
	}

	/// Switch to a previously unused remote connection ID, if possible
	fn update_rem_cid(&mut self) {
		let Some((reset_token, retired)) = self.rem_cids.next() else {
			return;
		};

		// Retire the current remote CID and any CIDs we had to skip.
		self.spaces[SpaceId::Data].pending.retire_cids.extend(retired);
		self.set_reset_token(reset_token);
	}

	fn set_reset_token(&mut self, reset_token: ResetToken) {
		self.endpoint_events
			.push_back(EndpointEventInner::ResetToken(self.path.remote, reset_token));
		self.peer_params.stateless_reset_token = Some(reset_token);
	}

	/// Issue an initial set of connection IDs to the peer upon connection
	fn issue_first_cids(&mut self, now: Instant) {
		if self.local_cid_state.cid_len() == 0 {
			return;
		}

		// Subtract 1 to account for the CID we supplied while handshaking
		let mut n = self.peer_params.issue_cids_limit() - 1;
		if let ConnectionSide::Server { server_config } = &self.side
			&& server_config.has_preferred_address()
		{
			// We also sent a CID in the transport parameters
			n -= 1;
		}
		self.endpoint_events
			.push_back(EndpointEventInner::NeedIdentifiers(now, n));
	}

	fn populate_packet(
		&mut self,
		now: Instant,
		space_id: SpaceId,
		buf: &mut Vec<u8>,
		max_size: usize,
		pn: u64,
		bandwidth_probe: bool,
	) -> SentFrames {
		let mut sent = SentFrames::default();
		let space = &mut self.spaces[space_id];
		space.pending_acks.maybe_ack_non_eliciting();

		let pre_payload_len = buf.len();

		// HANDSHAKE_DONE
		if mem::replace(&mut space.pending.handshake_done, false) {
			buf.write(frame::FrameType::HANDSHAKE_DONE);
			sent.retransmits.get_or_create().handshake_done = true;
			// This is just a u8 counter and the frame is typically just sent once
			self.stats.frame_tx.handshake_done = self.stats.frame_tx.handshake_done.saturating_add(1);
		}

		// PING
		if mem::replace(&mut space.ping_pending, false) {
			trace!("PING");
			buf.write(frame::FrameType::PING);
			sent.non_retransmits = true;
			self.stats.frame_tx.ping += 1;
		}

		// IMMEDIATE_ACK
		if mem::replace(&mut space.immediate_ack_pending, false) {
			trace!("IMMEDIATE_ACK");
			buf.write(frame::FrameType::IMMEDIATE_ACK);
			sent.non_retransmits = true;
			self.stats.frame_tx.immediate_ack += 1;
		}

		// ACK
		if space.pending_acks.can_send() {
			Self::try_populate_acks(
				now,
				self.receiving_ecn,
				&mut sent,
				space,
				buf,
				&mut self.stats,
				max_size,
			);
		}

		// ACK_FREQUENCY
		if mem::replace(&mut space.pending.ack_frequency, false) {
			let sequence_number = self.ack_frequency.next_sequence_number();

			// Safe to unwrap because this is always provided when ACK frequency is enabled
			let config = self.config.ack_frequency.as_ref().unwrap();

			// Ensure the delay is within bounds to avoid a PROTOCOL_VIOLATION error
			let max_ack_delay =
				self.ack_frequency
					.candidate_max_ack_delay(self.path.rtt.get(), config, &self.peer_params);

			trace!(?max_ack_delay, "ACK_FREQUENCY");

			frame::AckFrequency {
				sequence: sequence_number,
				ack_eliciting_threshold: config.ack_eliciting_threshold,
				request_max_ack_delay: max_ack_delay.as_micros().try_into().unwrap_or(VarInt::MAX),
				reordering_threshold: config.reordering_threshold,
			}
			.encode(buf);

			sent.retransmits.get_or_create().ack_frequency = true;

			self.ack_frequency.ack_frequency_sent(pn, max_ack_delay);
			self.path
				.congestion
				.on_ack_frequency_update(config.ack_eliciting_threshold.into_inner(), max_ack_delay);
			self.stats.frame_tx.ack_frequency += 1;
		}

		// PATH_CHALLENGE
		if buf.len() + 9 < max_size && space_id == SpaceId::Data {
			// Transmit challenges with every outgoing frame on an unvalidated path
			if let Some(token) = self.path.challenge {
				// But only send a packet solely for that purpose at most once
				self.path.challenge_pending = false;
				sent.non_retransmits = true;
				sent.requires_padding = true;
				trace!("PATH_CHALLENGE {:08x}", token);
				buf.write(frame::FrameType::PATH_CHALLENGE);
				buf.write(token);
				self.stats.frame_tx.path_challenge += 1;
			}
		}

		// PATH_RESPONSE
		if buf.len() + 9 < max_size
			&& space_id == SpaceId::Data
			&& let Some(token) = self.path_responses.pop_on_path(self.path.remote)
		{
			sent.non_retransmits = true;
			sent.requires_padding = true;
			trace!("PATH_RESPONSE {:08x}", token);
			buf.write(frame::FrameType::PATH_RESPONSE);
			buf.write(token);
			self.stats.frame_tx.path_response += 1;
		}

		// CRYPTO
		while buf.len() + frame::Crypto::SIZE_BOUND < max_size {
			let Some(mut frame) = space.pending.crypto.pop_front() else {
				break;
			};

			// Calculate the maximum amount of crypto data we can store in the buffer.
			// Since the offset is known, we can reserve the exact size required to encode it.
			// For length we reserve 2bytes which allows to encode up to 2^14,
			// which is more than what fits into normally sized QUIC frames.
			let max_crypto_data_size = max_size
                - buf.len()
                - 1 // Frame Type
                - VarInt::size(unsafe { VarInt::from_u64_unchecked(frame.offset) })
                - 2; // Maximum encoded length for frame size, given we send less than 2^14 bytes

			let len = frame.data.len().min(2usize.pow(14) - 1).min(max_crypto_data_size);

			let data = frame.data.split_to(len);
			let truncated = frame::Crypto {
				offset: frame.offset,
				data,
			};
			trace!("CRYPTO: off {} len {}", truncated.offset, truncated.data.len());
			truncated.encode(buf);
			self.stats.frame_tx.crypto += 1;
			sent.retransmits.get_or_create().crypto.push_back(truncated);
			if !frame.data.is_empty() {
				frame.offset += len as u64;
				space.pending.crypto.push_front(frame);
			}
		}

		if space_id == SpaceId::Data {
			self.streams.write_control_frames(
				buf,
				&mut space.pending,
				&mut sent.retransmits,
				&mut self.stats.frame_tx,
				max_size,
			);
		}

		// NEW_CONNECTION_ID
		while buf.len() + NewConnectionId::SIZE_BOUND < max_size {
			let Some(issued) = space.pending.new_cids.pop() else {
				break;
			};
			trace!(
				sequence = issued.sequence,
				id = %issued.id,
				"NEW_CONNECTION_ID"
			);
			NewConnectionId {
				sequence: issued.sequence,
				retire_prior_to: self.local_cid_state.retire_prior_to(),
				id: issued.id,
				reset_token: issued.reset_token,
			}
			.encode(buf);
			sent.retransmits.get_or_create().new_cids.push(issued);
			self.stats.frame_tx.new_connection_id += 1;
		}

		// RETIRE_CONNECTION_ID
		while buf.len() + frame::RETIRE_CONNECTION_ID_SIZE_BOUND < max_size {
			let Some(seq) = space.pending.retire_cids.pop() else {
				break;
			};
			trace!(sequence = seq, "RETIRE_CONNECTION_ID");
			buf.write(frame::FrameType::RETIRE_CONNECTION_ID);
			buf.write_var(seq);
			sent.retransmits.get_or_create().retire_cids.push(seq);
			self.stats.frame_tx.retire_connection_id += 1;
		}

		// NEW_TOKEN
		while let Some(remote_addr) = space.pending.new_tokens.pop() {
			debug_assert_eq!(space_id, SpaceId::Data);
			let ConnectionSide::Server { server_config } = &self.side else {
				panic!("NEW_TOKEN frames should not be enqueued by clients");
			};

			if remote_addr != self.path.remote {
				// NEW_TOKEN frames contain tokens bound to a client's IP address, and are only
				// useful if used from the same IP address.  Thus, we abandon enqueued NEW_TOKEN
				// frames upon an path change. Instead, when the new path becomes validated,
				// NEW_TOKEN frames may be enqueued for the new path instead.
				continue;
			}

			let token = Token::new(
				TokenPayload::Validation {
					ip: remote_addr.ip(),
					issued: moq_clock::wall(),
				},
				&mut self.rng,
			);
			let new_token = NewToken {
				token: token.encode(&*server_config.token_key).into(),
			};

			if buf.len() + new_token.size() >= max_size {
				space.pending.new_tokens.push(remote_addr);
				break;
			}

			new_token.encode(buf);
			sent.retransmits.get_or_create().new_tokens.push(remote_addr);
			self.stats.frame_tx.new_token += 1;
		}

		// STREAM and DATAGRAM, interleaved in schedule order
		if space_id == SpaceId::Data {
			let written = self
				.streams
				.write_frames(buf, pre_payload_len..max_size, self.config.send_fairness);
			sent.stream_frames = written.streams;
			sent.non_retransmits |= written.datagrams > 0;
			// Only a packet with nothing new to send probes, so its STREAM frames are all
			// duplicates
			if bandwidth_probe {
				debug_assert!(sent.stream_frames.is_empty());
				sent.stream_frames = self
					.bandwidth_probe
					.write(&space.sent_packets, &self.streams, buf, max_size);
				debug_assert!(
					!sent.stream_frames.is_empty(),
					"a probe found no in-flight data to resend"
				);
				sent.bandwidth_probe = true;
			}
			self.stats.frame_tx.stream += sent.stream_frames.len() as u64;
			self.stats.frame_tx.datagram += written.datagrams;
		}

		// Bundle ACK with other frames when there is room for them.
		// We want to reuse encryption and underlying protocol overhead,
		// but sending multiple ACKs for a single incoming packet is a waste of peer's resources,
		// so we have next_bundled_ack_time to control when to send ACKs.
		let any_frames_sent = buf.len() > pre_payload_len;
		if any_frames_sent
			&& sent.largest_acked.is_none()
			&& self.next_bundled_ack_time.is_some_and(|time| time <= now)
			&& space.pending_acks.can_send_with_other_frames()
		{
			Self::try_populate_acks(
				now,
				self.receiving_ecn,
				&mut sent,
				space,
				buf,
				&mut self.stats,
				max_size,
			);
		}

		sent
	}

	/// Tries to write pending ACKs into a buffer if there is enough space.
	///
	/// If the ACK frame does not fit into the buffer, the ACK frame will not
	/// be sent at all.
	///
	/// This method assumes ACKs are pending, and should only be called if
	/// `!PendingAcks::ranges().is_empty()` returns `true`.
	fn try_populate_acks(
		now: Instant,
		receiving_ecn: bool,
		sent: &mut SentFrames,
		space: &mut PacketSpace,
		buf: &mut Vec<u8>,
		stats: &mut Stats,
		max_size: usize,
	) {
		debug_assert!(!space.pending_acks.ranges().is_empty());

		let ecn = if receiving_ecn { Some(&space.ecn_counters) } else { None };

		let delay_micros = space.pending_acks.ack_delay(now).as_micros() as u64;

		// TODO: This should come from `transport::Config` if that gets configurable.
		let ack_delay_exp = TransportParameters::default().ack_delay_exponent;
		let delay = delay_micros >> ack_delay_exp.into_inner();

		trace!("ACK {:?}, Delay = {}us", space.pending_acks.ranges(), delay_micros);

		let no_acks_len = buf.len();
		frame::Ack::encode(delay as _, space.pending_acks.ranges(), ecn, buf);
		if buf.len() > max_size {
			// The ACK frame is too large. Remove it.
			buf.truncate(no_acks_len);
			return;
		}
		sent.largest_acked = space.pending_acks.ranges().max();
		stats.frame_tx.acks += 1;
	}

	fn close_common(&mut self) {
		trace!("connection closed");
		for &timer in &Timer::VALUES {
			self.timers.stop(timer);
		}
	}

	fn set_close_timer(&mut self, now: Instant) {
		self.timers.set(Timer::Close, now + 3 * self.pto(self.highest_space));
	}

	/// Handle transport parameters received from the peer
	fn handle_peer_params(&mut self, params: TransportParameters) -> Result<(), TransportError> {
		if Some(self.orig_rem_cid) != params.initial_src_cid
			|| (self.side.is_client()
				&& (Some(self.initial_dst_cid) != params.original_dst_cid
					|| self.retry_src_cid != params.retry_src_cid))
		{
			return Err(TransportError::TRANSPORT_PARAMETER_ERROR("CID authentication failure"));
		}

		self.set_peer_params(params);

		Ok(())
	}

	fn set_peer_params(&mut self, params: TransportParameters) {
		self.streams.set_params(&params);
		self.idle_timeout = negotiate_max_idle_timeout(self.config.max_idle_timeout(), Some(params.max_idle_timeout));
		self.keep_alive = transport::keep_alive_interval(self.config.keep_alive, self.idle_timeout);
		trace!(
			"negotiated max idle timeout {:?}, keep-alive {:?}",
			self.idle_timeout, self.keep_alive
		);
		if let Some(info) = params.preferred_address {
			self.rem_cids
				.insert(NewConnectionId {
					sequence: 1,
					id: info.connection_id,
					reset_token: info.stateless_reset_token,
					retire_prior_to: 0,
				})
				.expect("preferred address CID is the first received, and hence is guaranteed to be legal");
		}
		self.ack_frequency.peer_max_ack_delay = get_max_ack_delay(&params);
		self.peer_params = params;
		self.path.mtud.on_peer_max_udp_payload_size_received(
			u16::try_from(self.peer_params.max_udp_payload_size.into_inner()).unwrap_or(u16::MAX),
		);
	}

	fn decrypt_packet(&mut self, now: Instant, packet: &mut Packet) -> Result<Option<u64>, Option<TransportError>> {
		let result = packet_crypto::decrypt_packet_body(
			packet,
			&self.spaces,
			self.key_phase,
			self.prev_crypto.as_ref(),
			self.next_crypto.as_ref(),
		)?;

		let Some(result) = result else {
			return Ok(None);
		};

		if result.outgoing_key_update_acked
			&& let Some(prev) = self.prev_crypto.as_mut()
		{
			prev.end_packet = Some((result.number, now));
			self.set_key_discard_timer(packet.header.space());
		}

		if result.incoming_key_update {
			trace!("key update authenticated");
			self.update_keys(Some((result.number, now)), true);
			self.set_key_discard_timer(packet.header.space());
		}

		Ok(Some(result.number))
	}

	fn update_keys(&mut self, end_packet: Option<(u64, Instant)>, remote: bool) {
		trace!("executing key update");
		// Generate keys for the key phase after the one we're switching to, store them in
		// `next_crypto`, make the contents of `next_crypto` current, and move the current keys into
		// `prev_crypto`.
		let new = self.crypto.next_1rtt_keys().expect("only called for `Data` packets");
		self.key_phase_size = new.local.confidentiality_limit().saturating_sub(KEY_UPDATE_MARGIN);
		let old = mem::replace(
			&mut self.spaces[SpaceId::Data]
				.crypto
				.as_mut()
				.unwrap() // safe because update_keys() can only be triggered by short packets
				.packet,
			mem::replace(self.next_crypto.as_mut().unwrap(), new),
		);
		self.spaces[SpaceId::Data].sent_with_keys = 0;
		self.prev_crypto = Some(PrevCrypto {
			crypto: old,
			end_packet,
			update_unacked: remote,
		});
		self.key_phase = !self.key_phase;
	}

	fn peer_supports_ack_frequency(&self) -> bool {
		self.peer_params.min_ack_delay.is_some()
	}

	#[cfg(test)]
	pub(crate) fn disable_peer_ack_frequency(&mut self) {
		self.peer_params.min_ack_delay = None;
	}

	/// Pretends the peer advertised `delay` as its min_ack_delay, as a stack with a coarser timer
	/// does
	#[cfg(test)]
	pub(crate) fn set_peer_min_ack_delay(&mut self, delay: Duration) {
		self.peer_params.min_ack_delay = Some(VarInt::from_u64(delay.as_micros() as u64).unwrap());
	}

	/// Send an IMMEDIATE_ACK frame to the remote endpoint
	///
	/// According to the spec, this will result in an error if the remote endpoint does not support
	/// the Acknowledgement Frequency extension
	pub(crate) fn immediate_ack(&mut self) {
		self.spaces[self.highest_space].immediate_ack_pending = true;
	}

	/// Decodes a packet, returning its decrypted payload, so it can be inspected in tests
	#[cfg(test)]
	pub(crate) fn decode_packet(&self, event: &ConnectionEvent) -> Option<Vec<u8>> {
		let ConnectionEventInner::Datagram(DatagramConnectionEvent {
			first_decode,
			remaining,
			..
		}) = &event.0
		else {
			return None;
		};

		if remaining.is_some() {
			panic!("Packets should never be coalesced in tests");
		}

		let decrypted_header = packet_crypto::unprotect_header(
			first_decode.clone(),
			&self.spaces,
			self.peer_params.stateless_reset_token,
		)?;

		let mut packet = decrypted_header.packet?;
		packet_crypto::decrypt_packet_body(
			&mut packet,
			&self.spaces,
			self.key_phase,
			self.prev_crypto.as_ref(),
			self.next_crypto.as_ref(),
		)
		.ok()?;

		Some(packet.payload.to_vec())
	}

	/// Encrypts `payload` as this connection's next packet in `space`, untracked, so tests can
	/// send frames a correct peer never would
	#[cfg(test)]
	pub(crate) fn forge_packet(&mut self, now: Instant, space: SpaceId, payload: &[u8]) -> Vec<u8> {
		let mut buf = Vec::new();
		let builder = PacketBuilder::new(now, space, self.rem_cids.active(), &mut buf, usize::MAX, 0, true, self)
			.expect("connection can still send");
		buf.extend_from_slice(payload);
		builder.finish(self, now, &mut buf);
		buf
	}

	/// The number of bytes of packets containing retransmittable frames that have not been
	/// acknowledged or declared lost.
	#[cfg(test)]
	pub(crate) fn bytes_in_flight(&self) -> u64 {
		self.path.in_flight.bytes
	}

	/// Forget the newest packet sent in the Data space, as making room for an ACK-only packet does.
	#[cfg(test)]
	pub(crate) fn forget_newest_sent_packet(&mut self) {
		let sent = &mut self.spaces[SpaceId::Data].sent_packets;
		let packet = sent.remove(sent.last().expect("a packet sent")).unwrap();
		self.path.remove_in_flight(&packet);
	}

	/// Number of bytes worth of non-ack-only packets that may be sent
	#[cfg(test)]
	pub(crate) fn congestion_window(&self) -> u64 {
		self.path.congestion.window().saturating_sub(self.path.in_flight.bytes)
	}

	/// When the receive rate cap next grants credit
	#[cfg(test)]
	pub(crate) fn receive_rate_deadline(&self) -> Option<Instant> {
		self.streams.receive_rate_deadline()
	}

	/// Whether no timers but keepalive, idle, rtt, pushnewcid, and key discard are running
	#[cfg(test)]
	pub(crate) fn is_idle(&self) -> bool {
		if self.streams.receive_rate_deadline().is_some() {
			return false;
		}
		Timer::VALUES
			.iter()
			.filter(|&&t| !matches!(t, Timer::KeepAlive | Timer::PushNewCid | Timer::KeyDiscard))
			.filter_map(|&t| Some((t, self.timers.get(t)?)))
			.min_by_key(|&(_, time)| time)
			.is_none_or(|(timer, _)| timer == Timer::Idle)
	}

	/// Whether explicit congestion notification is in use on outgoing packets.
	#[cfg(test)]
	pub(crate) fn using_ecn(&self) -> bool {
		self.path.sending_ecn
	}

	/// The number of received bytes in the current path
	#[cfg(test)]
	pub(crate) fn total_recvd(&self) -> u64 {
		self.path.total_recvd
	}

	#[cfg(test)]
	pub(crate) fn active_local_cid_seq(&self) -> (u64, u64) {
		self.local_cid_state.active_seq()
	}

	/// Instruct the peer to replace previously issued CIDs by sending a NEW_CONNECTION_ID frame
	/// with updated `retire_prior_to` field set to `v`
	#[cfg(test)]
	pub(crate) fn rotate_local_cid(&mut self, v: u64, now: Instant) {
		let n = self.local_cid_state.assign_retire_seq(v);
		self.endpoint_events
			.push_back(EndpointEventInner::NeedIdentifiers(now, n));
	}

	/// Check the current active remote CID sequence
	#[cfg(test)]
	pub(crate) fn active_rem_cid_seq(&self) -> u64 {
		self.rem_cids.active_seq()
	}

	/// Returns the detected maximum udp payload size for the current path
	#[cfg(test)]
	pub(crate) fn path_mtu(&self) -> u16 {
		self.path.current_mtu()
	}

	/// Whether we have 1-RTT data to send
	///
	/// See also `self.space(SpaceId::Data).can_send()`
	fn can_send_1rtt(&self, max_size: usize) -> bool {
		self.streams.can_send_frames(max_size)
			|| self.path.challenge_pending
			|| self.prev_path.as_ref().is_some_and(|(_, x)| x.challenge_pending)
			|| !self.path_responses.is_empty()
	}

	/// Update counters to account for a packet becoming acknowledged, lost, or abandoned
	fn remove_in_flight(&mut self, packet: &SentPacket) {
		// Visit known paths from newest to oldest to find the one `packet` was sent on
		for path in [&mut self.path]
			.into_iter()
			.chain(self.prev_path.as_mut().map(|(_, data)| data))
		{
			if path.remove_in_flight(packet) {
				return;
			}
		}
	}

	/// Record why the connection was lost, failing every wait once it drains what it can
	///
	/// [`Self::poll`] reports it once as [`Event::ConnectionLost`].
	fn lose(&mut self, reason: Error) {
		self.streams.waits.lose(reason.clone());
		self.error = Some(reason);
	}

	/// Terminate the connection instantly, without sending a close packet
	fn kill(&mut self, reason: Error) {
		self.close_common();
		self.lose(reason);
		self.state = State::Drained;
		self.report_draining();
		self.endpoint_events.push_back(EndpointEventInner::Drained);
	}

	/// Tell the endpoint the connection is no longer active, the first time it stops being so
	fn report_draining(&mut self) {
		if !mem::replace(&mut self.draining_reported, true) {
			self.endpoint_events.push_back(EndpointEventInner::Draining);
		}
	}

	/// Storage size required for the largest packet known to be supported by the current path
	///
	/// Buffers passed to [`Connection::poll_transmit`] should be at least this large.
	pub fn current_mtu(&self) -> u16 {
		self.path.current_mtu()
	}

	/// Size of non-frame data for a 1-RTT packet
	///
	/// Quantifies space consumed by the QUIC header and AEAD tag. All other bytes in a packet are
	/// frames. Changes if the length of the remote connection ID changes, which is expected to be
	/// rare. If `pn` is specified, may additionally change unpredictably due to variations in
	/// latency and packet loss.
	fn predict_1rtt_overhead(&self, pn: Option<u64>) -> usize {
		let pn_len = match pn {
			Some(pn) => PacketNumber::new(pn, self.spaces[SpaceId::Data].largest_acked_packet.unwrap_or(0)).len(),
			// Upper bound
			None => 4,
		};

		// 1 byte for flags
		1 + self.rem_cids.active().len() + pn_len + self.tag_len_1rtt()
	}

	fn tag_len_1rtt(&self) -> usize {
		// All supported cipher suites use 16-byte tags before negotiation chooses one.
		self.spaces[SpaceId::Data]
			.crypto
			.as_ref()
			.map_or(16, |keys| keys.packet.local.tag_len())
	}

	/// Mark the path as validated, and enqueue NEW_TOKEN frames to be sent as appropriate
	fn on_path_validated(&mut self) {
		self.path.validated = true;
		let ConnectionSide::Server { server_config } = &self.side else {
			return;
		};
		let new_tokens = &mut self.spaces[SpaceId::Data as usize].pending.new_tokens;
		new_tokens.clear();
		for _ in 0..server_config.validation_token.sent {
			new_tokens.push(self.path.remote);
		}
	}
}

impl fmt::Debug for Connection {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Connection")
			.field("handshake_cid", &self.handshake_cid)
			.finish()
	}
}

/// Fields of `Connection` specific to it being client-side or server-side
enum ConnectionSide {
	Client {
		/// Sent in every outgoing Initial packet. Always empty after Initial keys are discarded
		token: Bytes,
		token_store: Arc<dyn TokenStore>,
		server_name: String,
	},
	Server {
		server_config: Arc<server::Config>,
	},
}

impl ConnectionSide {
	fn remote_may_migrate(&self) -> bool {
		match self {
			Self::Server { server_config } => server_config.migration,
			Self::Client { .. } => false,
		}
	}

	fn is_client(&self) -> bool {
		self.side().is_client()
	}

	fn is_server(&self) -> bool {
		self.side().is_server()
	}

	fn side(&self) -> Side {
		match *self {
			Self::Client { .. } => Side::Client,
			Self::Server { .. } => Side::Server,
		}
	}
}

impl From<SideArgs> for ConnectionSide {
	fn from(side: SideArgs) -> Self {
		match side {
			SideArgs::Client {
				token_store,
				server_name,
			} => Self::Client {
				token: token_store.take(&server_name).unwrap_or_default(),
				token_store,
				server_name,
			},
			SideArgs::Server {
				server_config,
				pref_addr_cid: _,
				path_validated: _,
			} => Self::Server { server_config },
		}
	}
}

/// Parameters to `Connection::new` other than the TLS session
pub(crate) struct ConnectionArgs {
	pub(crate) grease_quic_bit: bool,
	pub(crate) transport_config: Arc<transport::Config>,
	/// Destination CID of the first Initial packet sent by the client
	pub(crate) init_cid: ConnectionId,
	/// CID initially issued to the peer for addressing this connection
	pub(crate) loc_cid: ConnectionId,
	/// CID initially issued by the peer for addressing it
	pub(crate) rem_cid: ConnectionId,
	pub(crate) remote: SocketAddr,
	pub(crate) local_ip: Option<IpAddr>,
	pub(crate) local_cid_len: usize,
	pub(crate) local_cid_lifetime: Option<Duration>,
	pub(crate) now: Instant,
	pub(crate) version: Version,
	pub(crate) allow_mtud: bool,
	pub(crate) rng_seed: [u8; 32],
	pub(crate) side_args: SideArgs,
}

/// Parameters to `Connection::new` specific to it being client-side or server-side
pub(crate) enum SideArgs {
	Client {
		token_store: Arc<dyn TokenStore>,
		server_name: String,
	},
	Server {
		server_config: Arc<server::Config>,
		pref_addr_cid: Option<ConnectionId>,
		path_validated: bool,
	},
}

impl SideArgs {
	pub(crate) fn pref_addr_cid(&self) -> Option<ConnectionId> {
		match *self {
			Self::Client { .. } => None,
			Self::Server { pref_addr_cid, .. } => pref_addr_cid,
		}
	}

	pub(crate) fn path_validated(&self) -> bool {
		match *self {
			Self::Client { .. } => true,
			Self::Server { path_validated, .. } => path_validated,
		}
	}

	pub(crate) fn side(&self) -> Side {
		match *self {
			Self::Client { .. } => Side::Client,
			Self::Server { .. } => Side::Server,
		}
	}
}

/// Reasons why a connection might be lost
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
	/// The peer doesn't implement any supported version
	#[error("peer doesn't implement any supported version")]
	VersionMismatch,
	/// The peer violated the QUIC specification as understood by this implementation
	#[error(transparent)]
	TransportError(#[from] TransportError),
	/// The peer's QUIC stack aborted the connection automatically
	#[error("aborted by peer: {0}")]
	ConnectionClosed(frame::ConnectionClose),
	/// The peer closed the connection
	#[error("closed by peer: {0}")]
	ApplicationClosed(frame::ApplicationClose),
	/// The peer is unable to continue processing this connection, usually due to having restarted
	#[error("reset by peer")]
	Reset,
	/// Communication with the peer has lapsed for longer than the negotiated idle timeout
	///
	/// If neither side is sending keep-alives, a connection will time out after a long enough idle
	/// period even if the peer is still reachable. See also [`transport::Config::idle_timeout`]
	/// and [`transport::Config::keep_alive`].
	#[error("timed out")]
	TimedOut,
	/// The handshake did not finish within [`transport::Config::handshake_timeout`]
	#[error("handshake timed out")]
	HandshakeTimeout,
	/// The local application closed the connection
	#[error("closed locally: {reason} (code {code})")]
	LocallyClosed {
		/// The application error code the close sent
		code: VarInt,
		/// The reason the close sent
		reason: String,
	},
	/// The connection could not be created because not enough of the CID space is available
	///
	/// Try using longer connection IDs.
	#[error("CIDs exhausted")]
	CidsExhausted,
}

impl From<Close> for Error {
	fn from(x: Close) -> Self {
		match x {
			Close::Connection(reason) => Self::ConnectionClosed(reason),
			Close::Application(reason) => Self::ApplicationClosed(reason),
		}
	}
}

// For compatibility with API consumers
impl From<Error> for io::Error {
	fn from(x: Error) -> Self {
		use Error::*;
		let kind = match x {
			TimedOut | HandshakeTimeout => io::ErrorKind::TimedOut,
			Reset => io::ErrorKind::ConnectionReset,
			ApplicationClosed(_) | ConnectionClosed(_) => io::ErrorKind::ConnectionAborted,
			TransportError(_) | VersionMismatch | LocallyClosed { .. } | CidsExhausted => io::ErrorKind::Other,
		};
		Self::new(kind, x)
	}
}

#[derive(Clone)]
pub(crate) enum State {
	Handshake(state::Handshake),
	Established,
	Closed(state::Closed),
	Draining,
	/// Waiting for application to call close so we can dispose of the resources
	Drained,
}

impl State {
	fn closed<R: Into<Close>>(reason: R) -> Self {
		Self::Closed(state::Closed { reason: reason.into() })
	}

	fn is_handshake(&self) -> bool {
		matches!(*self, Self::Handshake(_))
	}

	fn is_established(&self) -> bool {
		matches!(*self, Self::Established)
	}

	fn is_closed(&self) -> bool {
		matches!(*self, Self::Closed(_) | Self::Draining | Self::Drained)
	}

	fn is_drained(&self) -> bool {
		matches!(*self, Self::Drained)
	}
}

mod state {
	use super::*;

	#[derive(Clone)]
	pub(crate) struct Handshake {
		/// Whether the remote CID has been set by the peer yet
		///
		/// Always set for servers
		pub(super) rem_cid_set: bool,
		/// Stateless retry token received in the first Initial by a server.
		///
		/// Must be present in every Initial. Always empty for clients.
		pub(super) expected_token: Bytes,
		/// First cryptographic message
		///
		/// Only set for clients
		pub(super) client_hello: Option<Bytes>,
	}

	#[derive(Clone)]
	pub(crate) struct Closed {
		pub(super) reason: Close,
	}
}

/// Events of interest to the application
#[derive(Debug)]
#[non_exhaustive]
pub enum Event {
	/// The connection's handshake data is ready
	HandshakeDataReady,
	/// The connection was successfully established
	Connected,
	/// The TLS handshake was confirmed
	HandshakeConfirmed,
	/// The connection was lost
	///
	/// Emitted when the connection is closed due to an error, a timeout, the peer closing it, or
	/// the local application closing it via [`Connection::close()`], which reports
	/// [`Error::LocallyClosed`].
	ConnectionLost {
		/// Reason that the connection was closed
		reason: Error,
	},
	/// Stream events
	Stream(StreamEvent),
	/// One or more application datagrams have been received
	DatagramReceived,
	/// The currently active path was updated
	PathUpdated,
}

fn get_max_ack_delay(params: &TransportParameters) -> Duration {
	Duration::from_micros(params.max_ack_delay.0 * 1000)
}

// Prevents overflow and improves behavior in extreme circumstances
const MAX_BACKOFF_EXPONENT: u32 = 16;

/// Maximum number of 1-RTT packets buffered while the handshake completes
const MAX_EARLY_1RTT_PACKETS: usize = 16;

/// An authenticated 1-RTT packet received before the handshake completed
struct EarlyPacket {
	remote: SocketAddr,
	ecn: Option<EcnCodepoint>,
	number: u64,
	packet: Packet,
}

/// Minimal remaining size to allow packet coalescing, excluding cryptographic tag
///
/// This must be at least as large as the header for a well-formed empty packet to be coalesced,
/// plus some space for frames. We only care about handshake headers because short header packets
/// necessarily have smaller headers, and initial packets are only ever the first packet in a
/// datagram (because we coalesce in ascending packet space order and the only reason to split a
/// packet is when packet space changes).
const MIN_PACKET_SPACE: usize = MAX_HANDSHAKE_HEADER_SIZE + 32;

/// Largest amount of space that could be occupied by a Handshake packet's header
///
/// Excludes packet-type-specific fields such as packet number or Initial token
// https://www.rfc-editor.org/rfc/rfc9000.html#name-handshake-packet: flags + version + dcid len + dcid +
// scid len + scid + length + pn
const MAX_HANDSHAKE_HEADER_SIZE: usize =
	1 + 4 + 1 + MAX_CID_SIZE + 1 + MAX_CID_SIZE + VarInt::from_u32(u16::MAX as u32).size() + 4;

/// Perform key updates this many packets before the AEAD confidentiality limit.
///
/// Chosen arbitrarily, intended to be large enough to prevent spurious connection loss.
const KEY_UPDATE_MARGIN: u64 = 10_000;

#[derive(Default)]
struct SentFrames {
	retransmits: ThinRetransmits,
	largest_acked: Option<u64>,
	stream_frames: StreamMetaVec,
	/// Whether `stream_frames` only resend in-flight data, to probe for bandwidth
	bandwidth_probe: bool,
	/// Whether the packet contains non-retransmittable frames (like datagrams)
	non_retransmits: bool,
	requires_padding: bool,
}

impl SentFrames {
	/// Returns whether the packet contains only ACKs
	fn is_ack_only(&self, streams: &StreamsState) -> bool {
		self.largest_acked.is_some()
			&& !self.non_retransmits
			&& self.stream_frames.is_empty()
			&& self.retransmits.is_empty(streams)
	}
}

/// Compute the negotiated idle timeout based on local and remote max_idle_timeout transport parameters.
///
/// According to the definition of max_idle_timeout, a value of `0` means the timeout is disabled; see <https://www.rfc-editor.org/rfc/rfc9000#section-18.2-4.4.1.>
///
/// According to the negotiation procedure, either the minimum of the timeouts or one specified is used as the negotiated value; see <https://www.rfc-editor.org/rfc/rfc9000#section-10.1-2.>
///
/// Returns the negotiated idle timeout as a `Duration`, or `None` when both endpoints have opted out of idle timeout.
fn negotiate_max_idle_timeout(x: Option<VarInt>, y: Option<VarInt>) -> Option<Duration> {
	match (x, y) {
		(Some(VarInt(0)) | None, Some(VarInt(0)) | None) => None,
		(Some(VarInt(0)) | None, Some(y)) => Some(Duration::from_millis(y.0)),
		(Some(x), Some(VarInt(0)) | None) => Some(Duration::from_millis(x.0)),
		(Some(x), Some(y)) => Some(Duration::from_millis(cmp::min(x, y).0)),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{
		client,
		endpoint::{self, Dial, Endpoint},
	};

	#[cfg(feature = "ring")]
	#[test]
	fn large_initial_token_leaves_room_for_close() {
		let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
		let mut roots = rustls::RootCertStore::empty();
		roots.add(cert.cert.der().clone()).unwrap();
		let config = client::Config::with_root_certificates(Arc::new(roots)).unwrap();
		// With 40 bytes of header overhead, a 16-byte tag, and a 35-byte ACK,
		// a 1084-byte token leaves exactly ConnectionClose::SIZE_BOUND bytes.
		for (token_len, ack_fits) in [
			(1083, true),
			(1084, true),
			(1085, false),
			(1100, false),
			// Exactly enough frame space for CONNECTION_CLOSE alone.
			(1119, false),
		] {
			config.token_store.insert("localhost", vec![0; token_len].into());
			let mut endpoint = Endpoint::new(endpoint::Config::default(), true).unwrap();
			let now = Instant::now();
			let (_, mut conn) = endpoint
				.connect(
					now,
					Dial {
						config: &config,
						remote: "[::1]:4433".parse().unwrap(),
						server_name: "localhost",
					},
				)
				.unwrap();
			let keys = conn.crypto.initial_keys(conn.initial_dst_cid, Side::Server);
			let space = &mut conn.spaces[SpaceId::Initial];
			for pn in (0..32).step_by(2) {
				space.pending_acks.insert_one(pn, now);
				space.dedup.insert(pn);
				space.pending_acks.packet_received(now, pn, true, &space.dedup);
			}
			conn.close(now, 0u32.into(), String::new());
			let mut buf = Vec::new();
			assert!(conn.poll_transmit(now, 1, &mut buf).is_some());
			assert!(buf.len() <= 1200);
			assert!(!conn.close);

			let (packet, rest) =
				PartialDecode::new(buf.as_slice().into(), &FixedLengthConnectionIdParser::new(0), false).unwrap();
			assert!(rest.is_none());
			let mut packet = packet.finish(Some(&*keys.header.remote)).unwrap();
			assert_eq!(packet.header_data.len(), 40 + token_len);
			keys.packet
				.remote
				.decrypt(0, &packet.header_data, &mut packet.payload)
				.unwrap();
			let mut frames = frame::Iter::new(packet.payload.freeze())
				.unwrap()
				.map(Result::unwrap)
				.filter(|frame| !matches!(frame, Frame::Padding));
			if ack_fits {
				assert!(matches!(frames.next(), Some(Frame::Ack(_))), "token {token_len}");
			}
			assert!(
				matches!(
					frames.next(),
					Some(Frame::Close(Close::Connection(frame::ConnectionClose {
						error_code: TransportErrorCode::APPLICATION_ERROR,
						..
					})))
				),
				"token {token_len}"
			);
			assert!(frames.next().is_none());
		}
	}

	#[test]
	fn negotiate_max_idle_timeout_commutative() {
		let test_params = [
			(None, None, None),
			(None, Some(VarInt(0)), None),
			(None, Some(VarInt(2)), Some(Duration::from_millis(2))),
			(Some(VarInt(0)), Some(VarInt(0)), None),
			(Some(VarInt(2)), Some(VarInt(0)), Some(Duration::from_millis(2))),
			(Some(VarInt(1)), Some(VarInt(4)), Some(Duration::from_millis(1))),
		];

		for (left, right, result) in test_params {
			assert_eq!(negotiate_max_idle_timeout(left, right), result);
			assert_eq!(negotiate_max_idle_timeout(right, left), result);
		}
	}
}
