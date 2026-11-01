//! MoQ over one reliable, ordered WebRTC data channel, for sessions with browser peers.
//!
//! An [`Offerer`] or [`Answerer`] is one side of a str0m peer connection with a
//! full ICE agent: a dual-stack UDP socket, a host candidate per non-loopback
//! interface address, a server-reflexive candidate from each configured STUN
//! server, and remote `.local` candidates resolved over mDNS. Signaling is the
//! caller's: send the [`Offer`] or [`Answer`] and the local [`Candidate`]s to the
//! remote peer, and feed its replies back.
//!
//! The [`Offerer`] opens the data channel, labelled `moq` and carrying the agreed
//! MoQ ALPN as its protocol, and runs the MoQ client over it with
//! [`crate::Client::connect_webrtc`]. The [`Answerer`] adopts that channel and
//! runs the MoQ server with [`crate::Listener::accept_webrtc`]. This matches
//! `@moq/p2p`, where `Transport.connect` creates the channel and
//! `Transport.accept` adopts it.
//!
//! The channel carries qmux-01, one record per SCTP message, capped at 16 KiB and
//! the peer's SCTP message limit. Ordered delivery means one lost chunk stalls
//! every stream until SCTP retransmits it.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::str::FromStr;
use std::sync::Arc;

use bytes::Bytes;
use str0m::Rtc;
use str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};
use str0m::channel::{ChannelConfig, ChannelId, Reliability};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot};

mod channel;
mod resolver;

use resolver::Resolver;

/// The data channel label, as `@moq/p2p` creates it.
const LABEL: &str = "moq";

/// The largest qmux record, as `@moq/p2p` sends: SCTP flow control is per
/// message, so a smaller record keeps one stream from holding the association.
const RECORD_SIZE: u64 = 16 * 1024;

/// qmux-01 does not allow a peer to advertise a smaller record limit.
const MIN_RECORD_SIZE: u64 = 16_382;

/// SCTP's message limit when a description omits `a=max-message-size` (RFC 8841 section 6.1).
const DEFAULT_MAX_MESSAGE_SIZE: u64 = 64 * 1024;

/// Bytes the channel may hold unsent before qmux's writer waits, as `@moq/p2p` allows.
const SEND_WINDOW: usize = RECORD_SIZE as usize * 4;

/// Unread bytes the channel tolerates past qmux's advertised `receive_window`, for the
/// frame headers and control frames that flow control does not count. A peer that
/// honors flow control with reasonably full records stays under it; one that
/// ignores it has the channel closed instead of growing memory without limit.
const READ_SLACK: u64 = 16 * RECORD_SIZE;

/// The connection-level receive window a data channel session grants, qmux's default.
const RECEIVE_WINDOW: u64 = 1 << 20;

/// The default STUN port (RFC 8489 section 18.10).
const STUN_PORT: u16 = 3478;

/// Errors from a WebRTC peer connection or its data channel.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// Binding the UDP socket or resolving a STUN server failed.
	#[error(transparent)]
	Io(#[from] std::io::Error),

	/// The peer disconnected, or the connection was closed.
	#[error("peer connection closed")]
	Closed,

	/// ICE never connected before the establishment deadline.
	#[error("ICE did not connect before the establishment deadline")]
	Timeout,

	/// The WebRTC stack failed (DTLS, SCTP, or ICE state).
	#[error("rtc error: {0}")]
	Rtc(String),

	/// A session description failed to parse or apply.
	#[error("invalid session description: {0}")]
	Sdp(String),

	/// An ICE candidate failed to parse.
	#[error("invalid ICE candidate: {0}")]
	Candidate(String),

	/// An ICE server URL is malformed or not a `stun:` URL.
	#[error("unsupported ICE server: {0}")]
	Server(String),

	/// No interface has an address to offer as a host candidate.
	#[error("no network interface address to offer as a host candidate")]
	NoHost,

	/// The data channel was already taken by a session.
	#[error("the data channel is already in use")]
	Taken,

	/// The protocol is not a MoQ ALPN this endpoint speaks, or the channel's is not the one signaling agreed.
	#[error("unsupported MoQ protocol: {0:?}")]
	Protocol(String),

	/// The peer opened an unordered or partially reliable channel, which qmux cannot run over.
	#[error("the data channel must be reliable and ordered")]
	Unreliable,

	/// The peer's SCTP message limit is below qmux's minimum record size.
	#[error("SCTP message limit {0} is smaller than qmux's minimum record size")]
	MessageSize(u64),

	/// The qmux handshake over the channel failed.
	#[error("qmux handshake failed: {0}")]
	Qmux(String),
}

