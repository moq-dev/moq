//! How a session is admitted: through an auth server behind `--auth-url`, or with
//! the static grant `--auth-public` names for anonymous sessions. Nothing else admits
//! anyone; a verified client certificate is reported to the server as a fact.

use std::sync::Arc;

use axum::http;
use moq_auth::{Counters, Grant, Request, lease};
use moq_auth::{Pattern, Patterns};
use moq_net::{Path, PathOwned, PathPrefixes, stats::Tier};
use serde::{Deserialize, Serialize};
use serde_with::{OneOrMany, serde_as};
use url::Url;

/// Where every session's grant comes from. Exactly one of `url` and the public
/// patterns is set; [`validate`](Self::validate) refuses anything else.
#[serde_as]
#[derive(usage::Args, Clone, Debug, Serialize, Deserialize, Default)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[serde(default)]
#[non_exhaustive]
pub struct AuthConfig {
	/// The auth server asked once per session event: `connect`, `revalidate`, and
	/// `end`, each one JSON POST carrying everything the relay knows. `https://`
	/// presents the `--connect-tls-*` identity, `unix://` speaks HTTP over a socket,
	/// and `http://` is accepted for a loopback host only.
	#[usage(long = "auth-url", env = "MOQ_AUTH_URL", setting = "auth.url")]
	#[serde(skip_serializing_if = "Option::is_none")]
	pub url: Option<Url>,

	/// Patterns an anonymous session may both publish and subscribe to, such as
	/// `anon/**`. Repeatable or comma-separated. Sets a static grant with no expiry
	/// and no server.
	#[usage(
		long = "auth-public",
		env = "MOQ_AUTH_PUBLIC",
		setting = "auth.public",
		delimiter = ','
	)]
	#[serde(skip_serializing_if = "Vec::is_empty")]
	#[serde_as(as = "OneOrMany<_>")]
	pub public: Vec<Pattern>,

	/// Patterns an anonymous session may subscribe to. Repeatable.
	#[usage(
		long = "auth-public-subscribe",
		env = "MOQ_AUTH_PUBLIC_SUBSCRIBE",
		setting = "auth.public_subscribe",
		delimiter = ','
	)]
	#[serde(skip_serializing_if = "Vec::is_empty")]
	#[serde_as(as = "OneOrMany<_>")]
	pub public_subscribe: Vec<Pattern>,

	/// Patterns an anonymous session may publish. Repeatable.
	#[usage(
		long = "auth-public-publish",
		env = "MOQ_AUTH_PUBLIC_PUBLISH",
		setting = "auth.public_publish",
		delimiter = ','
	)]
	#[serde(skip_serializing_if = "Vec::is_empty")]
	#[serde_as(as = "OneOrMany<_>")]
	pub public_publish: Vec<Pattern>,
}

impl AuthConfig {
	/// The static grant the public patterns name, or `None` when none is set.
	fn public_grant(&self) -> Option<Grant> {
		let publish: Patterns = self.public.iter().chain(&self.public_publish).cloned().collect();
		let subscribe: Patterns = self.public.iter().chain(&self.public_subscribe).cloned().collect();
		(!publish.is_empty() || !subscribe.is_empty()).then(|| Grant::new(publish, subscribe))
	}

	/// Refuse a configuration that admits nobody, or that names both a server and
	/// a static grant, so the question of who decides has one answer.
	pub fn validate(&self) -> anyhow::Result<()> {
		match (&self.url, self.public_grant()) {
			(Some(_), Some(_)) => anyhow::bail!("--auth-url and --auth-public cannot both be set; the server decides"),
			(None, None) => anyhow::bail!(
				"no --auth-url or --auth-public configured; nobody can authenticate (a client certificate admits nothing on its own)"
			),
			_ => Ok(()),
		}
	}

	/// Build the [`Auth`] this configuration describes. `tls` is the client
	/// identity an `https://` server is dialed with; `node` names this relay in
	/// every request.
	pub fn init(&self, node: impl Into<String>, tls: &moq_tokio::tls::Connect) -> anyhow::Result<Auth> {
		self.validate()?;
		let mode = match (&self.url, self.public_grant()) {
			(Some(url), _) => {
				let tls = tls.build()?;
				Mode::Server(moq_auth::Client::new(url.clone(), Some(tls))?)
			}
			(None, Some(grant)) => Mode::Public(grant),
			(None, None) => unreachable!("validated above"),
		};
		Ok(Auth {
			mode: Arc::new(mode),
			node: Arc::from(node.into()),
		})
	}
}

