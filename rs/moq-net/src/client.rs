use crate::driver::Transport;
use crate::origin;
use crate::time::{Deadlines, Instant};
use crate::transport::poll::Session as _;
use crate::{
	ALPN_18, ALPN_20, ALPN_22, ALPN_LITE, ALPN_LITE_03, ALPN_LITE_04, ALPN_LITE_05, ALPN_LITE_06, ALPN_LITE_07_WIP,
	Consume, Error, NEGOTIATED, Session, Version, Versions,
	coding::{self, Encode, Stream},
	ietf, lite, setup, stats,
};

/// A MoQ client session builder.
#[derive(Default, Clone)]
pub struct Client {
	publish: Option<origin::Consumer>,
	subscribe: Option<origin::Producer>,
	stats: stats::Session,
	versions: Versions,
	url: Option<url::Url>,
	setup_token: Option<setup::Token>,
	cost: Option<u64>,
	peer_hop: Option<crate::Hop>,
	limits: crate::session::Limits,
	extensions: setup::Extensions,
	auth: Option<crate::auth::Handle>,
	allocator: Option<crate::bandwidth::Allocator>,
	implementation: Option<String>,
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
	/// traffic counters are attributed through the model for this session's lifetime,
	/// and how the session ended is recorded there.
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

	/// Cap what the server can make this session hold. Defaults to [`session::Limits::default`](crate::session::Limits::default).
	pub fn with_limits(mut self, limits: crate::session::Limits) -> Self {
		self.limits = limits;
		self
	}

	/// Share `allocator`'s reservations with every session this client dials.
	///
	/// Each session probes for what they add up to, and its
	/// [`Session::allocator`](crate::Session::allocator) reserves into the same registry,
	/// so a reservation taken on one session still counts after a reconnect. Without
	/// one, each session starts an empty registry of its own.
	pub fn with_allocator(mut self, allocator: crate::bandwidth::Allocator) -> Self {
		self.allocator = Some(allocator);
		self
	}

	/// The registry the next session probes for.
	fn allocator(&self) -> crate::bandwidth::Allocator {
		self.allocator.clone().unwrap_or_default()
	}

	/// Restrict which protocol versions to offer, in preference order.
	/// Defaults to every version this crate supports.
	pub fn with_versions(mut self, versions: Versions) -> Self {
		self.versions = versions;
		self
	}

	/// Name the URL each session is dialed at, so the server learns the request target.
	///
	/// A transport whose request URI already reaches the server
	/// ([`Address::Uri`](crate::transport::Address::Uri)) sends nothing more. Otherwise
	/// SETUP carries the path and query (moq-lite-05 and newer, every moq-transport
	/// draft), which is how a credential in the query (`?jwt=`) arrives, and for
	/// `moqt://` the authority (moq-transport only). A `unix://` URL names its target in
	/// its `?path=` query, since its path is the socket file.
	pub fn with_url(mut self, url: url::Url) -> Self {
		self.url = Some(url);
		self
	}

	/// The request target our SETUP carries: the dialed URL's path, plus `?` and the
	/// query when there is one (draft-ietf-moq-transport-22, section 9.1.2).
	///
	/// `None` on a transport that carries its own request URI, where a SETUP path is a
	/// protocol violation, and when the target is empty, which means the server's
	/// default path: a peer on published lite-05 rejects an empty value outright.
	fn setup_path(&self, address: crate::transport::Address) -> Option<String> {
		let url = self
			.url
			.as_ref()
			.filter(|_| address == crate::transport::Address::Setup)?;

		let target = match url.scheme() {
			// One form-encoded value, so a target with its own `?query` arrives percent-encoded.
			"unix" => url.query_pairs().find(|(k, _)| k == "path")?.1.into_owned(),
			// A trailing `?` parses as an empty query, which is not a query: appending it
			// would spell one target two ways.
			_ => match url.query().filter(|query| !query.is_empty()) {
				Some(query) => format!("{}?{}", url.path(), query),
				None => url.path().to_owned(),
			},
		};

		(!target.is_empty()).then_some(target)
	}

	/// The URI authority our SETUP carries, which a raw-QUIC `moqt://` client must send
	/// (draft-ietf-moq-transport-22, 9.1.1).
	///
	/// Built from the host and port so URL userinfo never goes on the wire. `None` for
	/// every other scheme, on a transport that carries its own request URI, and when the
	/// URL has no host.
	fn setup_authority(&self, address: crate::transport::Address) -> Option<String> {
		let url = self
			.url
			.as_ref()
			.filter(|_| address == crate::transport::Address::Setup)?;
		if url.scheme() != "moqt" {
			return None;
		}

		let host = url.host_str().filter(|host| !host.is_empty())?;
		Some(match url.port() {
			Some(port) => format!("{host}:{port}"),
			None => host.to_owned(),
		})
	}

	/// Present `token` as the connection credential in SETUP, on versions that carry one
	/// (moq-lite-07+ and every moq-transport draft we speak). The session's empty-token
	/// AUTH stream then answers with its grant.
	///
	/// Other versions send nothing, so a caller that may negotiate one also puts the
	/// token in the URL query. moq-lite carries no Token Type, so only
	/// [`Token::OUT_OF_BAND`](setup::Token::OUT_OF_BAND) reaches it; another type fails
	/// the handshake.
	pub fn with_token(mut self, token: setup::Token) -> Self {
		self.setup_token = Some(token);
		self
	}

	/// Build every session on `auth`, so each presents the tokens it holds and a route
	/// naming one of its [`Credential`](crate::auth::Credential)s keeps it across a reconnect. Omit to
	/// give each session a handle of its own, which ends with it.
	///
	/// The handle backs one active session at a time, plus the ones draining after a
	/// GOAWAY: connecting another while one is still active fails with
	/// [`Error::Duplicate`].
	pub fn with_auth(mut self, auth: crate::auth::Handle) -> Self {
		self.auth = Some(auth);
		self
	}

	/// Choose which extensions to offer in SETUP. Defaults to every moq-transport one, and
	/// not [`ietf`](setup::Extensions::ietf), which only relays turn on.
	pub fn with_extensions(mut self, extensions: setup::Extensions) -> Self {
		self.extensions = extensions;
		self
	}

