use crate::origin;
use crate::{
	ALPN_14, ALPN_15, ALPN_16, ALPN_17, ALPN_18, ALPN_19, ALPN_LITE, ALPN_LITE_03, ALPN_LITE_04, ALPN_LITE_05,
	ALPN_LITE_06_WIP, Consume, Driver, Error, NEGOTIATED, Session, Version, Versions,
	coding::{self, Decode, Encode, Stream},
	ietf, lite, setup, stats,
};

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

	/// Perform the MoQ handshake, returning the [`Session`] and the [`Driver`] that
	/// runs its protocol work. The driver must be polled (spawned or awaited) for
	/// the session to make progress.
	pub async fn connect<S: crate::transport::poll::Session>(
		&self,
		mut session: S,
	) -> Result<(Session, Driver), Error> {
		if self.publish.is_none() && self.subscribe.is_none() {
			tracing::warn!("not publishing or consuming anything");
		}

		// Tag the origin pair with the stats context: reads through the publish
		// (egress) consumer and writes through the subscribe (ingress) producer are
		// then attributed by the model. One shared context, so presence and viewer
		// counts are never double-attributed across the two halves.
		let publish = self.publish.clone().map(|origin| origin.with_stats(self.stats.clone()));
		let subscribe = self
			.subscribe
			.clone()
			.map(|origin| origin.with_stats(self.stats.clone()));

		// An assigned peer identity means subscriptions from the peer resolve to a
		// source whose hop chain excludes it, the same split-horizon rule applied
		// when a peer declares its own id. Announce filtering is per-protocol and
		// handled inside each publisher.
		let publish = match self.peer_origin {
			Some(peer) => publish.map(|origin| origin.excluding(peer)),
			None => publish,
		};

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
				return Ok(Session::new(session, v, None, protocol, goaway));
			}
			Some(ALPN_18) => {
				let v = self
					.versions
					.select(Version::Ietf(ietf::Version::Draft18))
					.ok_or(Error::Version)?;

				// Draft-17+: SETUP is exchanged by the connection driver.
				// We advertise the request path in our SETUP for URL-less transports.
				let (protocol, goaway) = ietf::start(ietf::Config {
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
				return Ok(Session::new(session, v, None, protocol, goaway));
			}
			Some(ALPN_17) => {
				let v = self
					.versions
					.select(Version::Ietf(ietf::Version::Draft17))
					.ok_or(Error::Version)?;

				// Draft-17+: SETUP is exchanged by the connection driver.
				// We advertise the request path in our SETUP for URL-less transports.
				let (protocol, goaway) = ietf::start(ietf::Config {
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
				return Ok(Session::new(session, v, None, protocol, goaway));
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

				// Advertise our capabilities (we report send bitrate; we don't pad) plus
				// the request path on URI-less transports, and the direction we intend to
				// use so the server can reject a token that lacks the matching scope during
				// the handshake instead of silently carrying no media.
				let our_setup = lite::Setup {
					probe: lite::ProbeLevel::Report,
					path: self.setup_path.clone(),
					role: lite::Role::from_origins(self.publish.is_some(), self.subscribe.is_some()),
					cost: self.cost,
					// Filled by `lite::start` from the attached origin handles.
					origin: None,
				};

				let start = lite::start(lite::Config {
					session: session.clone(),
					setup_stream: None,
					publish: publish.clone(),
					subscribe: subscribe.clone(),
					peer_origin: self.peer_origin,
					version,
					our_setup,
					peer_setup: None,
				})?;

				return Ok(Session::new(
					session,
					version.into(),
					start.recv_bandwidth,
					start.driver,
					start.goaway,
				));
			}
			Some(ALPN_LITE_04) => {
				self.versions
					.select(Version::Lite(lite::Version::Lite04))
					.ok_or(Error::Version)?;

				let start = lite::start(lite::Config {
					session: session.clone(),
					setup_stream: None,
					publish: publish.clone(),
					subscribe: subscribe.clone(),
					peer_origin: self.peer_origin,
					version: lite::Version::Lite04,
					our_setup: lite::Setup::default(),
					peer_setup: None,
				})?;

				return Ok(Session::new(
					session,
					lite::Version::Lite04.into(),
					start.recv_bandwidth,
					start.driver,
					start.goaway,
				));
			}
			Some(ALPN_LITE_03) => {
				self.versions
					.select(Version::Lite(lite::Version::Lite03))
					.ok_or(Error::Version)?;

				// Starting with draft-03, there's no more SETUP control stream.
				let start = lite::start(lite::Config {
					session: session.clone(),
					setup_stream: None,
					publish: publish.clone(),
					subscribe: subscribe.clone(),
					peer_origin: self.peer_origin,
					version: lite::Version::Lite03,
					our_setup: lite::Setup::default(),
					peer_setup: None,
				})?;

				return Ok(Session::new(
					session,
					lite::Version::Lite03.into(),
					start.recv_bandwidth,
					start.driver,
					start.goaway,
				));
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

				(start.recv_bandwidth, start.driver, start.goaway)
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
				(None, protocol, goaway)
			}
		};

		Ok(Session::new(session, version, recv_bw, protocol, goaway))
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
			};
			Self {
				state: Arc::new(state),
				park: kio::Park::default(),
			}
		}

		fn set_send_rate(&self, rate: Option<u64>) {
			*self.state.send_rate.lock().unwrap() = rate;
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
			}
		}
	}

	struct FakeStats {
		send_rate: Option<u64>,
	}

	impl web_transport_trait::Stats for FakeStats {
		fn estimated_send_rate(&self) -> Option<u64> {
			self.send_rate
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

		// `connect` returns as soon as the handshake completes and never polls the driver,
		// so the session makes no progress (and never closes) unless we drive it here.
		let (_session, driver) = client.connect(fake.clone()).await.unwrap();
		let _driver = tokio::spawn(driver);

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
		tokio::time::timeout(std::time::Duration::from_secs(30), client.connect(transport))
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

	// This fake reports no send-rate estimate, so it never reaches the tokio timer in
	// the bandwidth loop. A driver is NOT runtime-free in general; see the Async
	// docs in lib.rs.
	//
	// The driver must hold no Session clone (the #2286 leak), so the transport still
	// closes when the caller drops their last session handle, which is what lets a
	// spawned driver task finish.
	#[test]
	fn driver_is_caller_polled_and_holds_no_session() {
		let fake = FakeSession::new(Some(ALPN_LITE_04), Vec::new());
		let client = Client::new().with_versions(Version::Lite(lite::Version::Lite04).into());

		let (session, mut driver) = futures::executor::block_on(client.connect(fake.clone())).unwrap();
		assert_eq!(session.version(), Version::Lite(lite::Version::Lite04));

		// An arbitrary waiter drives it kio-style: nothing was spawned onto a runtime.
		assert!(driver.poll(&kio::Waiter::noop()).is_pending());

		// The driver is also a plain future (stand in for spawning it).
		let mut context = std::task::Context::from_waker(std::task::Waker::noop());
		assert!(std::future::Future::poll(std::pin::Pin::new(&mut driver), &mut context).is_pending());

		// The caller drops their only session clone, so the transport closes even
		// though the driver is still alive.
		drop(session);
		assert_eq!(fake.state.close_events.lock().unwrap()[0].0, Error::Cancel.to_code());
	}

	// Clones share the connection: the transport closes on the LAST drop, and
	// abort() closes it explicitly (first close wins).
	#[test]
	fn session_clones_share_the_close() {
		let fake = FakeSession::new(Some(ALPN_LITE_04), Vec::new());
		let client = Client::new().with_versions(Version::Lite(lite::Version::Lite04).into());

		let (session, _driver) = futures::executor::block_on(client.connect(fake.clone())).unwrap();
		let clone = session.clone();

		// One clone dropping does nothing while another is alive.
		drop(session);
		assert!(fake.state.close_events.lock().unwrap().is_empty());

		clone.abort(Error::Cancel);
		assert_eq!(fake.state.close_events.lock().unwrap()[0].0, Error::Cancel.to_code());

		// The final drop is a no-op thanks to close-once.
		drop(clone);
		assert_eq!(fake.state.close_events.lock().unwrap().len(), 1);
	}

	// The send-bandwidth sampler lives inside the driver: it samples as soon as a
	// consumer exists and keeps sampling on its interval. Paused tokio time makes
	// the interval fire deterministically.
	#[tokio::test(start_paused = true)]
	async fn send_bandwidth_samples_while_the_driver_runs() {
		let fake = FakeSession::new(Some(ALPN_LITE_04), Vec::new());
		fake.set_send_rate(Some(1_000_000));

		let client = Client::new().with_versions(Version::Lite(lite::Version::Lite04).into());
		let (session, driver) = client.connect(fake.clone()).await.unwrap();
		tokio::spawn(driver);

		let mut bandwidth = session.send_bandwidth().expect("backend reports an estimate");
		assert_eq!(bandwidth.changed().await.unwrap(), Some(1_000_000));

		// A later change is picked up by the next interval tick.
		fake.set_send_rate(Some(2_000_000));
		assert_eq!(bandwidth.changed().await.unwrap(), Some(2_000_000));
	}
}
