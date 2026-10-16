//! MoQ over one reliable, ordered WebRTC data channel, for sessions with browser peers.
//!
//! A [`Peer`] is one str0m peer connection with a full ICE agent: a dual-stack UDP
//! socket, a host candidate per non-loopback interface address, a server-reflexive
//! candidate from each configured STUN server, and remote `.local` candidates
//! resolved over mDNS. Signaling is the caller's: send the [`Offer`] or [`Answer`]
//! and the local [`Candidate`]s to the remote peer, and feed its replies back.
//!
//! The offering peer opens the data channel, labelled `moq` and carrying the
//! agreed MoQ ALPN as its protocol, and runs the MoQ client over it with
//! [`crate::Client::connect_webrtc`]. The answering peer adopts that channel and
//! runs the MoQ server with [`crate::Listener::accept_webrtc`]. This matches
//! `@moq/p2p`, where `Transport.connect` creates the channel and
//! `Transport.accept` adopts it.
//!
//! The channel carries qmux-01, one record per SCTP message, capped at 16 KiB and
//! the peer's SCTP message limit. Ordered delivery means one lost chunk stalls
//! every stream until SCTP retransmits it.

use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use str0m::Rtc;
use str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};
use str0m::channel::{ChannelConfig, ChannelId, Reliability};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot};

mod channel;
pub mod ice;

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

/// How long to wait for a `.local` name to resolve: mdns-sd's first query plus its
/// retransmissions at one and two seconds (RFC 6762 section 5.2).
const MDNS_TIMEOUT: Duration = Duration::from_secs(5);

/// The default STUN port (RFC 8489 section 18.10).
const STUN_PORT: u16 = 3478;