/// Why a session was refused, and the HTTP status a transport-level reject carries.
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum AuthError {
	/// The auth server answered and the answer was no.
	#[error("the auth server refused the session")]
	Refused,

	/// The auth server could not be asked, or answered nonsense; nothing is
	/// admitted because the server was down.
	#[error("auth server unavailable: {0}")]
	Unavailable(String),

	/// The grant names a pattern the relay cannot enforce yet: only `literal/**`
	/// and bare `**` scope an origin until pattern scopes land.
	#[error("unsupported pattern in grant: {0} (only foo/** and ** scope a session)")]
	UnsupportedPattern(String),

	/// The relay could not build the request the server needs.
	#[error("{0}")]
	Request(String),
}

impl From<moq_auth::Error> for AuthError {
	fn from(err: moq_auth::Error) -> Self {
		match err {
			moq_auth::Error::Refused => Self::Refused,
			other => Self::Unavailable(other.to_string()),
		}
	}
}

impl From<&AuthError> for http::StatusCode {
	fn from(err: &AuthError) -> Self {
		match err {
			// A server-side problem, not a credential problem: the client may retry.
			AuthError::Unavailable(_) => http::StatusCode::BAD_GATEWAY,
			AuthError::Request(_) => http::StatusCode::BAD_REQUEST,
			_ => http::StatusCode::UNAUTHORIZED,
		}
	}
}

impl From<AuthError> for http::StatusCode {
	fn from(err: AuthError) -> Self {
		Self::from(&err)
	}
}

impl axum::response::IntoResponse for AuthError {
	fn into_response(self) -> axum::response::Response {
		http::StatusCode::from(self).into_response()
	}
}

/// The grant a session was admitted under, reduced to what the origin scopes by.
///
/// Built from a [`Grant`] and the path the session dialed; rebuilt from the same
/// dialed path whenever the lease changes, so a re-check is compared field by field.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AuthToken {
	/// The path the session dialed, which a grant without `root` is relative to.
	pub(crate) path: String,
	/// The root the session is scoped to: the grant's `root` alias, else the dialed path.
	pub root: PathOwned,
	/// Prefixes the holder may subscribe to, relative to `root`.
	pub subscribe: PathPrefixes,
	/// Prefixes the holder may publish to, relative to `root`.
	pub publish: PathPrefixes,
	/// The tier this session's stats record under.
	pub tier: Tier,
}

impl AuthToken {
	/// Reduce `grant` for a session that dialed `path`.
	///
	/// The origin scopes by prefix until pattern scopes land, so only `foo/**` and
	/// `**` have an exact prefix. Anything else is refused naming the pattern,
	/// including a literal `foo`: reading it as the prefix `foo` would widen one
	/// broadcast into a subtree.
	pub fn new(path: &str, grant: &Grant) -> Result<Self, AuthError> {
		let prefixes = |patterns: &Patterns| -> Result<PathPrefixes, AuthError> {
			patterns
				.iter()
				.map(|pattern| {
					pattern
						.as_prefix()
						.map(|prefix| Path::new(prefix).to_owned())
						.ok_or_else(|| AuthError::UnsupportedPattern(pattern.to_string()))
				})
				.collect()
		};
		let root = grant.root.as_deref().unwrap_or(path);
		Ok(Self {
			path: path.to_string(),
			root: Path::new(root).to_owned(),
			subscribe: prefixes(&grant.subscribe)?,
			publish: prefixes(&grant.publish)?,
			tier: crate::configured_tier(grant.tier.clone()),
		})
	}

	/// Rebuild the token from a re-checked grant, relative to the same dialed path,
	/// so a grant that drops its `root` alias resolves back to what was dialed.
	pub(crate) fn recheck(&self, grant: &Grant) -> Result<Self, AuthError> {
		Self::new(&self.path, grant)
	}

	/// Whether `other` still covers everything this token scopes: the same root and
	/// every prefix still granted. A narrower re-check closes the session until
	/// pattern scopes can resize it in place.
	pub(crate) fn covered_by(&self, other: &Self) -> bool {
		let covers = |granted: &PathPrefixes, held: &PathPrefixes| {
			held.iter().all(|prefix| granted.iter().any(|g| prefix.has_prefix(g)))
		};
		self.root == other.root && covers(&other.subscribe, &self.subscribe) && covers(&other.publish, &self.publish)
	}
}

/// The lease a session holds, with whatever keeps it alive.
///
/// A server-backed lease is driven by the [`moq_auth::Client`]; a static one holds
/// its own [`lease::Producer`] so the grant never changes and never revokes.
pub struct Lease {
	consumer: lease::Consumer,
	_static: Option<lease::Producer>,
}

impl Lease {
	/// A lease on a static grant: no expiry, no re-check, no `end`.
	pub fn fixed(grant: Grant) -> Self {
		let (producer, consumer) = lease::Producer::new(grant);
		Self {
			consumer,
			_static: Some(producer),
		}
	}

