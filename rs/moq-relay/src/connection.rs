use crate::{Auth, AuthError, AuthParams, AuthToken, Cluster};

use axum::http;
use moq_native::Request;
use moq_net::Path;

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

		match (&publish, &subscribe) {
			(Some(publish), Some(subscribe)) => {
				tracing::info!(transport, internal = token.internal, root = %token.root, publish = %publish.allowed().map(|p| p.as_str()).collect::<Vec<_>>().join(","), subscribe = %subscribe.allowed().map(|p| p.as_str()).collect::<Vec<_>>().join(","), "session accepted");
			}
			(Some(publish), None) => {
				tracing::info!(transport, internal = token.internal, root = %token.root, publish = %publish.allowed().map(|p| p.as_str()).collect::<Vec<_>>().join(","), "publisher accepted");
			}
			(None, Some(subscribe)) => {
				tracing::info!(transport, internal = token.internal, root = %token.root, subscribe = %subscribe.allowed().map(|p| p.as_str()).collect::<Vec<_>>().join(","), "subscriber accepted")
			}
			_ => {
				let _ = self.request.close(http::StatusCode::FORBIDDEN.as_u16()).await;
				anyhow::bail!("invalid session; no allowed paths");
			}
		}

		// mTLS-authenticated peers (including other cluster nodes) report through
		// the internal tier so a billing service can rate-differentiate from
		// external traffic. The aggregator is shared; the tier picks which counter
		// set within each level the bumps land in.
		let tier = match token.internal {
			true => moq_net::Tier::Internal,
			false => moq_net::Tier::External,
		};
		let stats = self.cluster.stats.tier(tier);

		// Count this session against its auth root for the whole connection,
		// independent of any data flow, so presence-based billing sees a client
		// that connects to e.g. `/acme` even while idle. Dropped when
		// the connection closes below.
		let _session_stats = stats.session(&token.root);

		// Accept the connection.
		// NOTE: subscribe and publish seem backwards because of how relays work.
		// We publish the tracks the client is allowed to subscribe to.
		// We subscribe to the tracks the client is allowed to publish.
		let session = self
			.request
			.with_publish(subscribe)
			.with_consume(publish)
			.with_stats(stats)
			.ok()
			.await?;

		tracing::info!(version = %session.version(), transport, "negotiated");

		// Wait until the session is closed.
		session.closed().await?;
		Ok(())
	}

	/// Resolve an [`AuthToken`] from the request's URL and (optional) mTLS peer
	/// identity. Any failure is returned as a [`StatusError`] so [`run`] can
	/// close the request with the mapped HTTP status exactly once.
	///
	/// If the client presented a valid mTLS client certificate, JWT is skipped
	/// and full access is granted within the URL path's root. The cert's chain
	/// to the configured CA is the only credential we require.
	async fn authenticate(&self) -> Result<AuthToken, StatusError> {
		let params = match self.request.url() {
			Some(url) => self.auth.params_from_url(url),
			None => AuthParams::default(),
		};

		if self.request.has_peer_certificate() {
			tracing::debug!("mTLS peer authenticated");
			// Scope the grant to the canonical root. An mTLS publisher dialing a
			// vanity alias lands on the same tree a JWT would; cluster peers dial
			// "/", which resolves to an empty (unscoped) root. The API also returns
			// the billing tier (defaulting to internal for trusted peers).
			let (root, internal) = self.auth.resolve_mtls(&params.path).await;
			let mut token = AuthToken::unrestricted(Path::new(&root).to_owned());
			token.internal = internal;
			return Ok(token);
		}

		Ok(self.auth.verify(&params).await?)
	}
}
