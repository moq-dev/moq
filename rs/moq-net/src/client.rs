use crate::origin;
use crate::{
	ALPN_14, ALPN_15, ALPN_16, ALPN_17, ALPN_18, ALPN_19, ALPN_LITE, ALPN_LITE_03, ALPN_LITE_04, ALPN_LITE_05,
	ALPN_LITE_06_WIP, Consume, Error, NEGOTIATED, Session, Version, Versions,
	coding::{self, Decode, Encode, Stream},
	ietf, lite, setup, stats,
};

// The transport methods are called on the projected `R::Transport`, which needs the
// trait itself in scope (a plain type parameter would not).
use web_transport_trait::{MaybeSend, MaybeSync, poll::Session as _};

/// A MoQ client session builder.
#[derive(Default, Clone)]
pub struct Client {
	publish: Option<origin::Consumer>,
	subscribe: Option<origin::Producer>,
	stats: stats::Session,
	versions: Versions,
	setup_path: Option<String>,
	cost: Option<u64>,
	peer_origin: Option<crate::Origin>,
}

impl Client {
	/// A client that neither publishes nor subscribes until configured.
	pub fn new() -> Self {
		Default::default()
	}

	/// Publish local broadcasts to the remote: the session reads from the given
	/// origin (pass an [`origin::Producer`] or [`origin::Consumer`] by reference) and
	/// forwards its announcements. Omit to publish nothing.
	pub fn with_publisher(mut self, publish: impl Consume<origin::Consumer>) -> Self {
		self.publish = Some(publish.consume());
		self
	}

	/// Subscribe to remote broadcasts: the session writes the broadcasts the
	/// remote announces into this [`origin::Producer`]. Omit to subscribe to nothing.
	pub fn with_subscriber(mut self, subscribe: origin::Producer) -> Self {
		self.subscribe = Some(subscribe);
		self
	}

	/// Attach a per-connection [`stats::Session`] context. The session's publish
	/// (egress) and subscribe (ingress) origin handles are tagged with it, so all
	/// traffic counters are attributed through the model for this session's lifetime.
	/// Pass [`stats::Session::default`] (a no-op context) to opt out.
	pub fn with_stats(mut self, stats: stats::Session) -> Self {
		self.stats = stats;
		self
	}

	/// Set both publish and subscribe from one shared [`origin::Producer`].
	///
	/// Equivalent to [`with_publisher`](Self::with_publisher) and
	/// [`with_subscriber`](Self::with_subscriber) with the same origin.
	pub fn with_origin(self, origin: origin::Producer) -> Self {
		self.with_publisher(&origin).with_subscriber(origin)
	}

	/// Restrict which protocol versions to offer, in preference order.
	/// Defaults to every version this crate supports.
	pub fn with_versions(mut self, versions: Versions) -> Self {
		self.versions = versions;
		self
	}

	/// Set the request path to advertise in the SETUP (moq-lite-05 and every
	/// moq-transport draft we speak).
	///
	/// Only for transports that carry no request URI of their own (native QUIC, qmux
	/// over TCP/TLS, unix sockets), so the server learns which path the client wants.
	/// Append `?` and the URI query when there is one: that is how a credential in the
	/// query (`?jwt=`) reaches the server.
	/// Bindings that already carry a URI (WebTransport, qmux over WebSocket) convey
	/// the path there and MUST NOT send this; a server is entitled to treat it as a
	/// protocol violation. An empty path is equivalent to omitting it. Ignored by
	/// versions with no in-band request path (lite 01-04).
	pub fn with_path(mut self, path: impl Into<String>) -> Self {
		self.setup_path = Some(path.into());
		self
	}

	/// Price this link, in the units the rest of the mesh uses (moq-lite-06+, and
	/// `moqt-17`+ via the MoQ Cluster extension).
	///
	/// The dialer is the side that knows what a link costs, because it chose the peer:
	/// use `0` for a sibling in the same datacenter and something large for another
	/// region across a metered backbone. So this prices both directions. We add it to
	/// the route cost of every announcement the peer sends us, and declare it in our
	/// SETUP so the peer adds it to every announcement we send, which is what a server
	/// accepting an anonymous connection needs: it cannot tell a sibling from a
	/// stranger, so it has no price of its own to apply.
	///
	/// A price the peer declares applies only where we set none. An unpriced link costs
	/// `1`, which makes the cost track the hop count and so reproduces plain
	/// shortest-path routing.
	pub fn with_cost(mut self, cost: u64) -> Self {
		self.cost = Some(cost);
		self
	}

	/// Assign an origin (hop) id to the peer, used whenever the peer doesn't declare
	/// one itself.
	///
	/// Some relays never declare their identity: moq-lite peers without the hops
	/// extension, and moq-transport peers that don't negotiate the MoQ Cluster
	/// extension (or predate it, on `moqt-16` and earlier).
	/// Broadcasts received from such a peer are normally attributed to the reserved
	/// origin 0 ("unknown"), which identifies nothing: it never proves continuity,
	/// so their advertisements neither splice nor survive a restart in place. This
	/// knob pins a real identity instead, exactly as if the peer had declared it:
	///
	/// - broadcasts received from the peer carry `origin` in their hop chains, so
	///   every session dialing the same relay (with the same id) resolves to one
	///   route and loop checks can recognize it;
	/// - broadcasts whose hop chain already contains `origin` are neither announced
	///   nor served back to the peer, preventing an echo through a relay that does
	///   no loop detection of its own.
	///
	/// An identity the peer does declare wins over this one.
	pub fn with_peer_origin(mut self, origin: crate::Origin) -> Self {
		self.peer_origin = Some(origin);
		self
	}

