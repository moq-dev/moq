//! The sans-IO QMux connection
//!
//! [`Connection`] multiplexes QUIC streams, flow control, and datagrams over one reliable,
//! ordered transport such as TCP, TLS, or a WebSocket (draft-ietf-quic-qmux). It drives the
//! same stream state machine as a QUIC [`crate::connection::Connection`], through the same receive and
//! write entry points. The draft has acknowledgment happen "as soon as data is passed to the
//! underlying transport", so every frame counts as acknowledged the moment it is serialized
//! into a record, and nothing is retransmitted.
//!
//! Like [`crate::connection::Connection`], it performs no I/O. Feed it each record (each frame on
//! draft-00) the transport delimits with [`Connection::handle_record`], write out what
//! [`Connection::poll_transmit`] returns, run [`Connection::handle_timeout`] at
//! [`Connection::poll_timeout`], and drain [`Connection::poll`] for events.

use std::{collections::VecDeque, num::NonZeroU64};

use bytes::Bytes;
use tracing::{debug, trace};

use super::{
	Error as ConnectionError, State as ConnectionState, ThinRetransmits,
	datagrams::DatagramState,
	spaces::Retransmits,
	stats::FrameStats,
	streams::{Leased, RecvStream, SendGroups, SendOptions, SendStream, StreamEvent, Streams, StreamsState},
};
use crate::{
	ApplicationClose, Dir, Duration, Instant, Limits, MAX_STREAM_COUNT, SendDatagramError, Side, TransportError,
	VarInt,
	frame::{self, Close, Datagram},
	transport_parameters::TransportParameters,
};

mod params;
mod timer;
mod wire;

#[cfg(test)]
mod tests;

pub use wire::Version;

use params::{DEFAULT_MAX_RECORD_SIZE, Params};
use timer::Timer;
use wire::MAX_FRAME_SIZE;

/// The largest STREAM payload sent: released peers reject a larger one on receive
const MAX_STREAM_PAYLOAD: usize = MAX_FRAME_SIZE - 32;

/// The frame space of one record, capped so a STREAM frame that fills it, whose header is at
/// least 2 bytes, stays within [`MAX_STREAM_PAYLOAD`]
const MAX_RECORD_BUDGET: usize = MAX_STREAM_PAYLOAD + 2;

/// How a QMux connection behaves
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Config {
	/// The wire format
	pub version: Version,
	/// Flow-control windows and stream counts; each `None` takes its default: 100 streams of
	/// each direction, a 1 MiB connection window, a 256 KiB stream window, and 8 MiB of
	/// unacknowledged outgoing data
	pub limits: Limits,
	/// Whether streams of equal priority take turns frame by frame, rather than one finishing
	/// first
	pub send_fairness: bool,
	/// How long a silent peer is kept, or `None` for forever; record versions only
	pub idle_timeout: Option<Duration>,
	/// The largest record Size field the peer may send, at least 16382
	pub max_record_size: VarInt,
	/// The largest DATAGRAM frame the peer may send, or `None` to refuse datagrams; record
	/// versions only
	pub max_datagram_frame_size: Option<VarInt>,
	/// Bytes of received datagrams buffered before the oldest is dropped
	pub datagram_receive_buffer_size: usize,
	/// Bytes of outgoing datagrams queued before the oldest is dropped
	pub datagram_send_buffer_size: usize,
	/// How long to wait for the peer's transport parameters, or `None` for forever
	pub handshake_timeout: Option<Duration>,
	/// How often to probe the round-trip time with QX_PING, or `None` not to; record versions
	/// only
	pub rtt_interval: Option<Duration>,
	/// Application protocols to negotiate in-band, in preference order, for transports
	/// without ALPN; `None` when it was negotiated out of band or not at all, which makes a
	/// peer's offer a protocol error; a peer that offers none of them fails the
	/// handshake
	pub protocols: Option<Vec<String>>,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			version: Version::QMux02,
			limits: Limits::default(),
			send_fairness: true,
			idle_timeout: Some(Duration::from_secs(30)),
			max_record_size: VarInt::from_u64(DEFAULT_MAX_RECORD_SIZE).unwrap(),
			max_datagram_frame_size: Some(VarInt::from_u64(DEFAULT_MAX_RECORD_SIZE).unwrap()),
			datagram_receive_buffer_size: 1 << 20,
			datagram_send_buffer_size: 1 << 20,
			handshake_timeout: Some(Duration::from_secs(10)),
			rtt_interval: Some(Duration::from_secs(1)),
			protocols: None,
		}
	}
}