/// Errors from a WebRTC peer connection or its data channel.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// Binding the UDP socket or resolving a STUN server failed.
	#[error(transparent)]
	Io(#[from] std::io::Error),

	/// The peer connection ended.
	#[error(transparent)]
	Ice(#[from] ice::Error),

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

	/// A `.local` candidate could not be resolved.
	#[error("mDNS resolution failed: {0}")]
	Mdns(String),

	/// The call needs the other role: only the offering peer opens the data channel
	/// and applies an answer, and only the answering peer adopts a channel.
	#[error("wrong role: {0}")]
	Role(&'static str),

	/// The data channel was already taken by a session.
	#[error("the data channel is already in use")]
	Taken,

	/// The channel's protocol is not a MoQ ALPN this endpoint speaks.
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

/// How a [`Peer`] gathers its local candidates.
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
				if host.is_empty() || host.contains(['/', '?', '@', '[', ']']) {
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

/// One side of a WebRTC peer connection, for signaling.
///
/// A background task drives the connection from creation, so ICE checks and STUN
/// replies are answered while signaling is still in progress. It runs until both
/// this handle and any session over its data channel are gone. Dropping the handle
/// stops signaling but leaves a running session alone.
pub struct Peer {
	commands: mpsc::UnboundedSender<Command>,
	candidates: mpsc::UnboundedReceiver<Candidate>,
	/// The ALPN the offerer's channel carries, or `None` for the answering peer.
	protocol: Option<String>,
}

impl Peer {
	/// Start the offering side, opening a data channel for MoQ `protocol`, the ALPN
	/// the peers agreed through signaling.
	pub async fn offer(config: &Config, protocol: &str) -> Result<(Self, Offer), Error> {
		Self::offer_from(config, protocol, hosts()?).await
	}

	async fn offer_from(config: &Config, protocol: &str, hosts: Vec<IpAddr>) -> Result<(Self, Offer), Error> {
		if protocol.is_empty() || protocol.contains(char::is_whitespace) {
			return Err(Error::Protocol(protocol.to_string()));
		}
		let (socket, locals) = bind(&hosts)?;
		let mut rtc = rtc(&locals)?;

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
		let peer = Self::spawn(config, rtc, socket, locals, role, None).await?;
		Ok((
			Self {
				protocol: Some(protocol.to_string()),
				..peer
			},
			Offer(offer.to_sdp_string()),
		))
	}

	/// Start the answering side for a browser's offer.
	pub async fn answer(config: &Config, offer: &Offer) -> Result<(Self, Answer), Error> {
		Self::answer_from(config, offer, hosts()?).await
	}

	async fn answer_from(config: &Config, offer: &Offer, hosts: Vec<IpAddr>) -> Result<(Self, Answer), Error> {
		let (sdp, mdns) = split_mdns(offer.as_str());
		let max_message = max_message_size(offer.as_str())?;
		let parsed = SdpOffer::from_sdp_string(&sdp).map_err(|err| Error::Sdp(err.to_string()))?;

		let (socket, locals) = bind(&hosts)?;
		let mut rtc = rtc(&locals)?;
		let answer = rtc
			.sdp_api()
			.accept_offer(parsed)
			.map_err(|err| Error::Sdp(err.to_string()))?;

		let peer = Self::spawn(config, rtc, socket, locals, Role::Answerer, Some(max_message)).await?;
		peer.resolve(mdns);
		Ok((peer, Answer(answer.to_sdp_string())))
	}

	async fn spawn(
		config: &Config,
		rtc: Rtc,
		socket: Arc<UdpSocket>,
		locals: Vec<SocketAddr>,
		role: Role,
		max_message: Option<u64>,
	) -> Result<Self, Error> {
		let stun = resolve_stun(&config.stun).await;
		let inbound = ice::reader(socket.clone());
		let agent = ice::Agent::new(rtc, socket, locals, inbound)?.with_stun(&stun);

		let (commands, rx) = mpsc::unbounded_channel();
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
		tokio::spawn(driver.run().instrument(tracing::Span::current()));

		Ok(Self {
			commands,
			candidates,
			protocol: None,
		})
	}

	/// Apply the browser's answer to this peer's offer.
	pub async fn accept(&self, answer: &Answer) -> Result<(), Error> {
		if self.protocol.is_none() {
			return Err(Error::Role("only the offering peer applies an answer"));
		}
		let (sdp, mdns) = split_mdns(answer.as_str());
		let max_message = max_message_size(answer.as_str())?;
		let parsed = SdpAnswer::from_sdp_string(&sdp).map_err(|err| Error::Sdp(err.to_string()))?;

		let (tx, rx) = oneshot::channel();
		self.send(Command::Answer(parsed, max_message, tx))?;
		rx.await.map_err(|_| ice::Error::Closed)??;
		self.resolve(mdns);
		Ok(())
	}

	/// The next local candidate to send the remote peer, or `None` once gathering is done.
	///
	/// Host candidates ride in the description; these are the server-reflexive ones
	/// that STUN finds afterwards.
	pub async fn candidate(&mut self) -> Option<Candidate> {
		self.candidates.recv().await
	}

	/// Add a candidate the remote peer sent, resolving a `.local` address over mDNS first.
	///
	/// An empty candidate, which a browser sends to mark the end of its candidates, is ignored.
	pub async fn add_candidate(&self, candidate: &Candidate) -> Result<(), Error> {
		let line = candidate.as_str().trim();
		let line = line.strip_prefix("a=").unwrap_or(line);
		if line.is_empty() {
			return Ok(());
		}
		let candidate = parse_candidate(line).await?;
		self.send(Command::Candidate(candidate))
	}

	/// The MoQ ALPN this peer's data channel carries, for the offering side.
	pub(crate) fn protocol(&self) -> Result<&str, Error> {
		self.protocol
			.as_deref()
			.ok_or(Error::Role("only the offering peer opens the data channel"))
	}

	/// Whether this is the answering side, which adopts the remote peer's channel.
	pub(crate) fn answering(&self) -> bool {
		self.protocol.is_none()
	}

	/// Wait for the data channel to open, and take it.
	pub(crate) async fn channel(&self) -> Result<Opened, Error> {
		let (tx, rx) = oneshot::channel();
		self.send(Command::Channel(tx))?;
		let mut opened = rx.await.map_err(|_| ice::Error::Closed)??;
		// The session keeps the driver alive once this handle is gone.
		opened.channel.writer._driver = Some(self.commands.clone());
		Ok(opened)
	}

	fn send(&self, command: Command) -> Result<(), Error> {
		self.commands.send(command).map_err(|_| ice::Error::Closed.into())
	}

	/// Add the `.local` candidates a description carried once mDNS resolves them.
	fn resolve(&self, lines: Vec<String>) {
		for line in lines {
			let commands = self.commands.clone();
			tokio::spawn(async move {
				match parse_candidate(&line).await {
					Ok(candidate) => {
						let _ = commands.send(Command::Candidate(candidate));
					}
					Err(err) => tracing::debug!(%err, "dropped a remote candidate"),
				}
			});
		}
	}
}

/// A data channel that opened, ready to carry qmux.
pub(crate) struct Opened {
	pub channel: channel::Channel,
	/// The MoQ ALPN the channel carries.
	pub protocol: String,
	/// The record size to advertise: 16 KiB, capped by the peer's SCTP message limit.
	pub max_record_size: u64,
}

impl Opened {
	/// The qmux session settings for this channel.
	pub fn config(&self) -> qmux::Config {
		let mut config = crate::qmux_config();
		config.version = qmux::Version::QMux01;
		config.protocol = qmux::Protocol::Negotiated(self.protocol.clone());
		config.max_record_size = self.max_record_size;
		// The binding has no datagrams.
		config.max_datagram_frame_size = 0;
		config
	}
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
	reads: mpsc::UnboundedSender<Result<Bytes, qmux::Error>>,
	/// `None` once the session's writer closed.
	writes: Option<mpsc::Receiver<Bytes>>,
	max_record_size: u64,
	/// The session's halves, until a session takes them.
	halves: Option<channel::Channel>,
}

/// Owns the connection and runs it on its own task.
struct Driver {
	agent: ice::Agent,
	commands: mpsc::UnboundedReceiver<Command>,
	/// Dropped once gathering finishes, ending [`Peer::candidate`].
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
					Err(err) => break err.into(),
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
			Error::Ice(ice::Error::Closed) => tracing::debug!("peer connection closed"),
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
			.rtc()
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
			self.agent.rtc().direct_api().close_data_channel(id);
			return Ok(());
		};
		let Some(mut channel) = self.agent.rtc().channel(id) else {
			return Err(ice::Error::Closed.into());
		};
		// The send window keeps the buffer well below str0m's, so a refusal is a bug, not backpressure.
		match channel.write(true, &data) {
			Ok(true) => Ok(()),
			Ok(false) => Err(ice::Error::Rtc("SCTP refused a record within the send window".into()).into()),
			Err(err) => Err(ice::Error::Rtc(err.to_string()).into()),
		}
	}

	fn command(&mut self, command: Command) {
		match command {
			Command::Answer(answer, max_message, reply) => {
				let result = match &mut self.role {
					Role::Offerer { pending, .. } => match pending.take() {
						Some(pending) => self
							.agent
							.rtc()
							.sdp_api()
							.accept_answer(pending, answer)
							.map(|()| self.max_message = Some(max_message))
							.map_err(|err| Error::Sdp(err.to_string())),
						None => Err(Error::Sdp("the answer was already applied".into())),
					},
					Role::Answerer => Err(Error::Role("only the offering peer applies an answer")),
				};
				let _ = reply.send(result);
			}
			Command::Candidate(candidate) => self.agent.rtc().add_remote_candidate(candidate),
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

	fn event(&mut self, event: ice::Event) -> Result<(), Error> {
		let event = match event {
			ice::Event::Candidate(candidate) => {
				if let Some(candidates) = &self.candidates {
					let _ = candidates.send(Candidate(candidate.to_sdp_string()));
				}
				return Ok(());
			}
			ice::Event::Gathered => {
				self.candidates = None;
				return Ok(());
			}
			ice::Event::Rtc(event) => event,
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
				} else if data.data.len() as u64 > open.max_record_size {
					Err(qmux::Error::FrameTooLarge)
				} else {
					Ok(Bytes::from(data.data))
				};
				let _ = open.reads.send(record);
			}
			str0m::Event::ChannelClose(id) => {
				if let Some(Ok(open)) = &self.channel
					&& open.id == id
				{
					// The session's reader sees the end of its records.
					self.channel = Some(Err(ice::Error::Closed.into()));
				}
			}
			_ => {}
		}
		Ok(())
	}

	/// Take a channel that opened as the session's, if it is the one this role expects.
	fn open(&mut self, id: ChannelId) {
		if self.channel.is_some() {
			tracing::debug!(?id, "closing an extra data channel");
			self.agent.rtc().direct_api().close_data_channel(id);
			return;
		}
		if let Role::Offerer { channel, .. } = &self.role
			&& *channel != id
		{
			self.agent.rtc().direct_api().close_data_channel(id);
			return;
		}

		let opened = self.adopt(id);
		if opened.is_err() {
			self.agent.rtc().direct_api().close_data_channel(id);
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

		let mut channel = self.agent.rtc().channel(id).ok_or(ice::Error::Closed)?;
		let config = channel.config().ok_or(Error::Unreliable)?;
		if !config.ordered || config.reliability != Reliability::Reliable {
			return Err(Error::Unreliable);
		}
		let protocol = config.protocol.clone();
		channel.set_buffered_amount_low_threshold(SEND_WINDOW / 2);

		let (reads, rx) = mpsc::unbounded_channel();
		let (tx, writes) = mpsc::channel(1);
		Ok(Open {
			id,
			protocol,
			reads,
			writes: Some(writes),
			max_record_size,
			halves: Some(channel::Channel {
				writer: channel::Writer {
					tx: Some(tx),
					max: usize::try_from(max_message).unwrap_or(usize::MAX),
					_driver: None,
				},
				reader: channel::Reader { rx },
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
				max_record_size: open.max_record_size,
			}),
			Err(err) => Err(match err {
				Error::Unreliable => Error::Unreliable,
				Error::MessageSize(size) => Error::MessageSize(*size),
				_ => ice::Error::Closed.into(),
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
	let mut rtc = Rtc::builder().build(Instant::now());
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

/// Bind a dual-stack socket on an ephemeral port, pairing it with each host address.
fn bind(hosts: &[IpAddr]) -> Result<(Arc<UdpSocket>, Vec<SocketAddr>), Error> {
	let socket = crate::bind::udp(crate::bind::Udp::new("[::]:0".parse().unwrap()))
		.or_else(|_| crate::bind::udp(crate::bind::Udp::new("0.0.0.0:0".parse().unwrap())))?;
	socket.set_nonblocking(true)?;
	let socket = UdpSocket::from_std(socket)?;
	let local = socket.local_addr()?;

	let locals: Vec<SocketAddr> = hosts
		.iter()
		.filter(|ip| local.is_ipv6() || ip.is_ipv4())
		.map(|&ip| SocketAddr::new(ip, local.port()))
		.collect();
	if locals.is_empty() {
		return Err(Error::NoHost);
	}
	Ok((Arc::new(socket), locals))
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
fn split_mdns(sdp: &str) -> (String, Vec<String>) {
	let mut kept = String::with_capacity(sdp.len());
	let mut mdns = Vec::new();
	for line in sdp.split_inclusive('\n') {
		if let Some(candidate) = line.trim_end().strip_prefix("a=")
			&& candidate.starts_with("candidate:")
			&& candidate_address(candidate).is_some_and(|addr| addr.ends_with(".local"))
		{
			mdns.push(candidate.to_string());
			continue;
		}
		kept.push_str(line);
	}
	(kept, mdns)
}

/// The connection address of a `candidate:` line (RFC 8839 section 5.1).
fn candidate_address(line: &str) -> Option<&str> {
	line.split_whitespace().nth(4)
}

/// Parse a `candidate:` line, resolving a `.local` address over mDNS first.
async fn parse_candidate(line: &str) -> Result<str0m::Candidate, Error> {
	let line = match candidate_address(line) {
		Some(host) if host.ends_with(".local") => {
			let ip = mdns(host).await?;
			let mut fields: Vec<String> = line.split_whitespace().map(str::to_string).collect();
			fields[4] = ip.to_string();
			fields.join(" ")
		}
		_ => line.to_string(),
	};
	str0m::Candidate::from_sdp_string(&line).map_err(|err| Error::Candidate(err.to_string()))
}

/// The process-wide mDNS daemon, started on the first `.local` candidate.
static MDNS: Mutex<Option<mdns_sd::ServiceDaemon>> = Mutex::new(None);

/// Resolve a browser's `.local` host name, preferring an IPv4 address.
async fn mdns(host: &str) -> Result<IpAddr, Error> {
	let daemon = {
		let mut daemon = MDNS.lock().unwrap();
		match daemon.as_ref() {
			Some(daemon) => daemon.clone(),
			None => daemon
				.insert(mdns_sd::ServiceDaemon::new().map_err(|err| Error::Mdns(err.to_string()))?)
				.clone(),
		}
	};

	let name = format!("{host}.");
	let timeout = MDNS_TIMEOUT.as_millis() as u64;
	let events = daemon
		.resolve_hostname(&name, Some(timeout))
		.map_err(|err| Error::Mdns(err.to_string()))?;
	let found = loop {
		match events.recv_async().await {
			Ok(mdns_sd::HostnameResolutionEvent::AddressesFound(_, addrs)) if !addrs.is_empty() => {
				let addrs: Vec<IpAddr> = addrs.iter().map(|addr| addr.to_ip_addr()).collect();
				break addrs.iter().find(|ip| ip.is_ipv4()).or(addrs.first()).copied();
			}
			Ok(mdns_sd::HostnameResolutionEvent::SearchTimeout(_)) | Err(_) => break None,
			Ok(_) => {}
		}
	};
	let _ = daemon.stop_resolve_hostname(&name);
	found.ok_or_else(|| Error::Mdns(format!("{host} did not resolve")))
}

#[cfg(test)]
mod tests;