crate::error::from_message!(qmux::Error => Qmux);

impl Error {
	/// Adopt the agent's error. Not a `From` impl, which would put moq-ice in this crate's API.
	fn ice(err: moq_ice::Error) -> Self {
		match err {
			moq_ice::Error::Closed => Self::Closed,
			moq_ice::Error::Timeout => Self::Timeout,
			moq_ice::Error::Rtc(err) => Self::Rtc(err),
			err => Self::Rtc(err.to_string()),
		}
	}
}

/// How an [`Offerer`] or [`Answerer`] gathers its local candidates.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Config {
	/// STUN servers asked for this host's public address. Empty means LAN only:
	/// host candidates and nothing else.
	pub stun: Vec<Stun>,
}

/// A STUN server, written the way an `RTCIceServer` URL names one: `stun:host[:port]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stun {
	host: String,
	port: u16,
}

impl FromStr for Stun {
	type Err = Error;

	fn from_str(url: &str) -> Result<Self, Error> {
		let invalid = || Error::Server(url.to_string());
		let (scheme, rest) = url.split_once(':').ok_or_else(invalid)?;
		if !scheme.eq_ignore_ascii_case("stun") {
			// TURN is out of scope: the relay already carries what a TURN server would.
			return Err(invalid());
		}

		let (host, port) = match rest.strip_prefix('[') {
			Some(rest) => {
				let (host, port) = rest.split_once(']').ok_or_else(invalid)?;
				let port = match port {
					"" => None,
					port => Some(port.strip_prefix(':').ok_or_else(invalid)?),
				};
				(
					host.parse::<std::net::Ipv6Addr>().map_err(|_| invalid())?.to_string(),
					port,
				)
			}
			None => {
				let (host, port) = match rest.split_once(':') {
					Some((host, port)) => (host, Some(port)),
					None => (rest, None),
				};
				// A DNS name or IPv4 address, so a fragment or stray character fails here
				// instead of as a lookup that only warns.
				if host.is_empty()
					|| !host
						.chars()
						.all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'))
				{
					return Err(invalid());
				}
				(host.to_string(), port)
			}
		};
		let port = match port {
			Some(port) => port.parse().map_err(|_| invalid())?,
			None => STUN_PORT,
		};
		Ok(Self { host, port })
	}
}

impl std::fmt::Display for Stun {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self.host.contains(':') {
			true => write!(f, "stun:[{}]:{}", self.host, self.port),
			false => write!(f, "stun:{}:{}", self.host, self.port),
		}
	}
}

/// An SDP offer, as `RTCSessionDescription.sdp` carries it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offer(String);

/// An SDP answer, as `RTCSessionDescription.sdp` carries it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer(String);

/// An ICE candidate, as `RTCIceCandidate.candidate` carries it: `candidate:...`.
///
/// The connection has one media section, so a browser takes it with `sdpMLineIndex: 0`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate(String);

macro_rules! text {
	($ty:ident) => {
		impl $ty {
			/// Wrap the text a browser produced.
			pub fn new(text: impl Into<String>) -> Self {
				Self(text.into())
			}

			/// The text to hand a browser.
			pub fn as_str(&self) -> &str {
				&self.0
			}
		}

		impl std::fmt::Display for $ty {
			fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
				f.write_str(&self.0)
			}
		}
	};
}

text!(Offer);
text!(Answer);
text!(Candidate);

