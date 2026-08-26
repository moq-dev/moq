use crate::{Auth, AuthError, AuthParams, AuthToken, Cluster};

use axum::http;
use moq_tokio::Request;

/// An error carrying the HTTP status to send when closing the request.
///
/// Used only on the pre-accept auth path so the caller can close once with
/// the right code instead of sprinkling close/return at each failure site.
struct StatusError {
	status: http::StatusCode,
	source: anyhow::Error,
}

impl From<AuthError> for StatusError {
	fn from(err: AuthError) -> Self {
		Self {
			status: (&err).into(),
			source: err.into(),
		}
	}
}

/// An incoming connection that has not yet been authenticated.
///
/// Build with [`new`](Self::new), attach the optional knobs, then call
/// [`run`](Self::run) to authenticate the request, wire up publish/subscribe
/// origins, and serve the session until it closes.
pub struct Connection {
	/// A numeric identifier for logging.
	id: u64,
	/// The raw QUIC/WebTransport request to accept or reject.
	request: Request,
	/// The cluster state used to resolve origins.
	cluster: Cluster,
	/// The authenticator used to verify credentials.
	auth: Auth,
	/// Relay-wide shutdown broadcast: when it fires, the session is drained with
	/// a GOAWAY instead of being cut off.
	shutdown: crate::Shutdown,
}

impl Connection {
	/// Wrap an accepted request, resolving origins through `cluster` and
	/// credentials through `auth`.
	pub fn new(request: Request, cluster: Cluster, auth: Auth) -> Self {
		Self {
			id: 0,
			request,
			cluster,
			auth,
			shutdown: crate::Shutdown::disabled(),
		}
	}

	/// Set the identifier this connection logs under. Defaults to 0.
	pub fn with_id(mut self, id: u64) -> Self {
		self.id = id;
		self
	}

	/// Attach the relay-wide shutdown broadcast so the session drains with a
	/// GOAWAY when it fires. Without it the session is cut off on process exit.
	pub fn with_shutdown(mut self, shutdown: crate::Shutdown) -> Self {
		self.shutdown = shutdown;
		self
	}

	/// Authenticates and serves this connection until it closes.
	#[tracing::instrument("conn", skip_all, fields(id = self.id))]
	pub async fn run(self) -> anyhow::Result<()> {
		let peer_origin = self.request.peer_origin();
		let token = match self.authenticate().await {
			Ok(token) => token,
			Err(err) => {
				let _ = self.request.close(err.status.as_u16()).await;
				return Err(err.source);
			}
		};

		let transport = self.request.transport();
		let role = self.request.role();
		let grants = match authorize(&self.cluster, &token, role, &transport) {
			Ok(grants) => grants,
			Err(err) => {
				let _ = self.request.close(http::StatusCode::FORBIDDEN.as_u16()).await;
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
		let _node_connection = peer_origin.map(|origin| self.cluster.nodes.connect_inbound(self.id, origin));

		tracing::info!(version = %session.version(), %transport, "negotiated");

		supervise(&self.auth, session, token, self.shutdown.clone()).await
	}

	/// Resolve an [`AuthToken`] for this connection. Any failure is returned as a
	/// [`StatusError`] so [`run`] can close the request with the mapped HTTP
	/// status exactly once.
	///
	/// Every transport goes through the same authenticator; only the source of
	/// the path + JWT differs:
	/// - URL-bearing transports (QUIC, WebSocket) take it from the request URL,
	///   and a valid mTLS client certificate (QUIC only) stands in for a JWT,
	///   granting full access within the URL path's root.
	/// - Stream transports (`tcp`/`unix`) take the path + `?jwt=` from the
	///   moq-lite-05 SETUP. A no-JWT connection resolves anonymous/public access
	///   for its path exactly like a tokenless QUIC client (`--auth-public`).
	///   Unix peer-credential gating happens earlier, in the listener.
	async fn authenticate(&self) -> Result<AuthToken, StatusError> {
		// Forwarded to the auth API so it can bucket by connection type (e.g. tier
		// the internal Unix-socket gateways separately). "quic"/"websocket"/"tcp"/
		// "unix"/"iroh".
		let transport = self.request.transport();
		let mut params = match self.request.url() {
			// URL-bearing transports: mTLS (QUIC only) can stand in for a JWT.
			Some(url) => {
				let params = self.auth.params_from_url(url);
				if let Some(identity) = self.request.peer_identity() {
					tracing::debug!("mTLS peer authenticated");
					// Scope the grant to the canonical root. An mTLS publisher dialing a
					// vanity alias lands on the same tree a JWT would; cluster peers dial
					// "/", which the API resolves (typically to an unscoped root). The API
					// also returns the billing tier.
					let mut token = self.auth.verify_mtls(&params.path, Some(transport)).await?;
					// Close the session when the client certificate expires, mirroring
					// the JWT `exp` handling. Validated once at the TLS handshake otherwise.
					token.expires = identity.expiry();
					return Ok(token);
				}
				params
			}
			// URL-less stream transports: path + `?jwt=` ride the SETUP.
			None => AuthParams::from_path_query(self.request.path(), self.request.query()),
		};
		params.transport = Some(transport);

		Ok(self.auth.verify(&params).await?)
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

/// Authorize an authenticated session and resolve what it may serve, however
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
	cluster: &Cluster,
	token: &AuthToken,
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
		anyhow::bail!("token does not grant {wanted} access to {}", token.root);
	}

	match (&publish, &subscribe) {
		(Some(publish), Some(subscribe)) => {
			tracing::info!(%transport, ?role, tier = %token.tier, root = %token.root, publish = %publish.allowed().map(moq_net::Path::as_str).collect::<Vec<_>>().join(","), subscribe = %subscribe.allowed().map(moq_net::Path::as_str).collect::<Vec<_>>().join(","), "session accepted");
		}
		(Some(publish), None) => {
			tracing::info!(%transport, ?role, tier = %token.tier, root = %token.root, publish = %publish.allowed().map(moq_net::Path::as_str).collect::<Vec<_>>().join(","), "publisher accepted");
		}
		(None, Some(subscribe)) => {
			tracing::info!(%transport, ?role, tier = %token.tier, root = %token.root, subscribe = %subscribe.allowed().map(moq_net::Path::as_str).collect::<Vec<_>>().join(","), "subscriber accepted");
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

/// Hold an accepted session open for as long as it is allowed to run.
///
/// The credential (JWT `exp` or client cert `notAfter`) is only checked at
/// connect time, so hold the session open no longer than the credential is
/// valid. Without any bound, wait for the session to close. Either way, a
/// relay shutdown drains the session with a GOAWAY instead of cutting it off.
///
/// The session handle is `Send + Sync` whatever transport carries it, so this
/// runs on the shared runtime even for sessions a pinned QUIC worker drives.
pub(crate) async fn supervise(
	auth: &Auth,
	session: moq_net::Session,
	token: AuthToken,
	mut shutdown: crate::Shutdown,
) -> anyhow::Result<()> {
	tokio::select! {
		err = session.closed() => Err(err.into()),
		reason = auth.expired(&token) => {
			tracing::info!(%reason, "credential no longer valid, closing session");
			session.abort(moq_net::Error::Unauthorized);
			Ok(())
		}
		_ = shutdown.started() => {
			tracing::info!("relay shutting down; draining session");
			// Empty URI: "reconnect to me" (the relay is restarting). The session's
			// machine runs on its own, so the GOAWAY still reaches the wire while
			// we wait here.
			shutdown.drain_session(&session).await;
			Ok(())
		}
	}
}
