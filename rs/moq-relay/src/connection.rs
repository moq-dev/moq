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

impl StatusError {
	fn forbidden(source: anyhow::Error) -> Self {
		Self {
			status: http::StatusCode::FORBIDDEN,
			source,
		}
	}

	fn unauthorized(source: anyhow::Error) -> Self {
		Self {
			status: http::StatusCode::UNAUTHORIZED,
			source,
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
		let mut session = self
			.request
			.with_publish(subscribe)
			.with_consume(publish)
			.with_stats(stats)
			.ok()
			.await?;

		tracing::info!(version = %session.version(), transport, "negotiated");

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
	/// The path is sourced differently per transport:
	/// - URL-bearing transports (QUIC, WebSocket) take it from the request URL,
	///   and a valid mTLS client certificate (QUIC only) stands in for a JWT,
	///   granting full access within the URL path's root.
	/// - Stream transports (`tcp`/`unix`) take the path + `?jwt=` from the
	///   moq-lite-05 SETUP, with per-listener policy from the bind URL query.
	async fn authenticate(&self) -> Result<AuthToken, StatusError> {
		if let Some(url) = self.request.url() {
			let params = self.auth.params_from_url(url);

			if let Some(identity) = self.request.peer_identity() {
				tracing::debug!("mTLS peer authenticated");
				// Scope the grant to the canonical root. An mTLS publisher dialing a
				// vanity alias lands on the same tree a JWT would; cluster peers dial
				// "/", which the API resolves (typically to an unscoped root). The API
				// also returns the billing tier (defaulting to internal for trusted peers).
				let mut token = self.auth.verify_mtls(&params.path).await?;
				// Close the session when the client certificate expires, mirroring
				// the JWT `exp` handling. Validated once at the TLS handshake otherwise.
				token.expires = identity.expiry();
				return Ok(token);
			}

			return Ok(self.auth.verify(&params).await?);
		}

		self.authenticate_stream().await
	}

	/// Authenticate a URL-less stream (`tcp://`/`unix://`) connection.
	///
	/// The request path and JWT ride the SETUP; the listener's anon scope and
	/// peer-credential allowlist ride the `--server-bind` URL query.
	async fn authenticate_stream(&self) -> Result<AuthToken, StatusError> {
		let policy = ListenPolicy::parse(self.request.listen_query());

		// Peer-credential allowlist (unix only): defense-in-depth on top of the
		// JWT. A populated list fails closed when the transport reports no
		// credentials (e.g. plain TCP).
		if !policy.allow.is_empty() {
			match self.request.peer_cred() {
				Some(cred) if policy.allow.permits(&cred) => {}
				Some(cred) => {
					return Err(StatusError::forbidden(anyhow::anyhow!(
						"peer uid={} gid={} pid={:?} not in allow list",
						cred.uid,
						cred.gid,
						cred.pid
					)));
				}
				None => {
					return Err(StatusError::forbidden(anyhow::anyhow!(
						"peer-credential allowlist set but the transport reports no credentials"
					)));
				}
			}
		}

		let params = AuthParams::from_path(self.request.path().unwrap_or(""));

		// No JWT and no path: a trusted local helper (e.g. a stats publisher).
		// Grant the fixed anon subtree if this listener configured one, else
		// reject. The scope is relay-fixed, so the caller cannot widen it.
		if params.jwt.is_none() && params.path.trim_matches('/').is_empty() {
			return match policy.anon {
				Some(prefix) => Ok(AuthToken::anon(&prefix)),
				None => Err(StatusError::unauthorized(anyhow::anyhow!("connection requires a JWT"))),
			};
		}

		// A JWT verifies + scopes the session; a path with no JWT resolves
		// tokenless public access through the authenticator.
		Ok(self.auth.verify(&params).await?)
	}
}

/// Per-listener policy parsed from a stream `--server-bind` URL query string.
#[derive(Default)]
struct ListenPolicy {
	/// The subtree granted to no-JWT connections (`anon=<prefix>`). An empty
	/// value grants the whole root; absent rejects no-JWT connections.
	anon: Option<String>,
	/// Peer-credential allowlist (`allow-uid` / `allow-gid` / `allow-pid`).
	allow: PeerAllow,
}

impl ListenPolicy {
	fn parse(query: Option<&str>) -> Self {
		let mut policy = ListenPolicy::default();
		let Some(query) = query else {
			return policy;
		};

		for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
			match key.as_ref() {
				// Keep empty values: `anon=` is an explicit opt-in to the root.
				"anon" => policy.anon = Some(value.into_owned()),
				"allow-uid" => policy
					.allow
					.uid
					.extend(value.split(',').filter_map(|s| s.parse::<u32>().ok())),
				"allow-gid" => policy
					.allow
					.gid
					.extend(value.split(',').filter_map(|s| s.parse::<u32>().ok())),
				"allow-pid" => policy
					.allow
					.pid
					.extend(value.split(',').filter_map(|s| s.parse::<i32>().ok())),
				_ => {}
			}
		}

		policy
	}
}

/// A peer-credential allowlist. Each populated field constrains the matching
/// credential (AND across fields, OR within a field); all empty means no check.
#[derive(Default)]
struct PeerAllow {
	uid: Vec<u32>,
	gid: Vec<u32>,
	pid: Vec<i32>,
}

impl PeerAllow {
	fn is_empty(&self) -> bool {
		self.uid.is_empty() && self.gid.is_empty() && self.pid.is_empty()
	}