	/// Set the implementation to advertise in SETUP, as space-separated `name/version`
	/// product tokens, outermost first. Defaults to [`IMPLEMENTATION`](crate::IMPLEMENTATION);
	/// an empty string advertises nothing.
	pub fn with_implementation(mut self, implementation: impl Into<String>) -> Self {
		self.implementation = Some(implementation.into());
		self
	}

	/// The Implementation our SETUP carries, or `None` to omit it.
	fn implementation(&self) -> Option<String> {
		setup::implementation(self.implementation.as_deref().unwrap_or(crate::IMPLEMENTATION))
	}

	/// Price this link, in the units the rest of the mesh uses (moq-lite-06+, and
	/// moq-transport via the MoQ Cluster extension).
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
	pub fn with_cost(mut self, cost: impl Into<crate::origin::Cost>) -> Self {
		self.cost = Some(cost.into().value());
		self
	}

	/// Assign the identity this peer's routes are attributed to, overriding the fresh
	/// per-dial default.
	///
	/// A dialed session whose peer declares no hop gets a random one for that
	/// connection, the same way an accepted session does. That keeps a route
	/// learned from the peer off the session that learned it, and keeps a
	/// SUBSCRIBE that arrives on it from being routed back to it. An identity the
	/// peer declares on the wire still wins.
	///
	/// Pass an id only for a peer whose identity the caller has actually
	/// established. Two dials given the same hop are treated as one endpoint:
	/// routes learned from either are kept off both, and content arriving on
	/// either is interchangeable with the other's. That is the point when they
	/// really are one peer reconnecting or running redundant links, and a bug
	/// otherwise. Derive it from the authenticated identity, never from something
	/// coarser like the remote address.
	pub fn with_peer_hop(mut self, hop: crate::Hop) -> Self {
		self.peer_hop = Some(hop);
		self
	}

	/// The hop this connection attributes the peer to when the peer declares none.
	///
	/// A caller-supplied id is stable across dials of this client. Otherwise each
	/// connection gets a fresh one, matching the per-session default an accepted
	/// session already has.
	fn assigned_hop(&self) -> crate::Hop {
		self.peer_hop.unwrap_or_else(crate::Hop::random)
	}

	/// The new session's side of its auth handle: the one from [`with_auth`](Self::with_auth),
	/// or one of its own. `requests` for moq-transport, where a request carries a token.
	fn link(&self, supported: bool, requests: bool) -> Result<crate::auth::Link, Error> {
		match &self.auth {
			Some(handle) => crate::auth::Link::attach(handle, supported, requests),
			None if requests => Ok(crate::auth::Link::ietf(supported)),
			None => Ok(crate::auth::Link::new(supported)),
		}
	}

	/// The origin pair a session attaches, tagged and filtered.
	///
	/// Reads through the publish (egress) consumer and writes through the
	/// subscribe (ingress) producer are attributed by the model through the
	/// stats context; one shared context, so presence and viewer counts are
	/// never double-attributed across the two halves. `peer_hop` is the identity
	/// this connection attributes the peer to when the peer declares none, so
	/// subscriptions from the peer resolve to a source whose route excludes it.
	/// A declared identity still wins inside each publisher; announce filtering
	/// is per-protocol and handled there too.
	fn origins(&self, peer_hop: crate::Hop) -> (Option<origin::Consumer>, Option<origin::Producer>) {
		if self.publish.is_none() && self.subscribe.is_none() {
			tracing::warn!("not publishing or consuming anything");
		}
		let publish = self.publish.clone().map(|origin| origin.with_stats(self.stats.clone()));
		let subscribe = self
			.subscribe
			.clone()
			.map(|origin| origin.with_stats(self.stats.clone()));
		let publish = publish.map(|origin| origin.excluding(peer_hop));
		(publish, subscribe)
	}

	/// Start a lite session on an already-negotiated version: build our SETUP,
	/// wire the origins, and return the session and its driver.
	fn start_lite(
		&self,
		runtime: Deadlines,
		session: Transport,
		version: lite::Version,
		peer_hop: crate::Hop,
	) -> Result<(Session, crate::Driver), Error> {
		let (publish, subscribe) = self.origins(peer_hop);

		// Advertise our capabilities (we report what the transport measures; we
		// don't pad) plus the request path on URI-less transports, and the
		// direction we intend to use so the server can reject a token that lacks
		// the matching scope during the handshake instead of silently carrying
		// no media. Versions without a Setup Stream have nothing to advertise.
		let our_setup = if version.has_setup_stream() {
			lite::Setup {
				probe: lite::ProbeLevel::detect(&session),
				path: self.setup_path(session.address()),
				role: lite::Role::from_origins(self.publish.is_some(), self.subscribe.is_some()),
				cost: self.cost,
				// Filled by `lite::start` from the attached origin handles.
				hop: None,
				// Older versions carry the credential in the URL query instead.
				token: self.setup_token.clone().filter(|_| version.has_auth()),
				ietf: self.extensions.ietf && version.has_ietf(),
				implementation: self.implementation(),
			}
		} else {
			lite::Setup::default()
		};

		let start = lite::start(lite::Config {
			runtime: runtime.clone(),
			stats: self.stats.clone(),
			client: true,
			limits: self.limits,
			session: session.clone(),
			setup_stream: None,
			publish,
			subscribe,
			peer_hop: Some(peer_hop),
			version,
			our_setup,
			peer_setup: None,
			auth: self.link(version.has_auth(), false)?,
		})?;

		Ok(Session::new(
			runtime,
			session,
			version.into(),
			start.recv_bandwidth,
			crate::driver::Protocol::Lite(Box::new(start.driver)),
			crate::session::Handles {
				stats: self.stats.clone(),
				goaway: start.goaway,
				auth: start.auth,
				credential: start.credential,
				estimate: Some(start.estimate),
				bandwidth: self.allocator(),
				peer_implementation: start.peer_implementation,
			},
		))
	}

	/// Perform the MoQ handshake, returning the [`Session`] and its [`Driver`](crate::Driver).
	///
	/// Poll the returned driver with nondecreasing time, starting at `now`.
	pub fn connect(
		&self,
		now: Instant,
		session: impl crate::transport::poll::Boxable,
	) -> impl Future<Output = Result<(Session, crate::Driver), Error>> {
		// Box before any protocol code runs, so it is compiled once rather than per transport.
		self.connect_boxed(now, Transport::new(session))
	}

