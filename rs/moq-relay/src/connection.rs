use crate::{Auth, AuthError, AuthParams, AuthToken, Cluster};

use axum::http;
use moq_native::Request;

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
/// Call [`run`](Self::run) to authenticate the request, wire up
/// publish/subscribe origins, and serve the session until it closes.
pub struct Connection {
	/// A numeric identifier for logging.
	pub id: u64,
	/// The raw QUIC/WebTransport request to accept or reject.
	pub request: Request,
	/// The cluster state used to resolve origins.
	pub cluster: Cluster,
	/// The authenticator used to verify credentials.
	pub auth: Auth,
}

impl Connection {
	/// Authenticates and serves this connection until it closes.
	#[tracing::instrument("conn", skip_all, fields(id = self.id))]
	pub async fn run(self) -> anyhow::Result<()> {
		let token = match self.authenticate().await {
			Ok(token) => token,
			Err(err) => {
				let _ = self.request.close(err.status.as_u16()).await;
				return Err(err.source);
			}
		};

		let publish = self.cluster.publisher(&token);
		let subscribe = self.cluster.subscriber(&token);
		let transport = self.request.transport();

		// The client advertises which direction it intends to use (moq-lite-05 SETUP).
		// A bidirectional connection (e.g. a cluster peer) leaves this `Both`, so the
		// only requirement is that the token grants *something*. But a gateway that only
		// publishes or only subscribes says so, and a token missing that direction's
		// scope is rejected here during the handshake, instead of being accepted and
		// then silently carrying no media (the bug that motivated the role hint).
		let role = self.request.role();
		let authorized = match role {
			moq_net::Role::Publisher => publish.is_some(),
			moq_net::Role::Subscriber => subscribe.is_some(),
			moq_net::Role::Both => publish.is_some() || subscribe.is_some(),
		};
		if !authorized {
			let _ = self.request.close(http::StatusCode::FORBIDDEN.as_u16()).await;
			anyhow::bail!("token does not grant {role:?} access to {}", token.root);
		}

		match (&publish, &subscribe) {
			(Some(publish), Some(subscribe)) => {
				tracing::info!(%transport, ?role, tier = %token.tier, root = %token.root, publish = %publish.allowed().map(|p| p.as_str()).collect::<Vec<_>>().join(","), subscribe = %subscribe.allowed().map(|p| p.as_str()).collect::<Vec<_>>().join(","), "session accepted");
			}
			(Some(publish), None) => {
				tracing::info!(%transport, ?role, tier = %token.tier, root = %token.root, publish = %publish.allowed().map(|p| p.as_str()).collect::<Vec<_>>().join(","), "publisher accepted");
			}
			(None, Some(subscribe)) => {
				tracing::info!(%transport, ?role, tier = %token.tier, root = %token.root, subscribe = %subscribe.allowed().map(|p| p.as_str()).collect::<Vec<_>>().join(","), "subscriber accepted")
			}
			_ => unreachable!("authorized above guarantees at least one origin"),
		}

		// Record this session's stats under its billing tier (chosen by the auth
		// API; mTLS peers and cluster nodes default to `internal`). The aggregator
		// is shared; the tier picks which counter set the bumps land in.
		let stats = self.cluster.stats.tier(token.tier.clone());

		// Count this session against its auth root for the whole connection,
		// independent of any data flow, so presence-based billing sees a client
		// that connects to e.g. `/acme` even while idle. Dropped when
		// the connection closes below.
		let _session_stats = stats.session(&token.root);

		// Wire only the direction(s) the client will actually use. The token scope
		// (enforced above) caps what it *may* do; the role caps what it *will* do.
		// Pruning the unused half means moq-net feeds that side a no-op origin, so a
		// publish-only ingest isn't announced every cluster broadcast it would ignore,
		// and a subscribe-only egress issues no announce-interest. A `Both` client (and
		// any transport that carries no role) keeps whatever the token grants.
		let (publish, subscribe) = match role {
			moq_net::Role::Publisher => (publish, None),
			moq_net::Role::Subscriber => (None, subscribe),
			moq_net::Role::Both => (publish, subscribe),
		};

		// Accept the connection.
		// NOTE: subscribe and publish seem backwards because of how relays work.
		// We publish the tracks the client is allowed to subscribe to.
		// We subscribe to the tracks the client is allowed to publish.
		//
		// moq-net defaults the unset side to a fresh no-op origin, which is fine for a
		// publish-only or subscribe-only session.
		let mut request = self.request.with_stats(stats);
		if let Some(subscribe) = subscribe {
			request = request.with_publisher(&subscribe);
		}
		if let Some(publish) = publish {
			request = request.with_subscriber(publish);
		}
		let session = request.ok().await?;

		tracing::info!(version = %session.version(), %transport, "negotiated");

		// The credential (JWT `exp` or client cert `notAfter`) is only checked at
		// connect time, so hold the session open no longer than the credential is
		// valid. Without an expiry, just wait for the session to close.
		let Some(expires) = token.expires else {
			return Ok(session.closed().await?);
		};

		let remaining = expires.duration_since(std::time::SystemTime::now()).unwrap_or_default();
		match tokio::time::timeout(remaining, session.closed()).await {
			Ok(res) => Ok(res?),
			Err(_) => {
				tracing::info!("credential expired, closing session");
				session.close(moq_net::Error::Unauthorized);
				Ok(())
			}
		}
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
					// also returns the billing tier (defaulting to internal for trusted peers).
					let mut token = self.auth.verify_mtls(&params.path, Some(transport)).await?;
					// Close the session when the client certificate expires, mirroring
					// the JWT `exp` handling. Validated once at the TLS handshake otherwise.
					token.expires = identity.expiry();
					return Ok(token);
				}
				params
			}
			// URL-less stream transports: path + `?jwt=` ride the SETUP.
			None => AuthParams::from_path(self.request.path().unwrap_or("")),
		};
		params.transport = Some(transport);

		Ok(self.auth.verify(&params).await?)
	}
}