/// The offering side of a WebRTC peer connection, which opens the MoQ data channel
/// and runs the MoQ client over it with [`crate::Client::connect_webrtc`].
///
/// A background task drives the connection from creation, so ICE checks and STUN
/// replies are answered while signaling is still in progress. It runs until both
/// this handle and any session over its data channel are gone. Dropping the handle
/// stops signaling, including any remote `.local` candidates still resolving, but
/// leaves a running session alone.
pub struct Offerer {
	peer: Peer,
	/// The MoQ ALPN the channel carries.
	protocol: String,
}

impl Offerer {
	/// Start a peer connection with a data channel for MoQ `protocol`, the ALPN the
	/// peers agreed through signaling, returning the offer to send.
	pub async fn new(config: &Config, protocol: &str) -> Result<(Self, Offer), Error> {
		crate::runtime::scope(Self::with_hosts(config, protocol, hosts()?)).await
	}

	async fn with_hosts(config: &Config, protocol: &str, hosts: Vec<IpAddr>) -> Result<(Self, Offer), Error> {
		let protocol = negotiated(protocol)?;
		let socket = bind(&hosts)?;
		let mut rtc = rtc(socket.locals())?;

		let mut api = rtc.sdp_api();
		let id = api.add_channel_with_config(ChannelConfig {
			label: LABEL.to_string(),
			ordered: true,
			reliability: Reliability::Reliable,
			negotiated: None,
			protocol: protocol.to_string(),
		});
		let (offer, pending) = api.apply().expect("adding a channel is an SDP change");

		let role = Role::Offerer {
			channel: id,
			pending: Some(pending),
		};
		let peer = Peer::spawn(config, rtc, socket, role, None).await?;
		let offerer = Self { peer, protocol };
		Ok((offerer, Offer(offer.to_sdp_string())))
	}

	/// Apply the remote peer's answer to this offer.
	pub async fn accept(&self, answer: &Answer) -> Result<(), Error> {
		let (sdp, mdns) = split_mdns(answer.as_str())?;
		let max_message = max_message_size(answer.as_str())?;
		let parsed = SdpAnswer::from_sdp_string(&sdp).map_err(|err| Error::Sdp(err.to_string()))?;

		let (tx, rx) = oneshot::channel();
		self.peer.send(Command::Answer(parsed, max_message, tx)).await?;
		rx.await.map_err(|_| Error::Closed)??;
		self.peer.resolver.add(mdns).await
	}

	/// The next local candidate to send the remote peer, or `None` once gathering is done.
	///
	/// Host candidates ride in the offer; these are the server-reflexive ones that
	/// STUN finds afterwards.
	pub async fn candidate(&mut self) -> Option<Candidate> {
		self.peer.candidates.recv().await
	}

	/// Add a candidate the remote peer sent.
	///
	/// This waits while the connection is still taking earlier candidates. A `.local`
	/// address is resolved over mDNS first, one name at a time, so this also waits
	/// while earlier names are still resolving. One that does not resolve is
	/// dropped. An empty candidate, which a browser sends to mark the end of its
	/// candidates, is ignored.
	pub async fn add_candidate(&self, candidate: &Candidate) -> Result<(), Error> {
		self.peer.add_candidate(candidate).await
	}

	/// The MoQ ALPN this peer's data channel carries.
	pub(crate) fn protocol(&self) -> &str {
		&self.protocol
	}

	/// Wait for the data channel to open, and take it.
	pub(crate) async fn channel(&self) -> Result<Opened, Error> {
		self.peer.channel().await
	}
}

/// The answering side of a WebRTC peer connection, which adopts the remote peer's
/// MoQ data channel and serves it with [`crate::Listener::accept_webrtc`].
///
/// Like [`Offerer`], a background task drives the connection until both this
/// handle and any session over its data channel are gone.
pub struct Answerer {
	peer: Peer,
	/// The MoQ ALPN the remote peer's channel must carry.
	protocol: String,
}

impl Answerer {
	/// Start a peer connection for the remote peer's offer and MoQ `protocol`, the
	/// ALPN the peers agreed through signaling, returning the answer to send.
	pub async fn new(config: &Config, offer: &Offer, protocol: &str) -> Result<(Self, Answer), Error> {
		crate::runtime::scope(Self::with_hosts(config, offer, protocol, hosts()?)).await
	}