	/// The origin pair a session attaches, tagged and filtered.
	///
	/// Reads through the publish (egress) consumer and writes through the
	/// subscribe (ingress) producer are attributed by the model through the
	/// stats context; one shared context, so presence and viewer counts are
	/// never double-attributed across the two halves. An assigned peer identity
	/// means subscriptions from the peer resolve to a source whose hop chain
	/// excludes it, the same split-horizon rule applied when a peer declares
	/// its own id; announce filtering is per-protocol and handled inside each
	/// publisher.
	fn origins(&self) -> (Option<origin::Consumer>, Option<origin::Producer>) {
		if self.publish.is_none() && self.subscribe.is_none() {
			tracing::warn!("not publishing or consuming anything");
		}
		let publish = self.publish.clone().map(|origin| origin.with_stats(self.stats.clone()));
		let subscribe = self
			.subscribe
			.clone()
			.map(|origin| origin.with_stats(self.stats.clone()));
		let publish = match self.peer_origin {
			Some(peer) => publish.map(|origin| origin.excluding(peer)),
			None => publish,
		};
		(publish, subscribe)
	}

	/// Start a lite session on an already-negotiated version: build our SETUP,
	/// wire the origins, and hand the machine to the runtime.
	fn start_lite<R>(&self, runtime: R, session: R::Transport, version: lite::Version) -> Result<Session, Error>
	where
		R: crate::runtime::Runtime + 'static,
	{
		let (publish, subscribe) = self.origins();

		// Advertise our capabilities (we report what the transport measures; we
		// don't pad) plus the request path on URI-less transports, and the
		// direction we intend to use so the server can reject a token that lacks
		// the matching scope during the handshake instead of silently carrying
		// no media. Versions without a Setup Stream have nothing to advertise.
		let our_setup = if version.has_setup_stream() {
			lite::Setup {
				probe: lite::ProbeLevel::detect(&session),
				path: self.setup_path.clone(),
				role: lite::Role::from_origins(self.publish.is_some(), self.subscribe.is_some()),
				cost: self.cost,
				// Filled by `lite::start` from the attached origin handles.
				origin: None,
			}
		} else {
			lite::Setup::default()
		};

		let start = lite::start(lite::Config {
			runtime: runtime.clone(),
			session: session.clone(),
			setup_stream: None,
			publish,
			subscribe,
			peer_origin: self.peer_origin,
			version,
			our_setup,
			peer_setup: None,
		})?;

		Ok(Session::spawn(
			runtime,
			session,
			version.into(),
			start.recv_bandwidth,
			crate::runtime::Protocol::Lite(Box::new(start.driver)),
			start.goaway,
		))
	}

	/// Perform the MoQ handshake for moq-lite only, over any transport.
	///
	/// Unlike [`connect`](Self::connect) this puts no thread-affinity bound on
	/// the transport, so a pinned `!Send` transport works and yields a `!Send`
	/// machine that stays on its thread. The trade is protocol scope: only a
	/// moq-lite ALPN is accepted, since the moq-transport driver still needs a
	/// [`Boxable`](crate::transport::poll::Boxable) transport. An ietf ALPN, an
	/// unknown one, or the legacy no-ALPN SETUP negotiation is refused with
	/// [`Error::Version`].
	pub async fn connect_lite<R>(&self, runtime: R, session: R::Transport) -> Result<Session, Error>
	where
		R: crate::runtime::Runtime + 'static,
	{
		let version = match session.protocol() {
			Some(ALPN_LITE_06_WIP) => lite::Version::Lite06Wip,
			Some(ALPN_LITE_05) => lite::Version::Lite05,
			Some(ALPN_LITE_04) => lite::Version::Lite04,
			Some(ALPN_LITE_03) => lite::Version::Lite03,
			_ => return Err(Error::Version),
		};
		self.versions.select(Version::Lite(version)).ok_or(Error::Version)?;
		self.start_lite(runtime, session, version)
	}

