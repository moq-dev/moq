use crate::{auth, cluster};

use axum::http;
use moq_tokio::server::Request;

/// An error carrying the HTTP status to send when closing the request.
///
/// Used only on the pre-accept auth path so the caller can close once with
/// the right code instead of sprinkling close/return at each failure site.
struct StatusError {
	status: http::StatusCode,
	source: anyhow::Error,
}

impl From<auth::Error> for StatusError {
	fn from(err: auth::Error) -> Self {
		Self {
			status: (&err).into(),
			source: err.into(),
		}
	}
}

/// An incoming connection that has not yet been admitted.
///
/// Build with [`new`](Self::new), attach the optional knobs, then call
/// [`run`](Self::run) to admit the request through its lease, wire up
/// publish/subscribe origins, and serve the session until it closes.
pub struct Connection {
	/// A numeric identifier for logging.
	id: u64,
	/// The raw QUIC/WebTransport request to accept or reject.
	request: Request,
	/// The cluster state used to resolve origins.
	cluster: cluster::Cluster,
	/// Where the session's grant comes from.
	auth: auth::Auth,
	/// Relay-wide shutdown broadcast: when it fires, the session is drained with
	/// a GOAWAY instead of being cut off.
	shutdown: crate::shutdown::Observer,
	/// Live sessions on this node, so an operator can list and nudge this one.
	sessions: Option<crate::session::Registry>,
}

impl Connection {
	/// Wrap an accepted request, resolving origins through `cluster` and
	/// its grant through `auth`.
	pub fn new(request: Request, cluster: cluster::Cluster, auth: auth::Auth) -> Self {
		Self {
			id: 0,
			request,
			cluster,
			auth,
			shutdown: crate::shutdown::Observer::disabled(),
			sessions: None,
		}
	}

	/// Set the identifier this connection logs under. Defaults to 0.
	pub fn with_id(mut self, id: u64) -> Self {
		self.id = id;
		self
	}

	/// Attach the relay-wide shutdown broadcast so the session drains with a
	/// GOAWAY when it fires. Without it the session is cut off on process exit.
	pub fn with_shutdown(mut self, shutdown: crate::shutdown::Observer) -> Self {
		self.shutdown = shutdown;
		self
	}

	/// Register this session in the node's live table after it is admitted.
	/// Without it the session is served but cannot be listed or nudged.
	pub fn with_sessions(mut self, sessions: crate::session::Registry) -> Self {
		self.sessions = Some(sessions);
		self
	}

	/// Admits and serves this connection until it closes.
	#[tracing::instrument("conn", skip_all, fields(id = self.id, remote = self.request.remote_addr().map(tracing::field::display), session = tracing::field::Empty))]
	pub async fn run(self) -> anyhow::Result<()> {
		let peer_hop = self.request.peer_hop();
		let (lease, registration) = match self.admit().await {
			Ok(admitted) => admitted,
			Err(err) => {
				let reject = match err.status {
					http::StatusCode::UNAUTHORIZED => moq_tokio::server::Reject::Unauthorized,
					http::StatusCode::FORBIDDEN => moq_tokio::server::Reject::Forbidden,
					status => moq_tokio::server::Reject::App(status.as_u16()),
				};
				let _ = self.request.reject(reject).await;
				return Err(err.source);
			}
		};

		let transport = self.request.transport();
		let role = self.request.role();
		let grants = match authorize(&self.cluster, lease.token(), role, &transport) {
			Ok(grants) => grants,
			Err(err) => {
				let _ = self.request.reject(moq_tokio::server::Reject::Forbidden).await;
				return Err(err);
			}
		};

		// Accept the connection.
		// NOTE: subscribe and publish seem backwards because of how relays work.
		// We publish the tracks the client is allowed to subscribe to.
		// We subscribe to the tracks the client is allowed to publish.
		//
		// moq-net defaults the unset side to a fresh no-op origin, which is fine for a
		// publish-only or subscribe-only session.
		let mut request = self.request.with_stats(grants.stats);
		if let Some(subscribe) = grants.subscribe {
			request = request.with_publisher(&subscribe);
		}
		if let Some(publish) = grants.publish {
			request = request.with_subscriber(publish);
		}
		let session = request.ok().await?;
		let _node_connection = peer_hop.map(|origin| self.cluster.nodes.connect_inbound(self.id, origin));

		tracing::info!(version = %session.version(), %transport, "negotiated");

		supervise(session, lease, self.shutdown.clone(), registration).await
	}