	async fn with_hosts(
		config: &Config,
		offer: &Offer,
		protocol: &str,
		hosts: Vec<IpAddr>,
	) -> Result<(Self, Answer), Error> {
		let protocol = negotiated(protocol)?;
		let (sdp, mdns) = split_mdns(offer.as_str())?;
		let max_message = max_message_size(offer.as_str())?;
		let parsed = SdpOffer::from_sdp_string(&sdp).map_err(|err| Error::Sdp(err.to_string()))?;

		let socket = bind(&hosts)?;
		let mut rtc = rtc(socket.locals())?;
		let answer = rtc
			.sdp_api()
			.accept_offer(parsed)
			.map_err(|err| Error::Sdp(err.to_string()))?;

		let peer = Peer::spawn(config, rtc, socket, Role::Answerer, Some(max_message)).await?;
		peer.resolver.add(mdns).await?;
		Ok((Self { peer, protocol }, Answer(answer.to_sdp_string())))
	}

	/// The next local candidate to send the remote peer, or `None` once gathering is done.
	///
	/// Host candidates ride in the answer; these are the server-reflexive ones that
	/// STUN finds afterwards.
	pub async fn candidate(&mut self) -> Option<Candidate> {
		self.peer.candidates.recv().await
	}

	/// Add a candidate the remote peer sent.
	///
	/// This waits while the connection is still taking earlier candidates. A `.local`
	/// address is resolved over mDNS first, one name at a time, so this also waits
	/// while earlier names are still resolving. One that does not resolve is
	/// dropped. An empty candidate, which a browser sends to mark the end of its
	/// candidates, is ignored.
	pub async fn add_candidate(&self, candidate: &Candidate) -> Result<(), Error> {
		self.peer.add_candidate(candidate).await
	}

	/// The MoQ ALPN the remote peer's data channel must carry.
	pub(crate) fn protocol(&self) -> &str {
		&self.protocol
	}

	/// Wait for the remote peer's data channel to open, and take it.
	pub(crate) async fn channel(&self) -> Result<Opened, Error> {
		self.peer.channel().await
	}
}

/// Check a MoQ ALPN agreed through signaling, as `@moq/p2p` does.
fn negotiated(protocol: &str) -> Result<String, Error> {
	match protocol.is_empty() || protocol.contains(char::is_whitespace) {
		true => Err(Error::Protocol(protocol.to_string())),
		false => Ok(protocol.to_string()),
	}
}

/// The signaling handle both roles share: commands to the driver task, the
/// candidates it gathers, and the resolver for the remote peer's `.local` ones.
///
/// The driver takes one command at a time, so a peer that trickles candidates
/// faster than the connection applies them holds up its own signaling instead
/// of queueing them.
struct Peer {
	commands: mpsc::Sender<Command>,
	candidates: mpsc::UnboundedReceiver<Candidate>,
	resolver: Resolver,
}

impl Peer {
	async fn spawn(
		config: &Config,
		rtc: Rtc,
		socket: moq_ice::Socket,
		role: Role,
		max_message: Option<u64>,
	) -> Result<Self, Error> {
		let stun = resolve_stun(&config.stun).await;
		let mut agent = moq_ice::Agent::new(rtc, socket)?.with_stun(&stun);
		if let Role::Offerer { .. } = role {
			// The answer applies through `rtc_mut`, which starts the deadline.
			agent = agent.awaiting_answer();
		}

		let (commands, rx) = mpsc::channel(1);
		let (candidates_tx, candidates) = mpsc::unbounded_channel();
		let driver = Driver {
			agent,
			commands: rx,
			candidates: Some(candidates_tx),
			role,
			max_message,
			channel: None,
			waiter: None,
			taken: false,
		};
		use tracing::Instrument;
		crate::runtime::spawn(driver.run().instrument(tracing::Span::current()));

		let resolver = Resolver::spawn(commands.clone(), resolver::Mdns);
		Ok(Self {
			commands,
			candidates,
			resolver,
		})
	}