	/// Perform the MoQ handshake, returning the [`Session`].
	///
	/// The session's protocol machine is handed to `runtime`
	/// ([`Runtime::spawn`](crate::runtime::Runtime::spawn)), so there is nothing
	/// else to drive: the runtime runs the session for as long as it lives.
	pub async fn connect<R>(&self, runtime: R, mut session: R::Transport) -> Result<Session, Error>
	where
		R: crate::runtime::Runtime + MaybeSend + MaybeSync + 'static,
		R::Transport: crate::transport::poll::Boxable,
		R::Timer: MaybeSend,
	{
		let (publish, subscribe) = self.origins();

		// If ALPN was used to negotiate the version, use the appropriate encoding.
		// Default to IETF 14 if no ALPN was used and we'll negotiate the version later.
		let (encoding, supported) = match session.protocol() {
			Some(ALPN_19) => {
				let v = self
					.versions
					.select(Version::Ietf(ietf::Version::Draft19))
					.ok_or(Error::Version)?;

				// Draft-17+: SETUP is exchanged by the connection driver.
				let (protocol, goaway) = ietf::start(ietf::Config {
					runtime: runtime.clone(),
					session: session.clone(),
					setup: None,
					request_id_max: None,
					client: true,
					publish: publish.clone(),
					subscribe: subscribe.clone(),
					peer_origin: self.peer_origin,
					cost: self.cost,
					version: ietf::Version::Draft19,
					path: self.setup_path.clone(),
					peer_setup_stream: None,
					peer_declared: None,
				})?;

				tracing::debug!(version = ?v, "connected");
				return Ok(Session::spawn(
					runtime,
					session,
					v,
					None,
					crate::runtime::Protocol::Ietf(protocol),
					goaway,
				));
			}
			Some(ALPN_18) => {
				let v = self
					.versions
					.select(Version::Ietf(ietf::Version::Draft18))
					.ok_or(Error::Version)?;

				// Draft-17+: SETUP is exchanged by the connection driver.
				// We advertise the request path in our SETUP for URL-less transports.
				let (protocol, goaway) = ietf::start(ietf::Config {
					runtime: runtime.clone(),
					session: session.clone(),
					setup: None,
					request_id_max: None,
					client: true,
					publish: publish.clone(),
					subscribe: subscribe.clone(),
					peer_origin: self.peer_origin,
					cost: self.cost,
					version: ietf::Version::Draft18,
					path: self.setup_path.clone(),
					peer_setup_stream: None,
					peer_declared: None,
				})?;

				tracing::debug!(version = ?v, "connected");
				return Ok(Session::spawn(
					runtime,
					session,
					v,
					None,
					crate::runtime::Protocol::Ietf(protocol),
					goaway,
				));
			}
			Some(ALPN_17) => {
				let v = self
					.versions
					.select(Version::Ietf(ietf::Version::Draft17))
					.ok_or(Error::Version)?;

				// Draft-17+: SETUP is exchanged by the connection driver.
				// We advertise the request path in our SETUP for URL-less transports.
				let (protocol, goaway) = ietf::start(ietf::Config {
					runtime: runtime.clone(),
					session: session.clone(),
					setup: None,
					request_id_max: None,
					client: true,
					publish: publish.clone(),
					subscribe: subscribe.clone(),
					peer_origin: self.peer_origin,
					cost: self.cost,
					version: ietf::Version::Draft17,
					path: self.setup_path.clone(),
					peer_setup_stream: None,
					peer_declared: None,
				})?;

				tracing::debug!(version = ?v, "connected");
				return Ok(Session::spawn(
					runtime,
					session,
					v,
					None,
					crate::runtime::Protocol::Ietf(protocol),
					goaway,
				));
			}
			Some(ALPN_16) => {
				let v = self
					.versions
					.select(Version::Ietf(ietf::Version::Draft16))
					.ok_or(Error::Version)?;
				(v, v.into())
			}
			Some(ALPN_15) => {
				let v = self
					.versions
					.select(Version::Ietf(ietf::Version::Draft15))
					.ok_or(Error::Version)?;
				(v, v.into())
			}
			Some(ALPN_14) => {
				let v = self
					.versions
					.select(Version::Ietf(ietf::Version::Draft14))
					.ok_or(Error::Version)?;
				(v, v.into())
			}
			Some(alpn @ (ALPN_LITE_05 | ALPN_LITE_06_WIP)) => {
				let version = match alpn {
					ALPN_LITE_06_WIP => lite::Version::Lite06Wip,
					_ => lite::Version::Lite05,
				};
				self.versions.select(Version::Lite(version)).ok_or(Error::Version)?;
				return self.start_lite(runtime, session, version);
			}
			Some(ALPN_LITE_04) => {
				self.versions
					.select(Version::Lite(lite::Version::Lite04))
					.ok_or(Error::Version)?;
				return self.start_lite(runtime, session, lite::Version::Lite04);
			}
			Some(ALPN_LITE_03) => {
				self.versions
					.select(Version::Lite(lite::Version::Lite03))
					.ok_or(Error::Version)?;
				return self.start_lite(runtime, session, lite::Version::Lite03);
			}
			Some(ALPN_LITE) | None => {
				let supported = self.versions.filter(&NEGOTIATED.into()).ok_or(Error::Version)?;
				(Version::Ietf(ietf::Version::Draft14), supported)
			}
			Some(p) => return Err(Error::UnknownAlpn(p.to_string())),
		};

		let mut stream = Stream::open(&mut session, encoding).await?;

		// The encoding is always an IETF version for SETUP negotiation.
		let ietf_encoding = ietf::Version::try_from(encoding).map_err(|_| Error::Version)?;

		let mut parameters = ietf::Parameters::default();
		parameters.set_varint(ietf::ParameterVarInt::MaxRequestId, u32::MAX as u64);
		parameters.set_bytes(ietf::ParameterBytes::Implementation, b"moq-lite-rs".to_vec());
		// Advertise the request path in-band (draft 14-16), same as the lite-05 SETUP.
		if let Some(path) = &self.setup_path {
			parameters.set_bytes(ietf::ParameterBytes::Path, path.clone().into_bytes());
		}
		ietf::solicit::into_setup(&mut parameters, ietf_encoding);
		let parameters = parameters.encode_bytes(ietf_encoding)?;

		let client = setup::Client {
			versions: supported.clone().into(),
			parameters,
		};

		stream.writer.encode(&client).await?;

		let mut server: setup::Server = stream.reader.decode().await?;

		let version = supported
			.iter()
			.find(|v| coding::Version::from(**v) == server.version)
			.copied()
			.ok_or(Error::Version)?;

		let (recv_bw, protocol, goaway) = match version {
			Version::Lite(v) => {
				let stream = stream.with_version(v);
				let start = lite::start(lite::Config {
					runtime: runtime.clone(),
					session: session.clone(),
					setup_stream: Some(stream),
					publish: publish.clone(),
					subscribe: subscribe.clone(),
					peer_origin: self.peer_origin,
					version: v,
					// This path only handles versions negotiated via the bidi SETUP exchange
					// (pre-lite-05), which have no Setup Stream.
					our_setup: lite::Setup::default(),
					peer_setup: None,
				})?;

				(
					start.recv_bandwidth,
					crate::runtime::Protocol::Lite(Box::new(start.driver)),
					start.goaway,
				)
			}
			Version::Ietf(v) => {
				// Decode the parameters to get the initial request ID and what the server
				// requires of us.
				let parameters = ietf::Parameters::decode(&mut server.parameters, v)?;
				let request_id_max = parameters
					.get_varint(ietf::ParameterVarInt::MaxRequestId)
					.map(ietf::RequestId);
				let peer_declared = ietf::peer::Peer {
					solicit: ietf::solicit::from_setup(&parameters, v)?,
					..Default::default()
				};

				let stream = stream.with_version(v);
				// Draft 14-16: the path rode in the bidi SETUP above, not the uni one.
				let (protocol, goaway) = ietf::start(ietf::Config {
					runtime: runtime.clone(),
					session: session.clone(),
					setup: Some(stream),
					request_id_max,
					client: true,
					publish: publish.clone(),
					subscribe: subscribe.clone(),
					peer_origin: self.peer_origin,
					cost: self.cost,
					version: v,
					path: None,
					peer_setup_stream: None,
					peer_declared: Some(peer_declared),
				})?;
				(None, crate::runtime::Protocol::Ietf(protocol), goaway)
			}
		};

		Ok(Session::spawn(runtime, session, version, recv_bw, protocol, goaway))
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::model::ProduceTest;
	use std::{
		collections::VecDeque,
		sync::{Arc, Mutex},
	};

	use std::task::{Context, Poll};

	use crate::SessionError;
	use crate::coding::{Decode, Encode};
	use bytes::{BufMut, Bytes};

	#[derive(Debug, Clone, Default)]
	struct FakeError;

	impl std::fmt::Display for FakeError {
		fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
			write!(f, "fake transport error")
		}
	}