	/// The grant as it stands now.
	pub fn grant(&self) -> Grant {
		self.consumer.grant()
	}

	/// The next update: the new grant, or the reason the lease ended.
	pub async fn changed(&mut self) -> Result<Grant, lease::Reason> {
		self.consumer.changed().await
	}

	/// End the lease with the session's close classification.
	pub fn close(self, reason: impl Into<lease::Reason>) {
		self.consumer.close(reason);
	}
}

enum Mode {
	Server(moq_auth::Client),
	Public(Grant),
	/// Nothing admits an ordinary session; only a locally decided grant (the LAN
	/// mesh credential) gets through. What a `--cluster-lan` process with no
	/// listener of its own runs.
	Refuse,
}

/// Admits sessions: asks the server, or hands out the static public grant.
#[derive(Clone)]
pub struct Auth {
	mode: Arc<Mode>,
	node: Arc<str>,
}

impl Auth {
	/// An `Auth` that refuses every session a server or a public grant would have
	/// decided, admitting only what the relay decides for itself (a LAN peer).
	pub fn refuse(node: impl Into<String>) -> Self {
		Self {
			mode: Arc::new(Mode::Refuse),
			node: Arc::from(node.into()),
		}
	}

	/// The name this relay puts in every request.
	pub fn node(&self) -> &str {
		&self.node
	}

	/// A fresh `connect` request for this relay, before the transport's facts are filled in.
	pub fn request(&self, transport: moq_auth::Transport, path: impl Into<String>) -> Request {
		Request::connect(self.node.as_ref(), transport, path)
	}

	/// Admit a session: the lease it holds and the scope the origin applies.
	///
	/// `bytes` is what the session meters, reported in the `end` event.
	pub async fn admit(&self, request: Request, bytes: Counters) -> Result<Admitted, AuthError> {
		let path = request.path.clone();
		let lease = match self.mode.as_ref() {
			Mode::Server(client) => Lease {
				consumer: client.connect(request, bytes).await?,
				_static: None,
			},
			// A certificate is a fact for a server to weigh; with no server it admits
			// nothing on its own, so the peer gets what any anonymous session gets.
			Mode::Public(grant) => Lease::fixed(grant.clone()),
			Mode::Refuse => return Err(AuthError::Refused),
		};
		let token = AuthToken::new(&path, &lease.grant())?;
		Ok(Admitted { lease, token })
	}

	/// Admit a session on a grant decided locally, bypassing the server: the LAN
	/// mesh credential, which the relay minted for itself.
	pub(crate) fn admit_fixed(&self, path: &str, grant: Grant) -> Result<Admitted, AuthError> {
		let lease = Lease::fixed(grant);
		let token = AuthToken::new(path, &lease.grant())?;
		Ok(Admitted { lease, token })
	}
}

/// An admitted session: its lease and the scope it was admitted under.
pub struct Admitted {
	/// The lease the session holds for as long as it runs.
	pub lease: Lease,
	/// The grant reduced to what the origin scopes by.
	pub token: AuthToken,
}

/// The `moq_auth::Request` for an accepted transport request: every fact the
/// transport knows, nothing parsed on the server's behalf.
pub fn request_for(auth: &Auth, request: &moq_tokio::Request) -> Request {
	let transport = match request.transport() {
		moq_tokio::Transport::Quic => moq_auth::Transport::Quic,
		moq_tokio::Transport::Iroh => moq_auth::Transport::Iroh,
		moq_tokio::Transport::WebSocket => moq_auth::Transport::WebSocket,
		moq_tokio::Transport::Tcp => moq_auth::Transport::Tcp,
		moq_tokio::Transport::Unix => moq_auth::Transport::Unix,
		// A transport this build does not know is still a session on the wire; the
		// server sees the same facts either way.
		other => unreachable!("unknown transport {other}"),
	};
	// A URL-less transport reports its root as empty; the contract says what was dialed.
	let path = match request.path() {
		"" => "/".to_string(),
		path => path.to_string(),
	};
	let mut out = auth.request(transport, path);
	out.query = request.query().map(str::to_owned);
	out.remote = request.remote_addr();
	out.local = request.local_addr();
	out.server_name = request
		.server_name()
		.map(str::to_owned)
		.or_else(|| request.authority().map(str::to_owned));
	out.alpn = request.alpn().map(str::to_owned);
	out.role = request.role().map(|role| match role {
		moq_net::Role::Publisher => moq_auth::Role::Publisher,
		_ => moq_auth::Role::Subscriber,
	});
	out.tls = request.peer_identity().as_ref().and_then(peer);
	out
}