	async fn add_candidate(&self, candidate: &Candidate) -> Result<(), Error> {
		let line = candidate.as_str().trim();
		let line = line.strip_prefix("a=").unwrap_or(line);
		if line.is_empty() {
			return Ok(());
		}
		if mdns_host(line)?.is_some() {
			return self.resolver.add(vec![line.to_string()]).await;
		}
		let candidate = str0m::Candidate::from_sdp_string(line).map_err(|err| Error::Candidate(err.to_string()))?;
		self.send(Command::Candidate(candidate)).await
	}

	async fn channel(&self) -> Result<Opened, Error> {
		let (tx, rx) = oneshot::channel();
		self.send(Command::Channel(tx)).await?;
		let mut opened = rx.await.map_err(|_| Error::Closed)??;
		// The session keeps the driver alive once this handle is gone.
		opened.channel.writer._driver = Some(self.commands.clone());
		Ok(opened)
	}

	async fn send(&self, command: Command) -> Result<(), Error> {
		self.commands.send(command).await.map_err(|_| Error::Closed)
	}
}

/// A data channel that opened, ready to carry qmux.
pub(crate) struct Opened {
	pub channel: channel::Channel,
	/// The MoQ ALPN the channel carries.
	pub protocol: String,
	/// The qmux session settings for this channel.
	pub config: qmux::Config,
}

/// The qmux session settings for a channel carrying `protocol`, advertising
/// `max_record_size`: 16 KiB, capped by the peer's SCTP message limit.
fn session_config(protocol: &str, max_record_size: u64) -> qmux::Config {
	let mut config = crate::qmux_config();
	config.version = qmux::Version::QMux01;
	config.protocol = qmux::Protocol::Negotiated(protocol.to_string());
	config.max_record_size = max_record_size;
	// Set rather than left to qmux's default, since the read queue is sized from it.
	config.limits.receive_window = std::num::NonZeroU64::new(RECEIVE_WINDOW);
	// The binding has no datagrams.
	config.max_datagram_frame_size = None;
	config
}

pub(crate) enum Command {
	Answer(SdpAnswer, u64, oneshot::Sender<Result<(), Error>>),
	Candidate(str0m::Candidate),
	Channel(oneshot::Sender<Result<Opened, Error>>),
}

enum Role {
	Offerer {
		channel: ChannelId,
		/// Until the answer is applied.
		pending: Option<SdpPendingOffer>,
	},
	Answerer,
}

/// The open data channel, from the driver's side.
struct Open {
	id: ChannelId,
	protocol: String,
	reads: channel::Inbox,
	/// `None` once the session's writer closed.
	writes: Option<mpsc::Receiver<Bytes>>,
	config: qmux::Config,
	/// The session's halves, until a session takes them.
	halves: Option<channel::Channel>,
}

/// Owns the connection and runs it on its own task.
struct Driver {
	agent: moq_ice::Agent,
	commands: mpsc::Receiver<Command>,
	/// Dropped once gathering finishes, ending [`Offerer::candidate`] and [`Answerer::candidate`].
	candidates: Option<mpsc::UnboundedSender<Candidate>>,
	role: Role,
	/// The remote peer's SCTP message limit, once its description is applied.
	max_message: Option<u64>,
	channel: Option<Result<Open, Error>>,
	/// The session waiting for the channel to open.
	waiter: Option<oneshot::Sender<Result<Opened, Error>>>,
	/// Whether a session already asked for the channel, which only one may carry.
	taken: bool,
}

impl Driver {
	async fn run(mut self) {
		let err = loop {
			let writable = self.writable();
			let writes = match &mut self.channel {
				Some(Ok(Open {
					writes: Some(writes), ..
				})) => Some(writes),
				_ => None,
			};

			tokio::select! {
				event = self.agent.next() => match event {
					Ok(event) => if let Err(err) = self.event(event) {
						break err;
					},
					Err(err) => break Error::ice(err),
				},
				command = self.commands.recv() => match command {
					Some(command) => self.command(command),
					// Neither the signaling handle nor a session is left to use the connection.
					None => return,
				},
				data = next_write(writes), if writable => {
					if let Err(err) = self.write(data) {
						break err;
					}
				}
			}
		};

		match &err {
			Error::Closed => tracing::debug!("peer connection closed"),
			err => tracing::debug!(%err, "peer connection failed"),
		}
		if let Some(waiter) = self.waiter.take() {
			let _ = waiter.send(Err(err));
		}
	}