	impl std::error::Error for FakeError {}

	impl web_transport_trait::Error for FakeError {
		fn session_error(&self) -> Option<(u32, String)> {
			Some((0, "closed".to_string()))
		}
	}

	#[derive(Clone, Default)]
	struct FakeSession {
		state: Arc<FakeSessionState>,
		// Per-clone, so each pending poll_closed keeps its own registration live.
		park: kio::Park,
	}

	#[derive(Default)]
	struct FakeSessionState {
		protocol: Option<&'static str>,
		control_stream: Mutex<Option<(FakeSendStream, FakeRecvStream)>>,
		close_events: Mutex<Vec<(u32, String)>>,
		closed: kio::Fan,
		control_writes: Arc<Mutex<Vec<u8>>>,
		send_rate: Mutex<Option<u64>>,
		bytes_sent: Mutex<Option<u64>>,
	}

	impl FakeSession {
		fn new(protocol: Option<&'static str>, server_control_bytes: Vec<u8>) -> Self {
			let writes = Arc::new(Mutex::new(Vec::new()));
			let send = FakeSendStream { writes: writes.clone() };
			let recv = FakeRecvStream {
				data: VecDeque::from(server_control_bytes),
			};
			let state = FakeSessionState {
				protocol,
				control_stream: Mutex::new(Some((send, recv))),
				close_events: Mutex::new(Vec::new()),
				closed: kio::Fan::default(),
				control_writes: writes,
				send_rate: Mutex::new(None),
				bytes_sent: Mutex::new(None),
			};
			Self {
				state: Arc::new(state),
				park: kio::Park::default(),
			}
		}

		fn set_send_rate(&self, rate: Option<u64>) {
			*self.state.send_rate.lock().unwrap() = rate;
		}

		fn set_bytes_sent(&self, bytes: Option<u64>) {
			*self.state.bytes_sent.lock().unwrap() = bytes;
		}

		fn control_writes(&self) -> Vec<u8> {
			self.state.control_writes.lock().unwrap().clone()
		}

		async fn wait_for_first_close(&self) -> (u32, String) {
			kio::wait(|waiter| {
				self.state.closed.register(waiter);
				match self.state.close_events.lock().unwrap().first().cloned() {
					Some(close) => std::task::Poll::Ready(close),
					None => std::task::Poll::Pending,
				}
			})
			.await
		}
	}

	impl web_transport_trait::poll::Session for FakeSession {
		type SendStream = FakeSendStream;
		type RecvStream = FakeRecvStream;
		type Error = FakeError;

		fn poll_accept_uni(&mut self, _cx: &mut Context<'_>) -> Poll<Result<Self::RecvStream, Self::Error>> {
			Poll::Pending
		}