/// The certificate facts for a verified peer, or `None` when the chain does not parse.
pub(crate) fn peer(identity: &moq_tokio::tls::PeerIdentity) -> Option<moq_auth::Peer> {
	let fingerprint = identity.fingerprint()?;
	Some(moq_auth::Peer {
		name: identity.name().unwrap_or_else(|| fingerprint.clone()),
		fingerprint,
		expires: identity.expiry(),
		issuer: identity.issuer().unwrap_or_default(),
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	fn patterns(texts: &[&str]) -> Patterns {
		texts.iter().map(|text| text.parse().unwrap()).collect()
	}

	fn config(url: Option<&str>, public: &[&str]) -> AuthConfig {
		AuthConfig {
			url: url.map(|url| url.parse().unwrap()),
			public: public.iter().map(|p| p.parse().unwrap()).collect(),
			..Default::default()
		}
	}

	#[test]
	fn exactly_one_source() {
		assert!(config(None, &[]).validate().is_err());
		assert!(config(Some("http://127.0.0.1:4440/"), &["**"]).validate().is_err());
		assert!(config(Some("http://127.0.0.1:4440/"), &[]).validate().is_ok());
		assert!(config(None, &["anon/**"]).validate().is_ok());

		let split = AuthConfig {
			public_subscribe: patterns(&["anon/**"]).into_iter().collect(),
			..Default::default()
		};
		assert!(split.validate().is_ok());
		let grant = split.public_grant().unwrap();
		assert_eq!(grant.subscribe, patterns(&["anon/**"]));
		assert!(grant.publish.is_empty());
	}

	#[test]
	fn a_public_config_admits_anonymous_and_certificate_alike() {
		let auth = config(None, &["anon/**"])
			.init("relay-1", &moq_tokio::tls::Connect::default())
			.unwrap();
		let request = auth.request(moq_auth::Transport::Quic, "/anon/room");
		let admitted = futures::executor::block_on(auth.admit(request, Counters::default())).unwrap();
		assert_eq!(admitted.token.root, Path::new("anon/room").to_owned());
		assert_eq!(
			admitted.token.subscribe,
			PathPrefixes::from(vec![Path::new("anon").to_owned()])
		);
		assert_eq!(admitted.token.tier, Tier::default());
	}

	#[test]
	fn token_reduces_a_grant_and_refuses_what_it_cannot_scope() {
		let mut grant = Grant::new(patterns(&["alice/**"]), patterns(&["**"]));
		grant.root = Some("pid/room".into());
		grant.tier = Some("gold".into());
		let token = AuthToken::new("/vanity/room", &grant).unwrap();
		assert_eq!(token.root, Path::new("pid/room").to_owned());
		assert_eq!(token.publish, PathPrefixes::from(vec![Path::new("alice").to_owned()]));
		assert_eq!(token.subscribe, PathPrefixes::from(vec![Path::new("").to_owned()]));
		assert_eq!(token.tier, Tier::new("gold"));

		for pattern in ["*/chat", "alice", ""] {
			let grant = Grant::new(patterns(&[pattern]), Patterns::new());
			let err = AuthToken::new("/", &grant).unwrap_err();
			assert!(
				matches!(&err, AuthError::UnsupportedPattern(p) if p == pattern),
				"{pattern}: {err}"
			);
		}
	}

	#[test]
	fn a_narrower_recheck_is_not_covered() {
		let wide = AuthToken::new("/room", &Grant::new(patterns(&["**"]), patterns(&["**"]))).unwrap();
		let narrow = AuthToken::new("/room", &Grant::new(patterns(&["alice/**"]), patterns(&["**"]))).unwrap();
		let moved = AuthToken::new("/other", &Grant::new(patterns(&["**"]), patterns(&["**"]))).unwrap();
		assert!(wide.covered_by(&wide));
		assert!(narrow.covered_by(&wide));
		assert!(!wide.covered_by(&narrow));
		assert!(!wide.covered_by(&moved));
	}

	#[test]
	fn a_recheck_resolves_a_dropped_root_to_the_dialed_path() {
		let everything = || Grant::new(patterns(&["**"]), patterns(&["**"]));
		let mut aliased = everything();
		aliased.root = Some("pid/room".into());
		let token = AuthToken::new("/vanity/room", &aliased).unwrap();
		assert_eq!(token.root, Path::new("pid/room").to_owned());

		// The same alias still names the same root.
		assert_eq!(token.recheck(&aliased).unwrap().root, token.root);
		// A grant without the alias is relative to what was dialed, not to the old root.
		assert_eq!(
			token.recheck(&everything()).unwrap().root,
			Path::new("vanity/room").to_owned()
		);
	}
}