	/// Admit this connection. Any failure is returned as a [`StatusError`] so
	/// [`run`] can close the request with the mapped HTTP status exactly once.
	///
	/// Every transport goes through the same lease; the request the server sees
	/// carries what the transport knows. A LAN mesh dial is the one exception: its
	/// credential is a secret the relay minted for itself, checked locally.
	async fn admit(&self) -> Result<(auth::Lease, Option<crate::session::Registration>), StatusError> {
		// Checked first so a `/.cluster` request is never routed through the public
		// grant, and a relay without LAN discovery refuses it instead of treating the
		// path as a broadcast root.
		if cluster::Cluster::is_lan_path(self.request.path()) {
			return self.admit_lan();
		}

		let request = auth::request_for(&self.auth, &self.request);
		tracing::Span::current().record("session", &request.id);
		if self.request.peer_identity().is_some() {
			tracing::debug!("client certificate verified; reported to the auth server");
		}
		let lease = self.auth.admit(request.clone()).await?;
		let registration = self.sessions.as_ref().map(|sessions| sessions.register(request));
		Ok((lease, registration))
	}

	/// Authorize a `/.cluster/<credential>` dial against the live LAN advertisement.
	fn admit_lan(&self) -> Result<(auth::Lease, Option<crate::session::Registration>), StatusError> {
		let Some(presented) = cluster::Cluster::lan_credential(self.request.path()) else {
			return Err(StatusError {
				status: http::StatusCode::FORBIDDEN,
				source: anyhow::anyhow!("LAN peer did not present a membership proof"),
			});
		};
		match self.cluster.verify_lan_credential(presented) {
			Some(true) => {
				tracing::info!("accepted LAN peer");
				Ok((self.auth.admit_fixed("/", self.cluster.lan_peer_grant()), None))
			}
			Some(false) => Err(StatusError {
				status: http::StatusCode::FORBIDDEN,
				source: anyhow::anyhow!("LAN peer did not present this listener's membership proof"),
			}),
			None => Err(StatusError {
				status: http::StatusCode::FORBIDDEN,
				source: anyhow::anyhow!("/.cluster request refused: LAN discovery is not enabled"),
			}),
		}
	}
}

/// What an authorized session may serve: the token-scoped origin pair, pruned
/// to the advertised role, plus its stats context.
pub(crate) struct Grants {
	/// What the client may subscribe to (we publish it).
	pub(crate) publish: Option<moq_net::origin::Producer>,
	/// What the client may publish (we subscribe to it).
	pub(crate) subscribe: Option<moq_net::origin::Producer>,
	/// The session's billing/attribution context.
	pub(crate) stats: moq_net::stats::Session,
}