		fn poll_accept_bi(
			&mut self,
			_cx: &mut Context<'_>,
		) -> Poll<Result<(Self::SendStream, Self::RecvStream), Self::Error>> {
			Poll::Pending
		}

		fn poll_open_bi(
			&mut self,
			_cx: &mut Context<'_>,
		) -> Poll<Result<(Self::SendStream, Self::RecvStream), Self::Error>> {
			Poll::Ready(self.state.control_stream.lock().unwrap().take().ok_or(FakeError))
		}

		fn poll_open_uni(&mut self, _cx: &mut Context<'_>) -> Poll<Result<Self::SendStream, Self::Error>> {
			Poll::Pending
		}

		fn poll_send_datagram(&mut self, _cx: &mut Context<'_>, _payload: &[u8]) -> Poll<Result<(), Self::Error>> {
			Poll::Ready(Ok(()))
		}

		fn poll_recv_datagram(&mut self, _cx: &mut Context<'_>) -> Poll<Result<Bytes, Self::Error>> {
			Poll::Pending
		}

		fn max_datagram_size(&self) -> usize {
			1200
		}

		fn protocol(&self) -> Option<&str> {
			self.state.protocol
		}

		fn close(&mut self, code: u32, reason: &str) {
			self.state.close_events.lock().unwrap().push((code, reason.to_string()));
			self.state.closed.wake();
		}

		fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Self::Error> {
			// Register before checking so a close racing this poll still wakes it.
			self.state.closed.register(self.park.hold(cx));
			match self.state.close_events.lock().unwrap().is_empty() {
				false => Poll::Ready(FakeError),
				true => Poll::Pending,
			}
		}

		fn stats(&self) -> impl web_transport_trait::Stats {
			FakeStats {
				send_rate: *self.state.send_rate.lock().unwrap(),
				bytes_sent: *self.state.bytes_sent.lock().unwrap(),
			}
		}
	}

	struct FakeStats {
		send_rate: Option<u64>,
		bytes_sent: Option<u64>,
	}

	impl web_transport_trait::Stats for FakeStats {
		fn estimated_send_rate(&self) -> Option<u64> {
			self.send_rate
		}

		fn bytes_sent(&self) -> Option<u64> {
			self.bytes_sent
		}
	}

	#[derive(Clone, Default)]
	struct FakeSendStream {
		writes: Arc<Mutex<Vec<u8>>>,
	}

	impl web_transport_trait::poll::SendStream for FakeSendStream {
		type Error = FakeError;

		fn poll_write(&mut self, _cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize, Self::Error>> {
			self.writes.lock().unwrap().put_slice(buf);
			Poll::Ready(Ok(buf.len()))
		}

		fn set_priority(&mut self, _order: u8) {}

		fn finish(&mut self) -> Result<(), Self::Error> {
			Ok(())
		}

		fn reset(&mut self, _code: u32) {}

		fn poll_closed(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
			Poll::Ready(Ok(()))
		}
	}

	struct FakeRecvStream {
		data: VecDeque<u8>,
	}

	impl web_transport_trait::poll::RecvStream for FakeRecvStream {
		type Error = FakeError;

		fn poll_read(&mut self, _cx: &mut Context<'_>, dst: &mut [u8]) -> Poll<Result<Option<usize>, Self::Error>> {
			if self.data.is_empty() {
				return Poll::Ready(Ok(None));
			}

			let size = dst.len().min(self.data.len());
			for slot in dst.iter_mut().take(size) {
				*slot = self.data.pop_front().unwrap();
			}
			Poll::Ready(Ok(Some(size)))
		}

		fn stop(&mut self, _code: u32) {}

		fn poll_closed(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
			Poll::Ready(Ok(()))
		}
	}

	fn mock_server_setup(negotiated: Version) -> Vec<u8> {
		let mut encoded = Vec::new();
		let server = setup::Server {
			version: negotiated.into(),
			parameters: Bytes::new(),
		};
		server
			.encode(&mut encoded, Version::Ietf(ietf::Version::Draft14))
			.unwrap();

		// Add a setup-stream SessionInfo frame using the negotiated Lite version.
		let info = lite::SessionInfo { bitrate: Some(1) };
		let lite_v = lite::Version::try_from(negotiated).unwrap();
		info.encode(&mut encoded, lite_v).unwrap();

		encoded
	}

	async fn run_alpn_lite_fallback_case(protocol: Option<&'static str>) {
		let fake = FakeSession::new(protocol, mock_server_setup(Version::Lite(lite::Version::Lite01)));
		let client = Client::new().with_versions(
			[
				Version::Lite(lite::Version::Lite03),
				Version::Lite(lite::Version::Lite02),
				Version::Lite(lite::Version::Lite01),
				Version::Ietf(ietf::Version::Draft14),
			]
			.into(),
		);

		// `connect` returns as soon as the handshake completes; the protocol machine
		// was handed to the runtime, which spawned it onto tokio.
		let _session = client
			.connect(crate::runtime::tokio_test::Tokio::new(), fake.clone())
			.await
			.unwrap();

		// Verify the client setup was encoded using Draft14 framing (ALPN_LITE fallback path).
		let mut setup_bytes = Bytes::from(fake.control_writes());
		let setup = setup::Client::decode(&mut setup_bytes, Version::Ietf(ietf::Version::Draft14)).unwrap();
		let advertised: Vec<Version> = setup.versions.iter().map(|v| Version::try_from(*v).unwrap()).collect();
		assert_eq!(
			advertised,
			vec![
				Version::Lite(lite::Version::Lite02),
				Version::Lite(lite::Version::Lite01),
				Version::Ietf(ietf::Version::Draft14),
			]
		);

		// The first close comes from the lite connection driver.
		// Any non-Version error here means SessionInfo decoded successfully
		// after set_version(). This test cares about the SETUP framing
		// fallback, not the specific close code. Cancel is what we'd see
		// with no origin; RequiredExtension (or similar) is what an
		// auto-created origin's first interaction with a Lite01 peer trips.
		let (code, _) = fake.wait_for_first_close().await;
		// Session closes encode through the session registry, so compare against that one:
		// `Error::Version.to_code()` is the local table's value and would never match.
		assert_ne!(code, SessionError::Version.to_code(), "SessionInfo failed to decode");
	}