	/// Whether the channel can take another record without passing [`SEND_WINDOW`].
	fn writable(&mut self) -> bool {
		let Some(Ok(Open {
			id, writes: Some(_), ..
		})) = &self.channel
		else {
			return false;
		};
		let id = *id;
		self.agent
			.rtc_mut()
			.channel(id)
			.is_some_and(|mut channel| channel.buffered_amount() < SEND_WINDOW)
	}

	/// Hand a record from the session to SCTP, or close the channel when the session's writer closed.
	fn write(&mut self, data: Option<Bytes>) -> Result<(), Error> {
		let Some(Ok(open)) = &mut self.channel else {
			return Ok(());
		};
		let id = open.id;
		let Some(data) = data else {
			open.writes = None;
			self.agent.rtc_mut().direct_api().close_data_channel(id);
			return Ok(());
		};
		let written = {
			let mut rtc = self.agent.rtc_mut();
			let mut channel = rtc.channel(id).ok_or(Error::Closed)?;
			channel.write(true, &data).map_err(|err| Error::Rtc(err.to_string()))?
		};
		// The send window keeps the buffer well below str0m's, so a refusal is a bug, not backpressure.
		match written {
			true => Ok(()),
			false => Err(Error::Rtc("SCTP refused a record within the send window".into())),
		}
	}

	fn command(&mut self, command: Command) {
		match command {
			Command::Answer(answer, max_message, reply) => {
				// Only an `Offerer` sends an answer, so the answering role never has one pending.
				let pending = match &mut self.role {
					Role::Offerer { pending, .. } => pending.take(),
					Role::Answerer => None,
				};
				let result = match pending {
					Some(pending) => {
						let accepted = self.agent.rtc_mut().sdp_api().accept_answer(pending, answer);
						accepted
							.map(|()| self.max_message = Some(max_message))
							.map_err(|err| Error::Sdp(err.to_string()))
					}
					None => Err(Error::Sdp("the answer was already applied".into())),
				};
				let _ = reply.send(result);
			}
			Command::Candidate(candidate) => self.agent.rtc_mut().add_remote_candidate(candidate),
			Command::Channel(reply) => {
				if self.taken {
					let _ = reply.send(Err(Error::Taken));
					return;
				}
				self.taken = true;
				self.waiter = Some(reply);
				self.deliver();
			}
		}
	}

	fn event(&mut self, event: moq_ice::Event) -> Result<(), Error> {
		let event = match event {
			moq_ice::Event::Candidate(candidate) => {
				if let Some(candidates) = &self.candidates {
					let _ = candidates.send(Candidate(candidate.to_sdp_string()));
				}
				return Ok(());
			}
			moq_ice::Event::Gathered => {
				self.candidates = None;
				return Ok(());
			}
			moq_ice::Event::Rtc(event) => event,
			_ => return Ok(()),
		};

		match event {
			str0m::Event::ChannelOpen(id, _) => self.open(id),
			str0m::Event::ChannelData(data) => {
				let Some(Ok(open)) = &self.channel else {
					return Ok(());
				};
				if data.id != open.id {
					return Ok(());
				}
				let record = if !data.binary {
					Err(qmux::Error::ProtocolViolation)
				} else if data.data.len() as u64 > open.config.max_record_size {
					Err(qmux::Error::FrameTooLarge)
				} else {
					Ok(Bytes::from(data.data))
				};
				if !open.reads.push(record) {
					tracing::debug!("closing the data channel: the peer broke qmux framing or flow control");
					let id = open.id;
					self.agent.rtc_mut().direct_api().close_data_channel(id);
					self.channel = Some(Err(Error::Closed));
				}
			}
			str0m::Event::ChannelClose(id) => {
				if let Some(Ok(open)) = &self.channel
					&& open.id == id
				{
					// The session's reader sees the end of its records.
					self.channel = Some(Err(Error::Closed));
				}
			}
			_ => {}
		}
		Ok(())
	}