	fn permits(&self, cred: &moq_native::PeerCred) -> bool {
		let uid_ok = self.uid.is_empty() || self.uid.contains(&cred.uid);
		let gid_ok = self.gid.is_empty() || self.gid.contains(&cred.gid);
		// A required pid can't be satisfied if the platform doesn't report one.
		let pid_ok = self.pid.is_empty() || cred.pid.is_some_and(|pid| self.pid.contains(&pid));
		uid_ok && gid_ok && pid_ok
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use moq_native::PeerCred;

	#[test]
	fn listen_policy_parses_anon_and_allow() {
		let policy = ListenPolicy::parse(Some("anon=.stats&allow-uid=1001,1002&allow-gid=2000&allow-pid=42"));
		assert_eq!(policy.anon.as_deref(), Some(".stats"));
		assert_eq!(policy.allow.uid, vec![1001, 1002]);
		assert_eq!(policy.allow.gid, vec![2000]);
		assert_eq!(policy.allow.pid, vec![42]);
		assert!(!policy.allow.is_empty());
	}

	#[test]
	fn listen_policy_empty_anon_is_root_not_absent() {
		// `anon=` (empty value) is an explicit opt-in to the whole root, distinct
		// from an absent `anon` (which rejects no-JWT connections).
		assert_eq!(ListenPolicy::parse(Some("anon=")).anon.as_deref(), Some(""));
		assert_eq!(ListenPolicy::parse(Some("allow-uid=1")).anon, None);
		assert_eq!(ListenPolicy::parse(None).anon, None);
	}

	#[test]
	fn peer_allow_matches_and() {
		let allow = PeerAllow {
			uid: vec![1001],
			gid: vec![],
			pid: vec![],
		};
		assert!(allow.permits(&PeerCred {
			uid: 1001,
			gid: 5,
			pid: Some(9)
		}));
		assert!(!allow.permits(&PeerCred {
			uid: 1002,
			gid: 5,
			pid: Some(9)
		}));

		// A required pid is unsatisfiable when the platform reports none.
		let pid_required = PeerAllow {
			uid: vec![],
			gid: vec![],
			pid: vec![42],
		};
		assert!(!pid_required.permits(&PeerCred {
			uid: 1,
			gid: 1,
			pid: None
		}));
	}
}