/// Authorize an admitted session and resolve what it may serve, however
/// its transport is driven (the shared runtime or a QUIC worker).
///
/// The client advertises which direction it intends to use (moq-lite-05
/// SETUP). A bidirectional connection (e.g. a cluster peer) advertises
/// nothing, so the only requirement is that the token grants *something*. But
/// a gateway that only publishes or only subscribes says so, and a token
/// missing that direction's scope is rejected here during the handshake,
/// instead of being accepted and then silently carrying no media (the bug
/// that motivated the role hint).
pub(crate) fn authorize(
	cluster: &cluster::Cluster,
	token: &auth::Token,
	role: Option<moq_net::Role>,
	transport: &dyn std::fmt::Display,
) -> anyhow::Result<Grants> {
	let publish = cluster.publisher(token);
	let subscribe = cluster.subscriber(token);

	let authorized = match role {
		Some(moq_net::Role::Publisher) => publish.is_some(),
		Some(moq_net::Role::Subscriber) => subscribe.is_some(),
		// Bidirectional or an unrecognized future role: require the token to grant
		// something, and let the per-direction checks apply once it's used.
		None | Some(_) => publish.is_some() || subscribe.is_some(),
	};
	if !authorized {
		let wanted = role.map_or("any", moq_net::Role::as_str);
		anyhow::bail!("grant does not allow {wanted} access to {}", token.root);
	}

	match (&publish, &subscribe) {
		(Some(publish), Some(subscribe)) => {
			tracing::info!(%transport, ?role, tier = %token.tier, root = %token.root, publish = %publish.allowed().iter().map(|pattern| pattern.as_str()).collect::<Vec<_>>().join(","), subscribe = %subscribe.allowed().iter().map(|pattern| pattern.as_str()).collect::<Vec<_>>().join(","), "session accepted");
		}
		(Some(publish), None) => {
			tracing::info!(%transport, ?role, tier = %token.tier, root = %token.root, publish = %publish.allowed().iter().map(|pattern| pattern.as_str()).collect::<Vec<_>>().join(","), "publisher accepted");
		}
		(None, Some(subscribe)) => {
			tracing::info!(%transport, ?role, tier = %token.tier, root = %token.root, subscribe = %subscribe.allowed().iter().map(|pattern| pattern.as_str()).collect::<Vec<_>>().join(","), "subscriber accepted");
		}
		_ => unreachable!("authorized above guarantees at least one origin"),
	}

	// Build this session's stats context under its billing tier and auth root.
	// The context carries the presence gauge (a client that merely connects to
	// e.g. `/acme` is counted, even idle) and drives the model-layer counters
	// once it tags the session's origin pair. It closes when the last clone
	// drops (the connection ends).
	let stats = cluster.stats.tier(token.tier.clone()).session(&token.root);

	// Wire only the direction(s) the client will actually use. The token scope
	// (enforced above) caps what it *may* do; the role caps what it *will* do.
	// Pruning the unused half means moq-net feeds that side a no-op origin, so a
	// publish-only ingest isn't announced every cluster broadcast it would ignore,
	// and a subscribe-only egress issues no announce-interest. A bidirectional
	// client (and any transport that carries no role) keeps whatever the token grants.
	let (publish, subscribe) = match role {
		Some(moq_net::Role::Publisher) => (publish, None),
		Some(moq_net::Role::Subscriber) => (None, subscribe),
		// Bidirectional or an unrecognized future role: keep whatever the token grants.
		None | Some(_) => (publish, subscribe),
	};

	Ok(Grants {
		publish,
		subscribe,
		stats,
	})
}

/// Hold an accepted session open for as long as its lease allows.
///
/// Public so an embedder running its own accept loop (`moq --listen`) holds a
/// session the same way the relay does. Pass the [`session::Registration`](crate::session::Registration)
/// from [`Registry::register`](crate::session::Registry::register) so a push on
/// the internal listener re-checks this lease; `None` for a session that is
/// not in the table.
///
/// The lease is the decider's live word on the grant: when it stops covering
/// the session ([`auth::Lease::ended`]) the session closes with the reason, and
/// the session's own close is reported back through the lease as the `end` event.
/// Either way, a relay shutdown drains the session with a GOAWAY instead of
/// cutting it off.
///
/// The session handle is `Send + Sync` whatever transport carries it, so this
/// runs on the shared runtime even for sessions a pinned QUIC worker drives.
pub async fn supervise(
	session: moq_net::Session,
	mut lease: auth::Lease,
	mut shutdown: crate::shutdown::Observer,
	registration: Option<crate::session::Registration>,
) -> anyhow::Result<()> {
	loop {
		let nudged = async {
			match &registration {
				Some(registration) => registration.nudged().await,
				None => std::future::pending().await,
			}
		};
		tokio::select! {
			err = session.closed() => {
				let reason = match &err {
					moq_net::Error::Cancel => "closed".to_string(),
					other => other.to_string(),
				};
				lease.close(reason, session_bytes(&session));
				return Err(err.into());
			}
			why = lease.ended() => {
				tracing::info!(%why, "lease ended, closing session");
				session.abort(moq_net::Error::Unauthorized);
				lease.close(why, session_bytes(&session));
				return Ok(());
			}
			_ = shutdown.started() => {
				tracing::info!("relay shutting down; draining session");
				// Empty URI: "reconnect to me" (the relay is restarting). The session's
				// machine runs on its own, so the GOAWAY still reaches the wire while
				// we wait here.
				shutdown.drain_session(&session).await;
				lease.close("shutdown", session_bytes(&session));
				return Ok(());
			}
			() = nudged => lease.revalidate(),
		}
	}
}

/// The transport's own totals, read once at the end so the `end` event carries
/// what the session moved without the payload path paying for a second meter.
pub(crate) fn session_bytes(session: &moq_net::Session) -> moq_auth::Bytes {
	let stats = session.stats();
	moq_auth::Bytes {
		sent: stats.bytes_sent.unwrap_or_default(),
		received: stats.bytes_received.unwrap_or_default(),
	}
}