	async fn connect_boxed(&self, now: Instant, mut session: Transport) -> Result<(Session, crate::Driver), Error> {
		let runtime = Deadlines::new(now);
		let peer_hop = self.assigned_hop();
		let (publish, subscribe) = self.origins(peer_hop);

		let supported = match session.protocol() {
			Some(alpn @ (ALPN_22 | ALPN_20 | ALPN_18)) => {
				let draft = match alpn {
					ALPN_22 => ietf::Version::Draft22,
					ALPN_20 => ietf::Version::Draft20,
					_ => ietf::Version::Draft18,
				};

				let v = self.versions.select(Version::Ietf(draft)).ok_or(Error::Version)?;

				// SETUP is exchanged by the connection driver.
				// We advertise the request path in our SETUP for URL-less transports.
				// The peer's SETUP decides whether AUTH is negotiated. A handle that does not
				// offer it keeps the option out of our SETUP and presents no credential.
				let auth = self.link(self.extensions.auth, true)?;
				let peer_implementation = setup::PeerImplementation::default();
				let (protocol, goaway, credential) = ietf::start(ietf::Config {
					runtime: runtime.clone(),
					stats: self.stats.clone(),
					limits: self.limits,
					session: session.clone(),
					client: true,
					publish: publish.clone(),
					subscribe: subscribe.clone(),
					peer_hop: Some(peer_hop),
					cost: self.cost,
					version: draft,
					path: self.setup_path(session.address()),
					authority: self.setup_authority(session.address()),
					token: self.setup_token.clone(),
					peer_setup_stream: None,
					peer_declared: None,
					auth: auth.clone(),
					early_unis: Vec::new(),
					extensions: self.extensions,
					implementation: self.implementation(),
					peer_implementation: peer_implementation.clone(),
				})?;

				tracing::debug!(version = ?v, "connected");
				return Ok(Session::new(
					runtime,
					session,
					v,
					None,
					crate::driver::Protocol::Ietf(protocol),
					crate::session::Handles {
						stats: self.stats.clone(),
						goaway,
						auth,
						credential,
						estimate: None,
						bandwidth: self.allocator(),
						peer_implementation,
					},
				));
			}
			Some(alpn @ (ALPN_LITE_05 | ALPN_LITE_06 | ALPN_LITE_07_WIP)) => {
				let version = match alpn {
					ALPN_LITE_07_WIP => lite::Version::Lite07,
					ALPN_LITE_06 => lite::Version::Lite06,
					_ => lite::Version::Lite05,
				};
				self.versions.select(Version::Lite(version)).ok_or(Error::Version)?;
				return self.start_lite(runtime, session, version, peer_hop);
			}
			Some(ALPN_LITE_04) => {
				self.versions
					.select(Version::Lite(lite::Version::Lite04))
					.ok_or(Error::Version)?;
				return self.start_lite(runtime, session, lite::Version::Lite04, peer_hop);
			}
			Some(ALPN_LITE_03) => {
				self.versions
					.select(Version::Lite(lite::Version::Lite03))
					.ok_or(Error::Version)?;
				return self.start_lite(runtime, session, lite::Version::Lite03, peer_hop);
			}
			// No ALPN, or `moql`: negotiate moq-lite in a bidi SETUP.
			Some(ALPN_LITE) | None => self.versions.filter(&NEGOTIATED.into()).ok_or(Error::Version)?,
			Some(p) => return Err(Error::UnknownAlpn(p.to_string())),
		};

		// The SETUP keeps lite-01's framing whichever version it offers.
		let mut stream = Stream::open(&mut session, Version::Lite(lite::Version::Lite01))
			.await?
			.with_stats(&self.stats);

		let client = setup::Client {
			versions: supported.clone().into(),
			parameters: lite::Parameters::default().encode_bytes(lite::Version::Lite01)?,
		};

		stream.writer.encode(&client).await?;

		let server: setup::Server = stream.reader.decode().await?;

		let version = supported
			.iter()
			.find(|v| coding::Version::from(**v) == server.version)
			.copied()
			.ok_or(Error::Version)?;
		let Version::Lite(v) = version else {
			return Err(Error::Version);
		};

		// The server ends the stream after its SETUP. Reading to the end first keeps the drop
		// from stopping a reply the server is still waiting to have acknowledged.
		if version.hands_off() {
			kio::wait(|waiter| stream.reader.poll_closed(waiter)).await?;
			drop(stream);
			return self.start_lite(runtime, session, v, peer_hop);
		}

		let stream = stream.with_version(v);
		let start = lite::start(lite::Config {
			runtime: runtime.clone(),
			stats: self.stats.clone(),
			client: true,
			limits: self.limits,
			session: session.clone(),
			setup_stream: Some(stream),
			publish: publish.clone(),
			subscribe: subscribe.clone(),
			peer_hop: Some(peer_hop),
			version: v,
			// Lite 01/02 continue on the bidi SETUP stream and have no Setup Stream.
			our_setup: lite::Setup::default(),
			peer_setup: None,
			// Lite 01/02 carry no AUTH.
			auth: self.link(v.has_auth(), false)?,
		})?;

		Ok(Session::new(
			runtime,
			session,
			version,
			start.recv_bandwidth,
			crate::driver::Protocol::Lite(Box::new(start.driver)),
			crate::session::Handles {
				stats: self.stats.clone(),
				goaway: start.goaway,
				auth: start.auth,
				credential: start.credential,
				estimate: Some(start.estimate),
				bandwidth: self.allocator(),
				peer_implementation: start.peer_implementation,
			},
		))
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

	use std::task::Poll;

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

	/// Only this side ever closes a fake session, and like QUIC it says only that.
	impl crate::transport::Error for FakeError {
		fn close_kind(&self) -> crate::transport::CloseKind {
			crate::transport::CloseKind::Closed
		}
		fn session_error(&self) -> Option<(u32, String)> {
			None
		}
	}

	#[derive(Clone, Default)]
	struct FakeSession {
		state: Arc<FakeSessionState>,
		/// The sender, set by [`Self::with_probe`].
		probe: Option<crate::transport::Probe>,
	}

	#[derive(Default)]
	struct FakeSessionState {
		panic_on_poll: std::sync::atomic::AtomicBool,
		protocol: Option<&'static str>,
		control_stream: Mutex<Option<(FakeSendStream, FakeRecvStream)>>,
		close_events: kio::Shared<Vec<(u32, String)>>,
		control_writes: Arc<Mutex<Vec<u8>>>,
		send_rate: Mutex<Option<u64>>,
		wire: crate::transport::Wire,
	}

	fn any_close(events: &kio::Ref<'_, Vec<(u32, String)>>) -> Poll<()> {
		match events.is_empty() {
			true => Poll::Pending,
			false => Poll::Ready(()),
		}
	}

	impl FakeSession {
		fn new(protocol: Option<&'static str>, server_control_bytes: Vec<u8>) -> Self {
			let writes = Arc::new(Mutex::new(Vec::new()));
			let send = FakeSendStream { writes: writes.clone() };
			let recv = crate::transport::poll::Buffered::new(FakeChunks {
				data: VecDeque::from(server_control_bytes),
			});
			let state = FakeSessionState {
				panic_on_poll: Default::default(),
				protocol,
				control_stream: Mutex::new(Some((send, recv))),
				close_events: kio::Shared::default(),
				control_writes: writes,
				send_rate: Mutex::new(None),
				wire: crate::transport::Wire::new(),
			};
			Self {
				state: Arc::new(state),
				probe: None,
			}
		}

		fn set_send_rate(&self, rate: Option<u64>) {
			*self.state.send_rate.lock().unwrap() = rate;
		}

		/// Give the transport a sender, so the session hands it a target.
		fn with_probe(mut self) -> Self {
			self.probe = Some(crate::transport::Probe::new());
			self
		}

		/// What the session last handed the sender, in bits per second.
		fn probe_target(&self) -> u64 {
			self.probe.as_ref().expect("built with_probe").target().as_bps()
		}

		fn set_sent_bytes(&self, bytes: u64) {
			self.state.wire.set_sent_bytes(bytes);
		}

		fn control_writes(&self) -> Vec<u8> {
			self.state.control_writes.lock().unwrap().clone()
		}

		async fn wait_for_first_close(&self) -> (u32, String) {
			let events = self.state.close_events.wait(any_close).await;
			events[0].clone()
		}
	}

	impl crate::transport::poll::Session for FakeSession {
		type SendStream = FakeSendStream;
		type RecvStream = FakeRecvStream;
		type Error = FakeError;

		fn poll_accept_uni(&self, _waiter: &kio::Waiter) -> Poll<Result<Self::RecvStream, Self::Error>> {
			Poll::Pending
		}

		fn poll_accept_bi(
			&self,
			_waiter: &kio::Waiter,
		) -> Poll<Result<(Self::SendStream, Self::RecvStream), Self::Error>> {
			Poll::Pending
		}

		fn poll_open_bi(
			&self,
			_waiter: &kio::Waiter,
			_: crate::transport::poll::SendOptions<'_, crate::transport::poll::Ungrouped>,
		) -> Poll<Result<(Self::SendStream, Self::RecvStream), Self::Error>> {
			Poll::Ready(self.state.control_stream.lock().unwrap().take().ok_or(FakeError))
		}

		fn poll_open_request(
			&self,
			waiter: &kio::Waiter,
			options: crate::transport::poll::SendOptions<
				'_,
				<Self::SendStream as crate::transport::poll::SendStream>::Group,
			>,
		) -> std::task::Poll<Result<Option<crate::transport::poll::BiStreams<Self>>, Self::Error>> {
			self.poll_open_bi(waiter, options).map_ok(Some)
		}

		fn poll_open_uni(
			&self,
			_waiter: &kio::Waiter,
			_: crate::transport::poll::SendOptions<'_, crate::transport::poll::Ungrouped>,
		) -> Poll<Result<Self::SendStream, Self::Error>> {
			Poll::Pending
		}

		fn send_group(&self, _: i64) -> Option<crate::transport::poll::Ungrouped> {
			None
		}

		fn send_datagram(
			&self,
			_: Bytes,
			_: crate::transport::poll::SendOptions<'_, crate::transport::poll::Ungrouped>,
		) -> Result<(), Self::Error> {
			Ok(())
		}

		fn poll_recv_datagram(&self, _waiter: &kio::Waiter) -> Poll<Result<Bytes, Self::Error>> {
			Poll::Pending
		}

		fn max_datagram_size(&self) -> usize {
			1200
		}

		fn address(&self) -> crate::transport::Address {
			crate::transport::Address::Setup
		}
		fn protocol(&self) -> Option<&str> {
			self.state.protocol
		}

		fn close(&self, code: u32, reason: &str) {
			self.state.close_events.lock().push((code, reason.to_string()));
		}

		fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Self::Error> {
			assert!(
				!self
					.state
					.panic_on_poll
					.swap(false, std::sync::atomic::Ordering::Relaxed),
				"injected driver poll panic"
			);
			let _ = std::task::ready!(self.state.close_events.poll(waiter, any_close));
			Poll::Ready(FakeError)
		}

		fn metrics(&self) -> impl crate::transport::Metrics {
			FakeMetrics {
				send_rate: *self.state.send_rate.lock().unwrap(),
			}
		}

		fn wire(&self) -> Option<crate::transport::Wire> {
			Some(self.state.wire.clone())
		}

		fn probe(&self) -> Option<crate::transport::Probe> {
			self.probe.clone()
		}
	}

	struct FakeMetrics {
		send_rate: Option<u64>,
	}

	impl crate::transport::Metrics for FakeMetrics {
		fn estimated_send_rate(&self) -> Option<u64> {
			self.send_rate
		}
	}

	#[derive(Clone, Default)]
	struct FakeSendStream {
		writes: Arc<Mutex<Vec<u8>>>,
	}

	impl crate::transport::poll::SendStream for FakeSendStream {
		type Error = FakeError;
		type Group = crate::transport::poll::Ungrouped;

		fn poll_write(&mut self, _waiter: &kio::Waiter, buf: &[u8]) -> Poll<Result<usize, Self::Error>> {
			self.writes.lock().unwrap().put_slice(buf);
			Poll::Ready(Ok(buf.len()))
		}

		fn set_priority(&mut self, _order: i64) {}

		fn set_group(&mut self, group: &Self::Group) {
			match *group {}
		}

		fn finish(&mut self) -> Result<(), Self::Error> {
			Ok(())
		}

		fn reset(&mut self, _code: u32) {}

		fn poll_closed(&self, _waiter: &kio::Waiter) -> Poll<Result<(), Self::Error>> {
			Poll::Ready(Ok(()))
		}
	}

	type FakeRecvStream = crate::transport::poll::Buffered<FakeChunks>;

	struct FakeChunks {
		data: VecDeque<u8>,
	}

	impl crate::transport::poll::Chunks for FakeChunks {
		type Error = FakeError;

		fn poll_read_chunk(&mut self, _waiter: &kio::Waiter, max: usize) -> Poll<Result<Option<Bytes>, Self::Error>> {
			if self.data.is_empty() {
				return Poll::Ready(Ok(None));
			}

			let size = max.min(self.data.len());
			Poll::Ready(Ok(Some(self.data.drain(..size).collect())))
		}

		fn stop(&mut self, _code: u32) {}
	}

	fn mock_server_setup(negotiated: Version) -> Vec<u8> {
		let mut encoded = Vec::new();
		let server = setup::Server {
			version: negotiated.into(),
			parameters: Bytes::new(),
		};
		server
			.encode(
				&mut crate::coding::Encoder::new(&mut encoded, negotiated.into()),
				negotiated,
			)
			.unwrap();

		// Add a setup-stream SessionInfo frame using the negotiated Lite version.
		let info = lite::SessionInfo { bitrate: Some(1) };
		let lite_v = lite::Version::try_from(negotiated).unwrap();
		info.encode(&mut crate::coding::Encoder::new(&mut encoded, lite_v.into()), lite_v)
			.unwrap();

		encoded
	}

	async fn run_alpn_lite_fallback_case(protocol: Option<&'static str>) {
		let fake = FakeSession::new(protocol, mock_server_setup(Version::Lite(lite::Version::Lite01)));
		let client = Client::new().with_versions(
			[
				Version::Lite(lite::Version::Lite03),
				Version::Lite(lite::Version::Lite02),
				Version::Lite(lite::Version::Lite01),
			]
			.into(),
		);

		// Start the returned driver after the handshake completes.
		let (_session, driver) = client.connect(kio::clock::now(), fake.clone()).await.unwrap();
		kio::clock::sim::spawn(crate::time::run(driver));

		// Verify the client offered every configured moq-lite version over the `moql` SETUP.
		let mut setup_bytes = Bytes::from(fake.control_writes());
		let setup = crate::coding::decode_buf(&mut setup_bytes, Version::Lite(lite::Version::Lite02), |r, v| {
			setup::Client::decode(r, v)
		})
		.unwrap();
		let advertised: Vec<Version> = setup.versions.iter().map(|v| Version::try_from(*v).unwrap()).collect();
		assert_eq!(
			advertised,
			vec![
				Version::Lite(lite::Version::Lite03),
				Version::Lite(lite::Version::Lite02),
				Version::Lite(lite::Version::Lite01),
			]
		);

		// The first close comes from the lite connection driver.
		// Any non-Version error here means SessionInfo decoded successfully
		// after set_version(). This test cares about the SETUP framing
		// fallback, not the specific close code. Cancel is what we'd see
		// with no origin; a protocol violation (or similar) is what an
		// auto-created origin's first interaction with a Lite01 peer trips.
		let (code, _) = fake.wait_for_first_close().await;
		// Session closes encode through the session registry, so compare against that one.
		assert_ne!(code, SessionError::Version.to_code(), "SessionInfo failed to decode");
	}

	/// `connect` must not depend on the peer answering. A peer that opens the announce
	/// stream and then says nothing (or promises a count it never delivers) used to hold
	/// `connect` for the life of the session, since it waited for the initial announce
	/// set. Resolving a path you need is `request`'s job, which waits for
	/// that path rather than for the peer to finish talking.
	#[kio::test]
	async fn connect_does_not_wait_for_the_peer_to_announce() {
		// Serves bidi streams, so the announce stream opens, and never answers on them.
		let gate = kio::Producer::new(true);
		let transport = crate::lite::test_transport::SinkSession::gated_bi(gate.consume())
			.with_protocol(crate::version::ALPN_LITE_05);

		// A subscribe origin is what makes the client open an announce stream at all.
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let client = Client::new()
			.with_versions([Version::Lite(lite::Version::Lite05)].into())
			.with_subscriber(origin);

		// Paused time auto-advances while every task is idle, so a `connect` that waits
		// on the silent peer trips this rather than hanging the suite.
		let (_session, _driver) = kio::clock::timeout(
			std::time::Duration::from_secs(30),
			client.connect(kio::clock::now(), transport),
		)
		.await
		.expect("connect waited on a peer that never announced")
		.expect("connect failed");
	}

	fn dialed(url: &str) -> Client {
		Client::new().with_url(url.parse().unwrap())
	}

	#[test]
	fn setup_path_is_the_request_target() {
		// An empty path and an absent one both mean the server's default, so we send
		// neither. A peer on published lite-05 rejects an empty value outright.
		let cases = [
			("unix:///run/moq.sock?path=/room", Some("/room")),
			// The whole resource path is one form-encoded value, so a `?query` inside it
			// arrives percent-encoded and comes back out whole.
			("unix:///run/moq.sock?path=/room%3Fjwt%3Dabc", Some("/room?jwt=abc")),
			("unix:///run/moq.sock?path=", None),
			("unix:///run/moq.sock", None),
			("tcp://localhost:4443/room?jwt=abc", Some("/room?jwt=abc")),
			("tcp://localhost:4443", None),
			("moqt://relay.example.com/anon?jwt=abc", Some("/anon?jwt=abc")),
			("moql://relay.example.com/anon", Some("/anon")),
			("moqt://relay.example.com", None),
			(
				"iroh://k5lnrlndqpqcgh4d5nhbnbnhcyrgvw6ttxwrsvsu4nlt6foorxaa/",
				Some("/"),
			),
			// The fragment is processed by the client and never sent (draft-22 6.1.1).
			("moqt://relay.example.com/anon?jwt=abc#pos:12", Some("/anon?jwt=abc")),
			// A trailing `?` is an empty query, not a query.
			("moqt://relay.example.com/anon?", Some("/anon")),
			("moqt://relay.example.com?", None),
		];

		for (url, want) in cases {
			let client = dialed(url);
			assert_eq!(
				client.setup_path(crate::transport::Address::Setup).as_deref(),
				want,
				"{url}"
			);
			// The transport's own request URI carries the target: a SETUP path is a violation.
			assert_eq!(client.setup_path(crate::transport::Address::Uri), None, "{url}");
		}
		assert_eq!(Client::new().setup_path(crate::transport::Address::Setup), None);
	}

	#[test]
	fn setup_authority_is_only_for_moqt() {
		let cases = [
			("moqt://relay.example.com/anon?jwt=abc", Some("relay.example.com")),
			("moqt://relay.example.com:4443/anon", Some("relay.example.com:4443")),
			("moqt://[::1]:4443", Some("[::1]:4443")),
			// Userinfo is a credential, and never goes on the wire.
			(
				"moqt://user:pass@relay.example.com:4443",
				Some("relay.example.com:4443"),
			),
			// A hostless URL has no authority to send.
			("moqt:///anon", None),
			// Only the draft's `moqt://` scheme is required to send it.
			("moql://relay.example.com", None),
			("tcp://relay.example.com:4443", None),
			("iroh://k5lnrlndqpqcgh4d5nhbnbnhcyrgvw6ttxwrsvsu4nlt6foorxaa", None),
		];

		for (url, want) in cases {
			let client = dialed(url);
			assert_eq!(
				client.setup_authority(crate::transport::Address::Setup).as_deref(),
				want,
				"{url}"
			);
			assert_eq!(client.setup_authority(crate::transport::Address::Uri), None, "{url}");
		}
	}

	#[kio::test]
	async fn alpn_lite_falls_back_to_moql_setup_and_switches_version_post_setup() {
		run_alpn_lite_fallback_case(Some(ALPN_LITE)).await;
	}

	#[kio::test]
	async fn no_alpn_falls_back_to_moql_setup_and_switches_version_post_setup() {
		run_alpn_lite_fallback_case(None).await;
	}

	// No executor is running: only explicitly polling the driver may process
	// a session close, and the driver must not retain a session handle.
	#[test]
	fn driver_is_caller_polled_and_holds_no_session() {
		let fake = FakeSession::new(Some(ALPN_LITE_04), Vec::new());
		let client = Client::new().with_versions(Version::Lite(lite::Version::Lite04).into());

		let runtime = crate::time::Deadlines::new(crate::time::Instant::now());
		let (session, mut driver) = futures::executor::block_on(client.connect(runtime.now(), fake.clone())).unwrap();
		assert_eq!(session.version(), Version::Lite(lite::Version::Lite04));

		// Construction leaves the driver idle until the caller polls it.
		assert!(driver.poll(runtime.now(), &kio::Waiter::noop()).is_ok());

		// The caller drops their only session clone; the machine observes the
		// last handle going away and closes the transport.
		drop(session);
		assert!(fake.state.close_events.read().is_empty());
		let _ = driver.poll(runtime.now(), &kio::Waiter::noop());
		assert_eq!(fake.state.close_events.read()[0].0, SessionError::Cancel.to_code());
	}

	// Clones share the connection: the transport closes on the LAST drop, and
	// abort() closes it explicitly (first close wins). Both are relayed through
	// the machine, so each takes a tick to land.
	#[test]
	fn session_clones_share_the_close() {
		let fake = FakeSession::new(Some(ALPN_LITE_04), Vec::new());
		let client = Client::new().with_versions(Version::Lite(lite::Version::Lite04).into());

		let runtime = crate::time::Deadlines::new(crate::time::Instant::now());
		let (session, mut driver) = futures::executor::block_on(client.connect(runtime.now(), fake.clone())).unwrap();
		let clone = session.clone();

		// One clone dropping does nothing while another is alive.
		drop(session);
		assert!(fake.state.close_events.read().is_empty());
		let _ = driver.poll(runtime.now(), &kio::Waiter::noop());
		assert!(fake.state.close_events.read().is_empty());

		clone.abort(Error::Cancel);
		let _ = driver.poll(runtime.now(), &kio::Waiter::noop());
		assert_eq!(fake.state.close_events.read()[0].0, SessionError::Cancel.to_code());

		// And the machine publishes the transport's terminal error, which is
		// what `closed()` reports.
		let _ = driver.poll(runtime.now(), &kio::Waiter::noop());
		futures::executor::block_on(clone.closed());

		// The final drop requests no second close: the handle-side close is once.
		let closes = fake.state.close_events.read().len();
		drop(clone);
		let _ = driver.poll(runtime.now(), &kio::Waiter::noop());
		assert_eq!(fake.state.close_events.read().len(), closes);
	}

	// A transport that only knows this side closed it leaves the reason to the session,
	// which reports it from `closed()` and records it in the session's stats.
	#[test]
	fn a_local_close_reports_this_sides_reason() {
		use crate::stats::{Failure, Failures, Registry, Tier};
		let ended = |abort: Error| {
			let registry = Registry::new(Default::default());
			let fake = FakeSession::new(Some(ALPN_LITE_04), Vec::new());
			let client = Client::new()
				.with_versions(Version::Lite(lite::Version::Lite04).into())
				.with_stats(registry.tier(Tier::default()).session(crate::Path::from_static("root")));
			let runtime = crate::time::Deadlines::new(crate::time::Instant::now());
			let (session, mut driver) = futures::executor::block_on(client.connect(runtime.now(), fake)).unwrap();
			session.abort(abort);
			// One poll closes the transport, the next publishes how it ended.
			let _ = driver.poll(runtime.now(), &kio::Waiter::noop());
			let _ = driver.poll(runtime.now(), &kio::Waiter::noop());
			let err = futures::executor::block_on(session.closed());
			drop((client, session, driver));
			(err, registry.snapshot().sessions()[0].1.sessions_failed)
		};

		let (err, failed) = ended(Error::Expired);
		assert!(matches!(err, Error::Expired), "{err:?}");
		assert_eq!(failed.count(Failure::Expired), 1);

		// Anything the peer is told is "no error" is a clean close.
		let (err, failed) = ended(Error::GoingAway);
		assert!(matches!(err, Error::Cancel), "{err:?}");
		assert_eq!(failed, Failures::default());
	}

	/// A task that closes the transport itself, as a protocol violation on one request does,
	/// is the session's reason even when the transport reports only a local close, and it
	/// wins over whatever closes it after.
	#[test]
	fn a_task_close_is_the_sessions_reason() {
		use crate::stats::{Failure, Registry, Tier};
		use crate::transport::poll::Session as _;
		let registry = Registry::new(Default::default());
		let client = Client::new()
			.with_versions(Version::Lite(lite::Version::Lite04).into())
			.with_stats(registry.tier(Tier::default()).session(crate::Path::from_static("root")));
		let runtime = crate::time::Deadlines::new(crate::time::Instant::now());
		let task = Transport::new(FakeSession::new(Some(ALPN_LITE_04), Vec::new()));
		let (session, mut driver) =
			futures::executor::block_on(client.connect_boxed(runtime.now(), task.clone())).unwrap();
		task.close(SessionError::ProtocolViolation.to_code(), "malformed");
		session.abort(Error::App(7));
		let _ = driver.poll(runtime.now(), &kio::Waiter::noop());
		let _ = driver.poll(runtime.now(), &kio::Waiter::noop());
		let err = futures::executor::block_on(session.closed());
		assert!(
			matches!(err, Error::Session(SessionError::ProtocolViolation)),
			"{err:?}"
		);
		drop((client, session, driver));
		let failed = registry.snapshot().sessions()[0].1.sessions_failed;
		assert_eq!(failed.count(Failure::Application), 1, "{failed:?}");
	}

	// Dropping the driver instead of running it tears the session
	// down: the machine was the only transport holder, and `closed()` resolves
	// rather than parking forever on a machine nobody polls.
	#[test]
	fn dropped_driver_resolves_closed() {
		for polled in [false, true] {
			let fake = FakeSession::new(Some(ALPN_LITE_04), Vec::new());
			let client = Client::new().with_versions(Version::Lite(lite::Version::Lite04).into());
			let runtime = crate::time::Deadlines::new(crate::time::Instant::now());
			let (session, mut driver) = futures::executor::block_on(client.connect(runtime.now(), fake)).unwrap();
			if polled {
				assert!(driver.poll(runtime.now(), &kio::Waiter::noop()).is_ok());
			}
			drop(driver);
			assert!(matches!(futures::executor::block_on(session.closed()), Error::Cancel));
		}
	}

	#[test]
	fn driver_panic_is_published_before_the_executor_drops_it() {
		let fake = FakeSession::new(Some(ALPN_LITE_04), Vec::new());
		let client = Client::new().with_versions(Version::Lite(lite::Version::Lite04).into());
		let runtime = crate::time::Deadlines::new(crate::time::Instant::now());
		let (session, mut driver) = futures::executor::block_on(client.connect(runtime.now(), fake.clone())).unwrap();
		fake.state
			.panic_on_poll
			.store(true, std::sync::atomic::Ordering::Relaxed);
		let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
			let _ = driver.poll(runtime.now(), &kio::Waiter::noop());
		}));
		assert!(panic.is_err(), "the owner must still receive the panic");
		assert!(matches!(
			futures::FutureExt::now_or_never(session.closed()),
			Some(Error::Transport(message)) if message == "session driver panicked"
		));
		// Even a caller that catches the panic cannot resume the damaged session.
		assert!(matches!(
			driver.poll(runtime.now(), &kio::Waiter::noop()),
			Err(Error::Transport(_))
		));
		drop(driver); // outside unwinding, as an executor's catch_unwind drops a failed future
		assert!(matches!(
			futures::executor::block_on(session.closed()),
			Error::Transport(_)
		));
	}

	// `stats()` reads the machine's latest sample and primes the sampler, so a
	// periodic poller observes fresh counters without consuming the bandwidth
	// channel.
	#[kio::test]
	async fn stats_reads_prime_the_sampler() {
		let fake = FakeSession::new(Some(ALPN_LITE_04), Vec::new());
		fake.set_send_rate(Some(1_000_000));

		let client = Client::new().with_versions(Version::Lite(lite::Version::Lite04).into());
		let (session, driver) = client.connect(kio::clock::now(), fake.clone()).await.unwrap();
		kio::clock::sim::spawn(crate::time::run(driver));

		// The construction-time snapshot, before the machine sampled anything.
		assert_eq!(
			session.stats().estimated_send_rate,
			Some(crate::bandwidth::Rate::from_bps(1_000_000))
		);

		// That read was demand: the machine keeps sampling while stats are read,
		// so the new rate shows up within an interval (paused time auto-advances).
		fake.set_send_rate(Some(2_000_000));
		while session.stats().estimated_send_rate != Some(crate::bandwidth::Rate::from_bps(2_000_000)) {
			kio::clock::sleep(std::time::Duration::from_millis(10)).await;
		}
	}

	// Sampling stops when the supervisor ends, but `stats()` keeps serving its
	// cell, so the last thing the supervisor does is take a final snapshot.
	// Without one, "what did that session move?" asked at teardown answers with
	// the construction-time snapshot: this backend reports no send rate, so
	// there is no bandwidth consumer keeping the sampler ticking, and the test
	// never reads stats while the session is live.
	#[kio::test]
	async fn stats_capture_the_final_counters() {
		let fake = FakeSession::new(Some(ALPN_LITE_04), Vec::new());
		fake.set_send_rate(None);
		fake.set_sent_bytes(0);

		let client = Client::new().with_versions(Version::Lite(lite::Version::Lite04).into());
		let (session, driver) = client.connect(kio::clock::now(), fake.clone()).await.unwrap();
		kio::clock::sim::spawn(crate::time::run(driver));
		assert!(
			session.send_bandwidth().is_none(),
			"no send-rate estimate, so nothing samples on its own"
		);

		fake.set_sent_bytes(4242);

		session.abort(Error::Cancel);
		session.closed().await;

		assert_eq!(
			session.stats().bytes_sent,
			Some(4242),
			"the closing snapshot must carry the session's final counters"
		);
	}

	// The send-bandwidth sampler lives inside the driver: it samples as soon as a
	// consumer exists and keeps sampling on its interval. Simulated time makes
	// the interval fire deterministically.
	#[kio::test]
	async fn send_bandwidth_samples_while_the_driver_runs() {
		let fake = FakeSession::new(Some(ALPN_LITE_04), Vec::new());
		fake.set_send_rate(Some(1_000_000));

		let client = Client::new().with_versions(Version::Lite(lite::Version::Lite04).into());
		let (session, driver) = client.connect(kio::clock::now(), fake.clone()).await.unwrap();
		let mut bandwidth = session.send_bandwidth().expect("backend reports an estimate");
		assert_eq!(
			bandwidth.peek(),
			Some(crate::bandwidth::Rate::from_bps(1_000_000)),
			"the estimate is readable before the sampler first runs"
		);
		kio::clock::sim::spawn(crate::time::run(driver));

		assert_eq!(
			bandwidth.changed().await.unwrap(),
			Some(crate::bandwidth::Rate::from_bps(1_000_000))
		);

		// A later change is picked up by the next interval tick.
		fake.set_send_rate(Some(2_000_000));
		assert_eq!(
			bandwidth.changed().await.unwrap(),
			Some(crate::bandwidth::Rate::from_bps(2_000_000))
		);
	}

	/// A track to reserve against, with the broadcast keeping it alive.
	fn reservable() -> (crate::broadcast::Producer, crate::track::Producer) {
		let broadcast = crate::broadcast::Info::default().produce();
		let track = broadcast.create_track("t", crate::track::Info::timed()).unwrap();
		(broadcast, track)
	}

	/// Run the driver until the sender's target reads `bps`.
	async fn probes_to(fake: &FakeSession, bps: u64) {
		while fake.probe_target() != bps {
			kio::clock::sleep(std::time::Duration::from_millis(1)).await;
		}
	}

	// The session probes for every reservation on its allocator, demanded or not: a
	// stopped rendition loses its viewers, and still has to be funded to resume.
	#[kio::test]
	async fn the_sender_probes_to_the_reservations() {
		let fake = FakeSession::new(Some(ALPN_LITE_04), Vec::new()).with_probe();
		fake.set_send_rate(Some(1_000_000));

		let client = Client::new().with_versions(Version::Lite(lite::Version::Lite04).into());
		let (session, driver) = client.connect(kio::clock::now(), fake.clone()).await.unwrap();
		kio::clock::sim::spawn(crate::time::run(driver));
		let bandwidth = session.allocator();

		let (_watched_broadcast, watched) = reservable();
		let _sub = watched.subscribe(None);
		let watched = bandwidth.reserve(&watched.demand(), crate::bandwidth::Rate::from_bps(4_000_000));
		probes_to(&fake, 4_000_000).await;

		let (_idle_broadcast, idle) = reservable();
		let idle = bandwidth.reserve(&idle.demand(), crate::bandwidth::Rate::from_bps(1_000_000));
		probes_to(&fake, 5_000_000).await;

		idle.update(crate::bandwidth::Rate::from_bps(2_000_000));
		probes_to(&fake, 6_000_000).await;

		drop(watched);
		probes_to(&fake, 2_000_000).await;
		drop(idle);
		probes_to(&fake, 0).await;
	}

	// An override replaces the sum, zero turns probing off, and clearing it follows the
	// reservations again.
	#[kio::test]
	async fn an_override_replaces_the_reservations() {
		let fake = FakeSession::new(Some(ALPN_LITE_04), Vec::new()).with_probe();
		let client = Client::new().with_versions(Version::Lite(lite::Version::Lite04).into());
		let (session, driver) = client.connect(kio::clock::now(), fake.clone()).await.unwrap();
		kio::clock::sim::spawn(crate::time::run(driver));
		let bandwidth = session.allocator();

		let (_broadcast, track) = reservable();
		let _reserved = bandwidth.reserve(&track.demand(), crate::bandwidth::Rate::from_bps(4_000_000));
		probes_to(&fake, 4_000_000).await;

		bandwidth.set_target(Some(crate::bandwidth::Rate::ZERO));
		probes_to(&fake, 0).await;

		bandwidth.set_target(Some(crate::bandwidth::Rate::from_bps(10_000_000)));
		probes_to(&fake, 10_000_000).await;

		bandwidth.set_target(None);
		probes_to(&fake, 4_000_000).await;
	}

	// A reconnecting caller dials every session with one allocator, so a reservation
	// taken before the reconnect still moves the next session's target.
	#[kio::test]
	async fn a_reconnect_keeps_following_the_reservations() {
		let bandwidth = crate::bandwidth::Allocator::default();
		let client = Client::new()
			.with_versions(Version::Lite(lite::Version::Lite04).into())
			.with_allocator(bandwidth.clone());

		let (_broadcast, track) = reservable();
		let reserved = bandwidth.reserve(&track.demand(), crate::bandwidth::Rate::from_bps(4_000_000));

		let first = FakeSession::new(Some(ALPN_LITE_04), Vec::new()).with_probe();
		let (session, driver) = client.connect(kio::clock::now(), first.clone()).await.unwrap();
		kio::clock::sim::spawn(crate::time::run(driver));
		probes_to(&first, 4_000_000).await;
		session.abort(Error::Cancel);
		session.closed().await;
		drop(session);

		let second = FakeSession::new(Some(ALPN_LITE_04), Vec::new()).with_probe();
		let (_session, driver) = client.connect(kio::clock::now(), second.clone()).await.unwrap();
		kio::clock::sim::spawn(crate::time::run(driver));
		probes_to(&second, 4_000_000).await;

		reserved.update(crate::bandwidth::Rate::from_bps(3_000_000));
		probes_to(&second, 3_000_000).await;
		assert_eq!(
			first.probe_target(),
			4_000_000,
			"a closed session's sender is left alone"
		);
	}
}