	/// Take a channel that opened as the session's, if it is the one this role expects.
	fn open(&mut self, id: ChannelId) {
		let expected = match &self.role {
			Role::Offerer { channel, .. } => *channel == id,
			// The remote peer may open other channels first; only the `moq` one is the session's.
			Role::Answerer => self
				.agent
				.rtc_mut()
				.channel(id)
				.and_then(|channel| channel.config().map(|config| config.label == LABEL))
				.unwrap_or(false),
		};
		if !expected || self.channel.is_some() {
			tracing::debug!(?id, "closing an unexpected data channel");
			self.agent.rtc_mut().direct_api().close_data_channel(id);
			return;
		}

		let opened = self.adopt(id);
		if opened.is_err() {
			self.agent.rtc_mut().direct_api().close_data_channel(id);
		}
		self.channel = Some(opened);
		self.deliver();
	}

	fn adopt(&mut self, id: ChannelId) -> Result<Open, Error> {
		let max_message = self.max_message.unwrap_or(DEFAULT_MAX_MESSAGE_SIZE);
		let max_record_size = RECORD_SIZE.min(max_message);
		if max_record_size < MIN_RECORD_SIZE {
			return Err(Error::MessageSize(max_message));
		}

		let protocol = {
			let mut rtc = self.agent.rtc_mut();
			let mut channel = rtc.channel(id).ok_or(Error::Closed)?;
			let config = channel.config().ok_or(Error::Unreliable)?;
			if !config.ordered || config.reliability != Reliability::Reliable {
				return Err(Error::Unreliable);
			}
			let protocol = config.protocol.clone();
			channel.set_buffered_amount_low_threshold(SEND_WINDOW / 2);
			protocol
		};

		let config = session_config(&protocol, max_record_size);
		let (reads, reader) = channel::queue(RECEIVE_WINDOW + READ_SLACK);
		let (tx, writes) = mpsc::channel(1);
		Ok(Open {
			id,
			protocol,
			reads,
			writes: Some(writes),
			config,
			halves: Some(channel::Channel {
				writer: channel::Writer {
					tx: Some(tx),
					max: usize::try_from(max_message).unwrap_or(usize::MAX),
					_driver: None,
				},
				reader,
			}),
		})
	}

	/// Answer a waiting session once the channel opened or failed to.
	fn deliver(&mut self) {
		let Some(channel) = &mut self.channel else {
			return;
		};
		let Some(waiter) = self.waiter.take() else {
			return;
		};
		let result = match channel {
			Ok(open) => Ok(Opened {
				channel: open.halves.take().expect("only the first session takes the halves"),
				protocol: open.protocol.clone(),
				config: open.config.clone(),
			}),
			Err(err) => Err(match err {
				Error::Unreliable => Error::Unreliable,
				Error::MessageSize(size) => Error::MessageSize(*size),
				_ => Error::Closed,
			}),
		};
		let _ = waiter.send(result);
	}
}

/// The session's next record, or `None` when its writer closed. Pends without a channel.
async fn next_write(writes: Option<&mut mpsc::Receiver<Bytes>>) -> Option<Bytes> {
	match writes {
		Some(writes) => writes.recv().await,
		None => std::future::pending().await,
	}
}

/// A str0m connection with this socket's host candidates.
fn rtc(locals: &[SocketAddr]) -> Result<Rtc, Error> {
	let mut rtc = Rtc::builder().build(kio::clock::now().into_std());
	for &addr in locals {
		let candidate = str0m::Candidate::host(addr, "udp").map_err(|err| Error::Candidate(err.to_string()))?;
		rtc.add_local_candidate(candidate);
	}
	Ok(rtc)
}

/// Every address worth offering as a host candidate: not loopback, and not
/// link-local, which IPv6 cannot use without a scope and str0m refuses for IPv4.
fn hosts() -> Result<Vec<IpAddr>, Error> {
	let mut hosts = Vec::new();
	for interface in if_addrs::get_if_addrs()? {
		let ip = interface.ip();
		if interface.is_loopback() || interface.is_link_local() || ip.is_multicast() || hosts.contains(&ip) {
			continue;
		}
		hosts.push(ip);
	}
	Ok(hosts)
}