	/// `connect` must not depend on the peer answering. A peer that opens the announce
	/// stream and then says nothing (or promises a count it never delivers) used to hold
	/// `connect` for the life of the session, since it waited for the initial announce
	/// set. Resolving a path you need is `announced_broadcast`'s job, which waits for
	/// that path rather than for the peer to finish talking.
	#[tokio::test(start_paused = true)]
	async fn connect_does_not_wait_for_the_peer_to_announce() {
		// Serves bidi streams, so the announce stream opens, and never answers on them.
		let gate = kio::Producer::new(true);
		let transport = crate::lite::test_transport::SinkSession::gated_bi(gate.consume())
			.with_protocol(crate::version::ALPN_LITE_05);

		// A subscribe origin is what makes the client open an announce stream at all.
		let origin = crate::origin::Info::new(crate::Origin::new(1).unwrap()).produce();
		let client = Client::new()
			.with_versions([Version::Lite(lite::Version::Lite05)].into())
			.with_subscriber(origin);

		// Paused time auto-advances while every task is idle, so a `connect` that waits
		// on the silent peer trips this rather than hanging the suite.
		tokio::time::timeout(
			std::time::Duration::from_secs(30),
			client.connect(crate::runtime::tokio_test::Tokio::new(), transport),
		)
		.await
		.expect("connect waited on a peer that never announced")
		.expect("connect failed");
	}

	#[tokio::test(start_paused = true)]
	async fn alpn_lite_falls_back_to_draft14_and_switches_version_post_setup() {
		run_alpn_lite_fallback_case(Some(ALPN_LITE)).await;
	}

	#[tokio::test(start_paused = true)]
	async fn no_alpn_falls_back_to_draft14_and_switches_version_post_setup() {
		run_alpn_lite_fallback_case(None).await;
	}

	// This fake reports no send-rate estimate, so it never reaches a timer in the
	// bandwidth loop. `connect` hands the machine to the runtime we pass, so with
	// the deterministic Test runtime nothing runs on an ambient executor at all.
	//
	// The machine must hold no Session clone (the #2286 leak), so the transport
	// still closes when the caller drops their last session handle. The close is
	// relayed through the machine (the Session holds no transport), so it lands
	// on the next tick.
	#[test]
	fn machine_is_runtime_polled_and_holds_no_session() {
		let fake = FakeSession::new(Some(ALPN_LITE_04), Vec::new());
		let client = Client::new().with_versions(Version::Lite(lite::Version::Lite04).into());

		let runtime = crate::runtime::Test::<FakeSession>::new();
		let session = futures::executor::block_on(client.connect(runtime.clone(), fake.clone())).unwrap();
		assert_eq!(session.version(), Version::Lite(lite::Version::Lite04));

		// The machine sits inside the Test runtime, stepped manually: nothing was
		// spawned onto an ambient runtime, and it is still pending.
		assert_eq!(runtime.tick(), 1);

		// The caller drops their only session clone; the machine observes the
		// last handle going away and closes the transport.
		drop(session);
		runtime.tick();
		assert_eq!(fake.state.close_events.lock().unwrap()[0].0, Error::Cancel.to_code());
	}

	// Clones share the connection: the transport closes on the LAST drop, and
	// abort() closes it explicitly (first close wins). Both are relayed through
	// the machine, so each takes a tick to land.
	#[test]
	fn session_clones_share_the_close() {
		let fake = FakeSession::new(Some(ALPN_LITE_04), Vec::new());
		let client = Client::new().with_versions(Version::Lite(lite::Version::Lite04).into());

		let runtime = crate::runtime::Test::<FakeSession>::new();
		let session = futures::executor::block_on(client.connect(runtime.clone(), fake.clone())).unwrap();
		let clone = session.clone();

		// One clone dropping does nothing while another is alive.
		drop(session);
		runtime.tick();
		assert!(fake.state.close_events.lock().unwrap().is_empty());

		clone.abort(Error::Cancel);
		runtime.tick();
		assert_eq!(fake.state.close_events.lock().unwrap()[0].0, Error::Cancel.to_code());

		// And the machine publishes the transport's terminal error, which is
		// what `closed()` reports.
		runtime.tick();
		futures::executor::block_on(clone.closed());

		// The final drop requests no second close: the handle-side close is once.
		let closes = fake.state.close_events.lock().unwrap().len();
		drop(clone);
		runtime.tick();
		assert_eq!(fake.state.close_events.lock().unwrap().len(), closes);
	}