impl Config {
	fn max_streams(&self) -> VarInt {
		limit(self.limits.max_streams, 100)
	}

	fn receive_window(&self) -> VarInt {
		limit(self.limits.receive_window, 1 << 20)
	}

	fn stream_receive_window(&self) -> VarInt {
		limit(self.limits.stream_receive_window, 256 << 10)
	}

	fn send_window(&self) -> u64 {
		self.limits.send_window.map_or(8 << 20, NonZeroU64::get)
	}
}

/// A limit on the wire, or its default when unset; one [`Limits::validate`] refused saturates
fn limit(limit: Option<NonZeroU64>, default: u64) -> VarInt {
	VarInt::from_u64(limit.map_or(default, NonZeroU64::get)).unwrap_or(VarInt::MAX)
}

/// Events from [`Connection::poll`]
#[derive(Debug)]
pub enum Event {
	/// The peer's transport parameters arrived, so streams can be opened
	Connected,
	/// Stream activity, as on QUIC
	Stream(StreamEvent),
	/// One or more datagrams arrived
	DatagramReceived,
	/// The connection ended
	ConnectionLost {
		/// Why: [`ConnectionError::Reset`] means the transport ended without a close frame
		reason: ConnectionError,
	},
}

/// A QMux connection over a reliable, ordered transport
pub struct Connection {
	side: Side,
	config: Config,
	streams: StreamsState,
	/// Control frames waiting to be written
	pending: Retransmits,
	conn_state: ConnectionState,
	stats: FrameStats,

	params_sent: bool,
	peer: Option<Params>,
	protocol: Option<String>,
	timer: Timer,

	next_ping: u64,
	greatest_ping_received: Option<u64>,
	ping_request: bool,
	/// The largest request to answer; one response covers every request up to it
	ping_response: Option<VarInt>,

	datagrams: DatagramState,

	/// The close frame waiting to be written
	close: Option<Close>,
	close_sent: bool,
	error: Option<ConnectionError>,
	events: VecDeque<Event>,
	/// Draft-00 frames already serialized, one per transport message
	queued: VecDeque<Bytes>,
}

impl Connection {
	/// Start a connection; `side` decides which stream IDs are ours
	pub fn new(mut config: Config, side: Side, now: Instant) -> Self {
		if !config.version.uses_records() {
			config.idle_timeout = None;
			config.max_datagram_frame_size = None;
			config.rtt_interval = None;
		}
		let streams = StreamsState::new(
			side,
			config.max_streams(),
			config.max_streams(),
			config.send_window(),
			config.receive_window(),
			config.stream_receive_window(),
		);
		let timer = Timer::new(config.handshake_timeout, now);
		Self {
			side,
			config,
			streams,
			pending: Retransmits::default(),
			conn_state: ConnectionState::Established,
			stats: FrameStats::default(),
			params_sent: false,
			peer: None,
			protocol: None,
			timer,
			next_ping: 0,
			greatest_ping_received: None,
			ping_request: false,
			ping_response: None,
			datagrams: DatagramState::default(),
			close: None,
			close_sent: false,
			error: None,
			events: VecDeque::new(),
			queued: VecDeque::new(),
		}
	}

	/// Which side of the connection this is
	pub fn side(&self) -> Side {
		self.side
	}

	/// The wire format
	pub fn version(&self) -> Version {
		self.config.version
	}

	/// Whether the peer's transport parameters arrived and the connection is still up
	pub fn is_established(&self) -> bool {
		self.peer.is_some() && self.error.is_none()
	}

	/// Whether the connection ended
	pub fn is_closed(&self) -> bool {
		self.error.is_some()
	}

	/// Whether the connection ended and has nothing left to write, so the transport can be
	/// shut down
	pub fn is_drained(&self) -> bool {
		self.error.is_some() && self.close.is_none() && self.queued.is_empty()
	}

	/// Why the connection ended, if it has
	pub fn error(&self) -> Option<&ConnectionError> {
		self.error.as_ref()
	}

	/// The application protocol negotiated in-band, once the peer's parameters arrive
	pub fn protocol(&self) -> Option<&str> {
		self.protocol.as_deref()
	}