/// Bind a dual-stack socket on an ephemeral port, pairing it with each host
/// address, and start reading it.
fn bind(hosts: &[IpAddr]) -> Result<moq_ice::Socket, Error> {
	let socket = crate::bind::udp(crate::bind::Udp::new("[::]:0".parse().unwrap()))
		.or_else(|_| crate::bind::udp(crate::bind::Udp::new("0.0.0.0:0".parse().unwrap())))?;
	socket.set_nonblocking(true)?;
	// A `[::]` socket that stayed v6-only can't send to IPv4 candidates.
	let ipv4 = socket.local_addr()?.is_ipv4() || crate::bind::udp_is_dual_stack(&socket);
	let socket = UdpSocket::from_std(socket)?;
	let local = socket.local_addr()?;

	let locals: Vec<SocketAddr> = hosts
		.iter()
		.filter(|ip| if ip.is_ipv4() { ipv4 } else { local.is_ipv6() })
		.map(|&ip| SocketAddr::new(ip, local.port()))
		.collect();
	if locals.is_empty() {
		return Err(Error::NoHost);
	}
	let udp = Arc::new(socket);
	Ok(moq_ice::Socket::new(udp.clone(), locals, moq_ice::reader(udp))?)
}

/// Look up each STUN server. One that does not resolve is skipped, since a missing
/// reflexive candidate only costs the paths that needed it.
async fn resolve_stun(servers: &[Stun]) -> Vec<SocketAddr> {
	let mut addrs = Vec::new();
	for server in servers {
		match tokio::net::lookup_host((server.host.as_str(), server.port)).await {
			Ok(found) => addrs.extend(found),
			Err(err) => tracing::warn!(%err, %server, "failed to resolve STUN server"),
		}
	}
	addrs
}

/// The SCTP message limit a description advertises, where `0` means unlimited.
fn max_message_size(sdp: &str) -> Result<u64, Error> {
	let Some(value) = sdp
		.lines()
		.find_map(|line| line.trim().strip_prefix("a=max-message-size:"))
	else {
		return Ok(DEFAULT_MAX_MESSAGE_SIZE);
	};
	match value.trim().parse::<u64>() {
		Ok(0) => Ok(u64::MAX),
		Ok(size) => Ok(size),
		Err(_) => Err(Error::Sdp(format!("invalid max-message-size: {value}"))),
	}
}

/// Split a description's `.local` candidates out, since str0m only parses IP addresses.
fn split_mdns(sdp: &str) -> Result<(String, Vec<String>), Error> {
	let mut kept = String::with_capacity(sdp.len());
	let mut mdns = Vec::new();
	for line in sdp.split_inclusive('\n') {
		if let Some(candidate) = line.trim_end().strip_prefix("a=")
			&& candidate.starts_with("candidate:")
			&& mdns_host(candidate)?.is_some()
		{
			mdns.push(candidate.to_string());
			continue;
		}
		kept.push_str(line);
	}
	Ok((kept, mdns))
}

/// The `.local` host name a `candidate:` line connects to, if any. The rest of the
/// line is checked now, so a malformed candidate fails before it costs a query.
fn mdns_host(line: &str) -> Result<Option<&str>, Error> {
	let Some(host) = candidate_address(line).filter(|host| host.ends_with(".local")) else {
		return Ok(None);
	};
	with_address(line, IpAddr::V4(Ipv4Addr::UNSPECIFIED))?;
	Ok(Some(host))
}

/// The connection address of a `candidate:` line (RFC 8839 section 5.1).
fn candidate_address(line: &str) -> Option<&str> {
	line.split_whitespace().nth(4)
}

/// Parse a `candidate:` line with `ip` as its connection address.
fn with_address(line: &str, ip: IpAddr) -> Result<str0m::Candidate, Error> {
	let ip = ip.to_string();
	let mut fields: Vec<&str> = line.split_whitespace().collect();
	let Some(addr) = fields.get_mut(4) else {
		return Err(Error::Candidate(line.to_string()));
	};
	*addr = &ip;
	str0m::Candidate::from_sdp_string(&fields.join(" ")).map_err(|err| Error::Candidate(err.to_string()))
}

#[cfg(test)]
mod tests;