	// A runtime that drops the machine instead of running it tears the session
	// down: the machine was the only transport holder, and `closed()` resolves
	// rather than parking forever on a machine nobody polls.
	#[test]
	fn dropped_machine_resolves_closed() {
		let fake = FakeSession::new(Some(ALPN_LITE_04), Vec::new());
		let client = Client::new().with_versions(Version::Lite(lite::Version::Lite04).into());

		let runtime = crate::runtime::Test::<FakeSession>::new();
		let session = futures::executor::block_on(client.connect(runtime.clone(), fake.clone())).unwrap();

		runtime.shutdown();
		assert!(matches!(futures::executor::block_on(session.closed()), Error::Cancel));
	}

	/// A transport made deliberately `!Send` by an `Rc` marker on the session and
	/// both stream types: compiling at all is the point, proving the lite path
	/// never demands thread mobility of any transport piece.
	#[derive(Clone)]
	struct LocalSession {
		inner: FakeSession,
		_local: std::rc::Rc<()>,
	}

	struct LocalSend {
		inner: FakeSendStream,
		_local: std::rc::Rc<()>,
	}

	struct LocalRecv {
		inner: FakeRecvStream,
		_local: std::rc::Rc<()>,
	}

	impl web_transport_trait::poll::Session for LocalSession {
		type SendStream = LocalSend;
		type RecvStream = LocalRecv;
		type Error = FakeError;

		fn poll_accept_uni(&mut self, cx: &mut Context<'_>) -> Poll<Result<Self::RecvStream, Self::Error>> {
			self.inner.poll_accept_uni(cx).map_ok(|stream| LocalRecv {
				inner: stream,
				_local: self._local.clone(),
			})
		}

		fn poll_accept_bi(
			&mut self,
			cx: &mut Context<'_>,
		) -> Poll<Result<(Self::SendStream, Self::RecvStream), Self::Error>> {
			self.inner.poll_accept_bi(cx).map_ok(|(send, recv)| {
				(
					LocalSend {
						inner: send,
						_local: self._local.clone(),
					},
					LocalRecv {
						inner: recv,
						_local: self._local.clone(),
					},
				)
			})
		}

		fn poll_open_bi(
			&mut self,
			cx: &mut Context<'_>,
		) -> Poll<Result<(Self::SendStream, Self::RecvStream), Self::Error>> {
			self.inner.poll_open_bi(cx).map_ok(|(send, recv)| {
				(
					LocalSend {
						inner: send,
						_local: self._local.clone(),
					},
					LocalRecv {
						inner: recv,
						_local: self._local.clone(),
					},
				)
			})
		}

		fn poll_open_uni(&mut self, cx: &mut Context<'_>) -> Poll<Result<Self::SendStream, Self::Error>> {
			self.inner.poll_open_uni(cx).map_ok(|stream| LocalSend {
				inner: stream,
				_local: self._local.clone(),
			})
		}

		fn poll_send_datagram(&mut self, cx: &mut Context<'_>, payload: &[u8]) -> Poll<Result<(), Self::Error>> {
			self.inner.poll_send_datagram(cx, payload)
		}

		fn poll_recv_datagram(&mut self, cx: &mut Context<'_>) -> Poll<Result<Bytes, Self::Error>> {
			self.inner.poll_recv_datagram(cx)
		}

		fn max_datagram_size(&self) -> usize {
			self.inner.max_datagram_size()
		}

		fn protocol(&self) -> Option<&str> {
			self.inner.protocol()
		}

		fn close(&mut self, code: u32, reason: &str) {
			self.inner.close(code, reason);
		}

		fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Self::Error> {
			self.inner.poll_closed(cx)
		}

		fn stats(&self) -> impl web_transport_trait::Stats {
			self.inner.stats()
		}
	}

	impl web_transport_trait::poll::SendStream for LocalSend {
		type Error = FakeError;

		fn poll_write(&mut self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize, Self::Error>> {
			self.inner.poll_write(cx, buf)
		}

		fn set_priority(&mut self, order: u8) {
			self.inner.set_priority(order);
		}

		fn finish(&mut self) -> Result<(), Self::Error> {
			self.inner.finish()
		}

		fn reset(&mut self, code: u32) {
			self.inner.reset(code);
		}

		fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
			web_transport_trait::poll::SendStream::poll_closed(&mut self.inner, cx)
		}
	}

	impl web_transport_trait::poll::RecvStream for LocalRecv {
		type Error = FakeError;

		fn poll_read(&mut self, cx: &mut Context<'_>, dst: &mut [u8]) -> Poll<Result<Option<usize>, Self::Error>> {
			self.inner.poll_read(cx, dst)
		}

		fn stop(&mut self, code: u32) {
			self.inner.stop(code);
		}

		fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
			web_transport_trait::poll::RecvStream::poll_closed(&mut self.inner, cx)
		}
	}