	/// The smoothed round-trip time measured with QX_PING, once a probe is answered
	pub fn rtt(&self) -> Option<Duration> {
		self.timer.rtt()
	}

	/// Process one record's frames, or one frame on draft-00
	///
	/// A protocol violation ends the connection with a CONNECTION_CLOSE from the next
	/// [`Self::poll_transmit`].
	pub fn handle_record(&mut self, payload: Bytes, now: Instant) {
		if self.error.is_some() {
			return;
		}
		self.timer.on_recv(now);
		if let Err(err) = self.process(payload, now) {
			self.fail(err);
		}
	}

	/// The transport refused an incoming record larger than `max_record_size`, or a draft-00
	/// frame larger than `max_frame_size`, rather than read it into memory
	pub fn handle_oversized(&mut self) {
		if self.error.is_none() {
			self.fail(self.oversized());
		}
	}

	/// The transport found the peer violating its framing, such as overrunning a buffer it
	/// bounds itself, so close with `err`
	pub fn handle_violation(&mut self, err: TransportError) {
		if self.error.is_none() {
			self.fail(err);
		}
	}

	/// The transport failed, or ended without a close frame
	pub fn transport_closed(&mut self) {
		self.lost(ConnectionError::Reset);
	}

	/// Whether the transport is refusing writes, which defers the idle timeout by at most one
	/// more window: the peer is evidently there, just slow to read
	pub fn set_backpressured(&mut self, backpressured: bool) {
		self.timer.set_backpressured(backpressured);
	}

	/// Close the connection with an APPLICATION_CLOSE, unless it already ended
	///
	/// The frame comes from the next [`Self::poll_transmit`]; flush it before shutting down
	/// the transport.
	pub fn close(&mut self, error_code: VarInt, reason: String) {
		if self.error.is_some() {
			return;
		}
		self.close = Some(Close::Application(ApplicationClose {
			error_code,
			reason: reason.clone(),
		}));
		self.lost(ConnectionError::LocallyClosed {
			code: error_code,
			reason,
		});
	}

	/// The next record to write, without its Size field, or the next frame on draft-00
	pub fn poll_transmit(&mut self, now: Instant) -> Option<Bytes> {
		if let Some(frame) = self.queued.pop_front() {
			return Some(frame);
		}
		let buf = Bytes::from(self.fill(now)?);
		if self.config.version.uses_records() {
			return Some(buf);
		}
		self.split(buf);
		self.queued.pop_front()
	}

	/// The instant [`Self::handle_timeout`] should run next
	pub fn poll_timeout(&self) -> Option<Instant> {
		match self.error {
			Some(_) => None,
			None => self.timer.next(),
		}
	}

	/// Run the handshake, idle, keep-alive, and RTT timers
	pub fn handle_timeout(&mut self, now: Instant) {
		if self.error.is_some() {
			return;
		}
		let due = self.timer.poll(now);
		if due.handshake {
			debug!("qmux handshake timed out");
			self.lost(ConnectionError::HandshakeTimeout);
		} else if due.idle {
			// The idle timeout closes silently, as on QUIC
			debug!("qmux idle timeout");
			self.lost(ConnectionError::TimedOut);
		} else if due.ping {
			self.ping_request = true;
		}
	}

	/// The next event
	pub fn poll(&mut self) -> Option<Event> {
		if let Some(event) = self.events.pop_front() {
			return Some(event);
		}
		self.streams.poll().map(Event::Stream)
	}

	/// Open and accept streams
	pub fn streams(&mut self) -> Streams<'_> {
		Streams {
			state: &mut self.streams,
			conn_state: &self.conn_state,
		}
	}

	/// Schedule outgoing streams in send groups
	pub fn send_groups(&mut self) -> SendGroups<'_> {
		SendGroups {
			state: &mut self.streams,
		}
	}

	/// The send half of stream `id`
	pub fn send_stream(&mut self, id: crate::StreamId) -> SendStream<'_> {
		SendStream {
			id,
			state: &mut self.streams,
			pending: &mut self.pending,
			conn_state: &self.conn_state,
		}
	}

	/// The receive half of stream `id`
	pub fn recv_stream(&mut self, id: crate::StreamId) -> RecvStream<'_> {
		RecvStream {
			id,
			state: &mut self.streams,
			pending: &mut self.pending,
		}
	}

	/// Send and receive datagrams
	pub fn datagrams(&mut self) -> Datagrams<'_> {
		Datagrams { conn: self }
	}

	/// The bytes leased from receive stream `id` with [`RecvStream::lease()`], if it has a lease
	pub fn leased(&self, id: crate::StreamId) -> Option<&Leased> {
		self.streams.leased(id)
	}

	/// The frame space of a record: the peer's `max_record_size`, under the STREAM ceiling
	fn budget(&self) -> usize {
		let peer = self
			.peer
			.as_ref()
			.map_or(DEFAULT_MAX_RECORD_SIZE, |p| p.max_record_size.into_inner());
		usize::try_from(peer).map_or(MAX_RECORD_BUDGET, |peer| peer.min(MAX_RECORD_BUDGET))
	}

	fn local_params(&self) -> Params {
		let config = &self.config;
		let mut tp = TransportParameters::default();
		tp.max_idle_timeout = config.idle_timeout.map_or(VarInt::from_u32(0), |timeout| {
			VarInt::from_u64(timeout.as_millis() as u64).unwrap_or(VarInt::MAX)
		});
		tp.initial_max_data = config.receive_window();
		tp.initial_max_stream_data_bidi_local = config.stream_receive_window();
		tp.initial_max_stream_data_bidi_remote = config.stream_receive_window();
		tp.initial_max_stream_data_uni = config.stream_receive_window();
		tp.initial_max_streams_bidi = config.max_streams();
		tp.initial_max_streams_uni = config.max_streams();
		// A datagram must fit one record
		tp.max_datagram_frame_size = config
			.max_datagram_frame_size
			.map(|size| size.min(config.max_record_size));
		tp.reset_stream_at = config.version == Version::QMux02;
		Params {
			tp,
			max_record_size: config.max_record_size,
			protocols: config.protocols.clone(),
		}
	}

	/// Serialize the next batch of frames, each counted as acknowledged on the spot
	fn fill(&mut self, now: Instant) -> Option<Vec<u8>> {
		if self.close_sent {
			return None;
		}
		let budget = self.budget();
		let mut buf = Vec::new();

		// The transport parameters go first
		if !self.params_sent {
			let mut blob = Vec::new();
			self.local_params().encode(&mut blob);
			wire::encode_transport_parameters(&mut buf, &blob);
			self.params_sent = true;
		}

		if self.error.is_some() {
			// Write the close frame, if any, and go quiet
			if let Some(close) = self.close.take() {
				let room = budget - buf.len();
				close.encode(&mut buf, room);
			}
			self.close_sent = true;
			if buf.is_empty() {
				return None;
			}
			self.timer.on_send(now);
			return Some(buf);
		}

		if let Some(seq) = self.ping_response.take() {
			wire::encode_ping(&mut buf, true, seq);
		}
		if std::mem::take(&mut self.ping_request) {
			let seq = VarInt::from_u64(self.next_ping).expect("QX_PING sequence overflow");
			wire::encode_ping(&mut buf, false, seq);
			self.timer.ping_sent(self.next_ping, now);
			self.next_ping += 1;
		}

		let mut written = ThinRetransmits::default();
		self.streams
			.write_control_frames(&mut buf, &mut self.pending, &mut written, &mut self.stats, budget);
		if let Some(written) = written.get() {
			for &(id, reliable_size) in &written.reset_stream {
				self.streams.reset_acked(id, reliable_size);
			}
		}

		// Stream data and datagrams last, so a STREAM frame without a Length field runs to the
		// end of the record
		let written = self
			.streams
			.write_frames(&mut buf, 0..budget, self.config.send_fairness);
		for meta in written.streams {
			let id = meta.id;
			// A reset held until the peer has the data below its Reliable Size goes out now
			if let Some(reliable_size) = self.streams.received_ack_of(meta) {
				self.pending.reset_stream.push((id, reliable_size));
			}
		}
		// Freed streams may grant the peer more
		self.streams.queue_max_stream_id(&mut self.pending);

		if buf.is_empty() {
			return None;
		}
		self.timer.on_send(now);
		Some(buf)
	}

	/// Queue a draft-00 batch one frame per message, each STREAM frame with its Length field
	fn split(&mut self, mut buf: Bytes) {
		while !buf.is_empty() {
			let before = buf.clone();
			let ty = before[0];
			let frame = wire::next_frame(&mut buf)
				.expect("serialized a valid frame")
				.expect("not empty");
			let len = before.len() - buf.len();
			let stream = match frame {
				wire::Frame::Quic(frame::Frame::Stream(stream)) | wire::Frame::Continue(stream) if ty & 0x02 == 0 => {
					stream
				}
				_ => {
					self.queued.push_back(before.slice(..len));
					continue;
				}
			};
			let meta = frame::StreamMeta {
				id: stream.id,
				offsets: stream.offset..stream.offset + stream.data.len() as u64,
				fin: stream.fin,
			};
			let mut frame = Vec::with_capacity(len + 2);
			meta.encode(true, &mut frame);
			frame.extend_from_slice(&stream.data);
			self.queued.push_back(frame.into());
		}
	}

	fn process(&mut self, mut payload: Bytes, now: Instant) -> Result<(), TransportError> {
		let size = payload.len();
		let limit = match self.config.version.uses_records() {
			true => self.config.max_record_size.into_inner(),
			false => MAX_FRAME_SIZE as u64,
		};
		if size as u64 > limit {
			return Err(self.oversized());
		}

		let mut frames = 0;
		loop {
			let before = payload.len();
			let Some(frame) = wire::next_frame(&mut payload)? else {
				break;
			};
			let frame_len = before - payload.len();
			// A PADDING run decodes as one frame but is a frame per byte on the wire
			frames += match frame {
				wire::Frame::Quic(frame::Frame::Padding) => frame_len,
				_ => 1,
			};
			if frames > 1 && !self.config.version.uses_records() {
				return Err(TransportError::FRAME_ENCODING_ERROR(
					"draft-00 message carries more than one frame",
				));
			}
			self.on_frame(frame, size, frame_len, now)?;
			if self.error.is_some() {
				// The peer closed; ignore the rest
				return Ok(());
			}
		}
		if frames == 0 {
			return Err(TransportError::FRAME_ENCODING_ERROR("record carries no frames"));
		}
		self.streams.queue_max_stream_id(&mut self.pending);
		Ok(())
	}

	/// The error for a record or draft-00 frame over its size limit
	fn oversized(&self) -> TransportError {
		match self.config.version.uses_records() {
			true => TransportError::PROTOCOL_VIOLATION("record exceeds max_record_size"),
			false => TransportError::FRAME_ENCODING_ERROR("frame exceeds max_frame_size"),
		}
	}

	/// End the connection over a violation, telling the peer with a CONNECTION_CLOSE
	fn fail(&mut self, err: TransportError) {
		debug!(%err, "qmux protocol error");
		self.close = Some(Close::from(err.clone()));
		self.lost(ConnectionError::TransportError(err));
	}

	fn on_frame(
		&mut self,
		frame: wire::Frame,
		payload_len: usize,
		frame_len: usize,
		now: Instant,
	) -> Result<(), TransportError> {
		let params = matches!(frame, wire::Frame::TransportParameters(_));
		let padding = matches!(frame, wire::Frame::Quic(frame::Frame::Padding));
		if params && self.peer.is_some() {
			return Err(TransportError::PROTOCOL_VIOLATION("duplicate QX_TRANSPORT_PARAMETERS"));
		}
		// Draft-02 requires the parameters first
		if self.config.version == Version::QMux02 && !params && !padding && self.peer.is_none() {
			return Err(TransportError::PROTOCOL_VIOLATION(
				"first frame was not QX_TRANSPORT_PARAMETERS",
			));
		}

		let frame = match frame {
			wire::Frame::TransportParameters(blob) => return self.on_params(blob, now),
			wire::Frame::PingRequest(_) | wire::Frame::PingResponse(_) if !self.config.version.uses_records() => {
				return Err(TransportError::FRAME_ENCODING_ERROR("QX_PING on draft-00"));
			}
			wire::Frame::PingRequest(seq) => {
				let strict = self.config.version == Version::QMux02;
				if strict
					&& self
						.greatest_ping_received
						.is_some_and(|greatest| seq.into_inner() <= greatest)
				{
					return Err(TransportError::PROTOCOL_VIOLATION(
						"QX_PING sequence number did not increase",
					));
				}
				self.greatest_ping_received = Some(seq.into_inner());
				self.ping_response = self.ping_response.max(Some(seq));
				return Ok(());
			}
			wire::Frame::PingResponse(seq) => {
				if self.config.version == Version::QMux02 && seq.into_inner() >= self.next_ping {
					return Err(TransportError::PROTOCOL_VIOLATION(
						"QX_PING response to a request never sent",
					));
				}
				self.timer.pong(seq.into_inner(), now);
				return Ok(());
			}
			wire::Frame::Continue(mut stream) => {
				if let Some(offset) = self.streams.rx_offset(stream.id) {
					stream.offset = offset;
				}
				return self.on_stream(stream, payload_len);
			}
			wire::Frame::Quic(frame) => frame,
		};

		match frame {
			frame::Frame::Padding => {}
			frame::Frame::Stream(stream) => {
				// The transport is ordered, so a gap can only be a peer bug
				if self
					.streams
					.rx_offset(stream.id)
					.is_some_and(|expected| stream.offset != expected)
				{
					return Err(TransportError::PROTOCOL_VIOLATION("stream data out of order"));
				}
				self.on_stream(stream, payload_len)?;
			}
			frame::Frame::ResetStream(reset) => {
				if reset.reliable_size.is_some() && self.config.version != Version::QMux02 {
					let mut err = TransportError::FRAME_ENCODING_ERROR("unsupported RESET_STREAM_AT");
					err.frame = Some(frame::FrameType::RESET_STREAM_AT);
					return Err(err);
				}
				if self.streams.received_reset(reset)?.should_transmit() {
					self.pending.max_data = true;
				}
			}
			frame::Frame::StopSending(frame::StopSending { id, error_code }) => {
				if let Some(reliable_size) = self.streams.received_stop_sending(id, error_code)? {
					self.pending.reset_stream.push((id, reliable_size));
				}
			}
			frame::Frame::MaxData(max) => self.streams.received_max_data(max),
			frame::Frame::MaxStreamData { id, offset } => {
				self.streams.received_max_stream_data(id, offset)?;
			}
			frame::Frame::MaxStreams { dir, count } => {
				self.streams.received_max_streams(dir, count)?;
			}
			frame::Frame::DataBlocked { offset } => {
				trace!(offset, "peer is blocked at the connection level");
			}
			frame::Frame::StreamDataBlocked { id, offset } => {
				if id.initiator() == self.side && id.dir() == Dir::Uni {
					return Err(TransportError::STREAM_STATE_ERROR(
						"STREAM_DATA_BLOCKED on send-only stream",
					));
				}
				trace!(stream = %id, offset, "peer is blocked at the stream level");
			}
			frame::Frame::StreamsBlocked { dir, limit } => {
				if limit > MAX_STREAM_COUNT {
					return Err(TransportError::FRAME_ENCODING_ERROR("unrepresentable stream limit"));
				}
				trace!(%dir, limit, "peer is blocked opening streams");
			}
			frame::Frame::Close(Close::Application(close)) => {
				self.lost(ConnectionError::ApplicationClosed(close));
			}
			frame::Frame::Close(Close::Connection(close)) => {
				self.lost(ConnectionError::ConnectionClosed(close));
			}
			frame::Frame::Datagram(datagram) => {
				let Some(max) = self.config.max_datagram_frame_size else {
					return Err(TransportError::PROTOCOL_VIOLATION("unexpected DATAGRAM frame"));
				};
				// The limit is on the frame as encoded, with or without its Length field
				if frame_len as u64 > max.into_inner() {
					return Err(TransportError::PROTOCOL_VIOLATION("oversized datagram"));
				}
				if self
					.datagrams
					.received(datagram, &Some(self.config.datagram_receive_buffer_size))?
				{
					self.events.push_back(Event::DatagramReceived);
				}
			}
			// ACK, PING, CRYPTO, NEW_TOKEN, connection ID and path management, ACK_FREQUENCY,
			// IMMEDIATE_ACK, and HANDSHAKE_DONE have no place on a reliable transport
			frame => {
				let mut err = TransportError::PROTOCOL_VIOLATION("frame prohibited in QMux");
				err.frame = Some(frame.ty());
				return Err(err);
			}
		}
		Ok(())
	}

	fn on_stream(&mut self, stream: frame::Stream, payload_len: usize) -> Result<(), TransportError> {
		if self.streams.received(stream, payload_len)?.should_transmit() {
			self.pending.max_data = true;
		}
		Ok(())
	}

	fn on_params(&mut self, blob: Bytes, now: Instant) -> Result<(), TransportError> {
		let mut peer = Params::decode(!self.side, self.config.version, blob)?;
		self.protocol = match (&self.config.protocols, &peer.protocols) {
			(Some(ours), theirs) => {
				let theirs = theirs.as_deref().unwrap_or_default();
				// The server's preference wins, as with ALPN
				let (server, client) = match self.side {
					Side::Server => (ours.as_slice(), theirs),
					Side::Client => (theirs, ours.as_slice()),
				};
				let agreed = server.iter().find(|p| client.contains(p)).cloned();
				// As with ALPN, asking for a protocol and agreeing on none fails the handshake
				if agreed.is_none() {
					return Err(TransportError::TRANSPORT_PARAMETER_ERROR(
						"no common application protocol",
					));
				}
				agreed
			}
			(None, Some(_)) => {
				return Err(TransportError::TRANSPORT_PARAMETER_ERROR(
					"unexpected application_protocols",
				));
			}
			(None, None) => None,
		};
		// RESET_STREAM_AT is a draft-02 extension
		peer.tp.reset_stream_at &= self.config.version == Version::QMux02;
		self.streams.set_params(&peer.tp);

		let idle = match self.config.version.uses_records() {
			true => super::negotiate_max_idle_timeout(
				self.config
					.idle_timeout
					.map(|timeout| VarInt::from_u64(timeout.as_millis() as u64).unwrap_or(VarInt::MAX)),
				Some(peer.tp.max_idle_timeout),
			),
			false => None,
		};
		self.timer.established(idle, self.config.rtt_interval, now);
		self.peer = Some(peer);
		self.events.push_back(Event::Connected);
		Ok(())
	}

	/// End the connection with `reason`, unless it already ended: the first close wins
	fn lost(&mut self, reason: ConnectionError) {
		if self.error.is_some() {
			return;
		}
		self.error = Some(reason.clone());
		self.conn_state = ConnectionState::Draining;
		self.events.push_back(Event::ConnectionLost { reason });
	}
}