	// The point of the lite-only entry: a `!Send` transport yields a `!Send`
	// machine held by a local runtime, while the severed Session handle stays
	// Send + Sync. Compiling is most of the assertion; the rest checks the
	// machine still relays the close.
	#[test]
	fn connect_lite_over_a_send_less_transport() {
		let fake = FakeSession::new(Some(ALPN_LITE_04), Vec::new());
		let local = LocalSession {
			inner: fake.clone(),
			_local: std::rc::Rc::new(()),
		};
		let client = Client::new().with_versions(Version::Lite(lite::Version::Lite04).into());

		let runtime = crate::runtime::Test::<LocalSession>::new();
		let session = futures::executor::block_on(client.connect_lite(runtime.clone(), local)).unwrap();
		assert_eq!(runtime.tick(), 1);

		fn assert_send_sync<T: Send + Sync>(_: &T) {}
		assert_send_sync(&session);

		session.abort(Error::Cancel);
		runtime.tick();
		assert_eq!(fake.state.close_events.lock().unwrap()[0].0, Error::Cancel.to_code());
	}

	// The server-side twin: a `!Send` transport accepts a lite session whose
	// machine a local runtime drives.
	#[test]
	fn accept_lite_over_a_send_less_transport() {
		let fake = FakeSession::new(Some(ALPN_LITE_04), Vec::new());
		let local = LocalSession {
			inner: fake.clone(),
			_local: std::rc::Rc::new(()),
		};
		let server = crate::Server::new().with_versions(Version::Lite(lite::Version::Lite04).into());

		let runtime = crate::runtime::Test::<LocalSession>::new();
		let session = futures::executor::block_on(server.accept_lite(runtime.clone(), local)).unwrap();
		assert_eq!(session.version(), Version::Lite(lite::Version::Lite04));
		assert_eq!(runtime.tick(), 1);

		drop(session);
		runtime.tick();
		assert_eq!(fake.state.close_events.lock().unwrap()[0].0, Error::Cancel.to_code());
	}

	// The lite-only entry refuses everything that still needs the boxed ietf
	// driver, instead of silently negotiating it.
	#[test]
	fn connect_lite_refuses_ietf_alpns() {
		let fake = FakeSession::new(Some(ALPN_19), Vec::new());
		let local = LocalSession {
			inner: fake,
			_local: std::rc::Rc::new(()),
		};
		let client = Client::new();
		let runtime = crate::runtime::Test::<LocalSession>::new();
		let result = futures::executor::block_on(client.connect_lite(runtime, local));
		assert!(matches!(result, Err(Error::Version)));
	}

	// `stats()` reads the machine's latest sample and primes the sampler, so a
	// periodic poller observes fresh counters without consuming the bandwidth
	// channel.
	#[tokio::test(start_paused = true)]
	async fn stats_reads_prime_the_sampler() {
		let fake = FakeSession::new(Some(ALPN_LITE_04), Vec::new());
		fake.set_send_rate(Some(1_000_000));

		let client = Client::new().with_versions(Version::Lite(lite::Version::Lite04).into());
		let session = client
			.connect(crate::runtime::tokio_test::Tokio::new(), fake.clone())
			.await
			.unwrap();

		// The construction-time snapshot, before the machine sampled anything.
		assert_eq!(session.stats().estimated_send_rate, Some(1_000_000));

		// That read was demand: the machine keeps sampling while stats are read,
		// so the new rate shows up within an interval (paused time auto-advances).
		fake.set_send_rate(Some(2_000_000));
		while session.stats().estimated_send_rate != Some(2_000_000) {
			tokio::time::sleep(std::time::Duration::from_millis(10)).await;
		}
	}

	// Sampling stops when the supervisor ends, but `stats()` keeps serving its
	// cell, so the last thing the supervisor does is take a final snapshot.
	// Without one, "what did that session move?" asked at teardown answers with
	// the construction-time snapshot: this backend reports no send rate, so
	// there is no bandwidth consumer keeping the sampler ticking, and the test
	// never reads stats while the session is live.
	#[tokio::test(start_paused = true)]
	async fn stats_capture_the_final_counters() {
		let fake = FakeSession::new(Some(ALPN_LITE_04), Vec::new());
		fake.set_send_rate(None);
		fake.set_bytes_sent(Some(0));

		let client = Client::new().with_versions(Version::Lite(lite::Version::Lite04).into());
		let session = client
			.connect(crate::runtime::tokio_test::Tokio::new(), fake.clone())
			.await
			.unwrap();
		assert!(
			session.send_bandwidth().is_none(),
			"no send-rate estimate, so nothing samples on its own"
		);

		fake.set_bytes_sent(Some(4242));

		session.abort(Error::Cancel);
		session.closed().await;

		assert_eq!(
			session.stats().bytes_sent,
			Some(4242),
			"the closing snapshot must carry the session's final counters"
		);
	}

	// The send-bandwidth sampler lives inside the driver: it samples as soon as a
	// consumer exists and keeps sampling on its interval. Paused tokio time makes
	// the interval fire deterministically.
	#[tokio::test(start_paused = true)]
	async fn send_bandwidth_samples_while_the_driver_runs() {
		let fake = FakeSession::new(Some(ALPN_LITE_04), Vec::new());
		fake.set_send_rate(Some(1_000_000));

		let client = Client::new().with_versions(Version::Lite(lite::Version::Lite04).into());
		let session = client
			.connect(crate::runtime::tokio_test::Tokio::new(), fake.clone())
			.await
			.unwrap();

		let mut bandwidth = session.send_bandwidth().expect("backend reports an estimate");
		assert_eq!(bandwidth.changed().await.unwrap(), Some(1_000_000));

		// A later change is picked up by the next interval tick.
		fake.set_send_rate(Some(2_000_000));
		assert_eq!(bandwidth.changed().await.unwrap(), Some(2_000_000));
	}
}