impl std::fmt::Debug for Connection {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Connection")
			.field("side", &self.side)
			.field("version", &self.config.version)
			.field("established", &self.peer.is_some())
			.field("error", &self.error)
			.finish_non_exhaustive()
	}
}

/// Datagrams on a QMux connection, which the transport delivers reliably and in order
pub struct Datagrams<'a> {
	conn: &'a mut Connection,
}

impl Datagrams<'_> {
	/// Queue a datagram scheduled by `options`, like one of its group's streams
	///
	/// Sending never blocks: while queued datagrams overflow
	/// [`Config::datagram_send_buffer_size`], the oldest of the lowest-priority group is
	/// dropped.
	///
	/// # Panics
	/// - when `options.group` was opened on another connection
	pub fn send(&mut self, data: Bytes, options: SendOptions<'_>) -> Result<(), SendDatagramError> {
		if self.conn.config.max_datagram_frame_size.is_none() {
			return Err(SendDatagramError::Disabled);
		}
		let max = self.max_size().ok_or(SendDatagramError::UnsupportedByPeer)?;
		let budget = self.conn.config.datagram_send_buffer_size;
		let datagram = Datagram { data };
		if datagram.data.len() > max || datagram.size(true) > budget {
			return Err(SendDatagramError::TooLarge);
		}
		if !self.conn.streams.queue_datagram(datagram, options, budget) {
			trace!("dropping outgoing datagram");
		}
		Ok(())
	}

	/// The largest payload [`Self::send`] takes, or `None` when either side refuses datagrams
	/// or the peer's parameters have not arrived
	pub fn max_size(&self) -> Option<usize> {
		let peer = self.conn.peer.as_ref()?.tp.max_datagram_frame_size?;
		self.conn.config.max_datagram_frame_size?;
		let frame = peer.into_inner().min(self.conn.budget() as u64);
		Datagram::max_payload(frame).map(|payload| payload as usize)
	}

	/// Receive the next datagram
	pub fn recv(&mut self) -> Option<Bytes> {
		self.conn.datagrams.recv()
	}
}
