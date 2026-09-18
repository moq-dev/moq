//! How a session is admitted: through an auth server behind `--auth-url`, with the
//! static grant `--auth-public` names for anonymous sessions, or by the embedding
//! process answering [`Admissions`]. Nothing else admits anyone; a verified client
//! certificate is reported to whoever decides as a fact.

use std::sync::Arc;
use std::time::SystemTime;

use axum::http;
use moq_auth::{Bytes, Grant, Request, lease};
use moq_auth::{Pattern, Patterns};
use moq_net::{Path, PathOwned, stats::Tier};
use serde::{Deserialize, Serialize};
use serde_with::{OneOrMany, serde_as};
use tokio::sync::{mpsc, oneshot};
use url::Url;

/// The longest an embedder may take to answer an admission, the bound
/// `moq_auth::Client` puts on a server, so a stalled decider refuses rather
/// than parks the sessions behind it.
const ADMIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Where every session's grant comes from. Exactly one of `url` and the public
/// patterns is set; [`validate`](Self::validate) refuses anything else.
#[serde_as]
#[derive(usage::Args, Clone, Debug, Serialize, Deserialize, Default)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[serde(default)]
#[non_exhaustive]
pub struct Config {
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

impl Config {
	/// The static grant the public patterns name, or `None` when none is set.
	fn public_grant(&self) -> Option<Grant> {
		let publish: Patterns = self.public.iter().chain(&self.public_publish).cloned().collect();
		let subscribe: Patterns = self.public.iter().chain(&self.public_subscribe).cloned().collect();
		(!publish.is_empty() || !subscribe.is_empty()).then(|| Grant::new(publish, subscribe))
	}

	/// Whether no source is named at all. Such a relay admits nothing on its own:
	/// [`Relay::load`](crate::Relay::load) hands its sessions to the embedder as
	/// [`Admissions`], and [`validate`](Self::validate) refuses it for a binary.
	pub(crate) fn is_empty(&self) -> bool {
		self.url.is_none() && self.public_grant().is_none()
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
pub enum Error {
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

impl From<moq_auth::Error> for Error {
	fn from(err: moq_auth::Error) -> Self {
		match err {
			moq_auth::Error::Refused => Self::Refused,
			other => Self::Unavailable(other.to_string()),
		}
	}
}

impl From<&Error> for http::StatusCode {
	fn from(err: &Error) -> Self {
		match err {
			// A server-side problem, not a credential problem: the client may retry.
			Error::Unavailable(_) => http::StatusCode::BAD_GATEWAY,
			Error::Request(_) => http::StatusCode::BAD_REQUEST,
			_ => http::StatusCode::UNAUTHORIZED,
		}
	}
}

impl From<Error> for http::StatusCode {
	fn from(err: Error) -> Self {
		Self::from(&err)
	}
}

impl axum::response::IntoResponse for Error {
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
pub struct Token {
	/// The path the session dialed, which a grant without `root` is relative to.
	pub(crate) path: String,
	/// The root the session is scoped to: the grant's `root` alias, else the dialed path.
	pub root: PathOwned,
	/// The subtree grants the holder may subscribe to, relative to `root`.
	pub subscribe: Patterns,
	/// The subtree grants the holder may publish to, relative to `root`.
	pub publish: Patterns,
	/// The tier this session's stats record under.
	pub tier: Tier,
}

impl Token {
	/// Reduce `grant` for a session that dialed `path`.
	///
	/// The origin scopes by prefix, so only `foo/**` and `**` have an exact prefix.
	/// Anything else is refused naming the pattern, including a literal `foo`:
	/// reading it as the prefix `foo` would widen one broadcast into a subtree.
	/// The token keeps the Patterns the grant already yields rather than converting
	/// them to prefixes.
	pub fn new(path: &str, grant: &Grant) -> Result<Self, Error> {
		let supported = |patterns: &Patterns| -> Result<Patterns, Error> {
			match patterns.iter().find(|pattern| pattern.as_prefix().is_none()) {
				Some(pattern) => Err(Error::UnsupportedPattern(pattern.to_string())),
				None => Ok(patterns.clone()),
			}
		};
		let root = grant.root.as_deref().unwrap_or(path);
		Ok(Self {
			path: path.to_string(),
			root: Path::new(root).to_owned(),
			subscribe: supported(&grant.subscribe)?,
			publish: supported(&grant.publish)?,
			tier: crate::configured_tier(grant.tier.clone()),
		})
	}

	/// Rebuild the token from a re-checked grant, relative to the same dialed path,
	/// so a grant that drops its `root` alias resolves back to what was dialed.
	pub(crate) fn recheck(&self, grant: &Grant) -> Result<Self, Error> {
		Self::new(&self.path, grant)
	}

	/// Whether `other` still covers everything this token scopes: the same root and
	/// every grant still held. A narrower re-check closes the session until
	/// pattern scopes can resize it in place.
	pub(crate) fn covered_by(&self, other: &Self) -> bool {
		self.root == other.root && other.subscribe.covers(&self.subscribe) && other.publish.covers(&self.publish)
	}
}

/// What admitted a session and keeps it admitted: the grant's lease and the
/// scope it was reduced to.
///
/// The lease is the decider's live word on the grant: the [`moq_auth::Client`]
/// behind an auth server, the embedder answering an [`Admission`], or nobody for
/// a fixed grant. [`ended`](Self::ended) resolves when that word no longer covers
/// the session; the holder then closes the session, and the lease with the reason.
pub struct Lease {
	consumer: lease::Consumer,
	token: Token,
	/// When the grant runs out, enforced here whoever drives the lease: a fixed
	/// grant has no driver, and an auth server's may be mid-outage.
	expires: Option<SystemTime>,
}

impl Lease {
	/// Hold `consumer` for a session that dialed `path`, reducing its grant to
	/// what the origin scopes by.
	pub fn new(path: &str, consumer: lease::Consumer) -> Result<Self, Error> {
		let grant = consumer.grant();
		Ok(Self {
			token: Token::new(path, &grant)?,
			expires: grant.expires,
			consumer,
		})
	}

	/// The scope the session was admitted under.
	pub fn token(&self) -> &Token {
		&self.token
	}

	/// Wait for the lease to stop covering the session: the grant expired, was
	/// revoked, or was re-checked into one that no longer covers the token.
	///
	/// A changed root or a narrower grant ends it: the origin cannot be resized in
	/// place until pattern scopes land. A changed tier is kept for this session and
	/// applies to its next connection, since the stats carriers resolved their
	/// counters at admission.
	pub async fn ended(&mut self) -> lease::Reason {
		loop {
			let expire = async {
				match self.expires {
					Some(at) => tokio::time::sleep(at.duration_since(SystemTime::now()).unwrap_or_default()).await,
					None => std::future::pending().await,
				}
			};
			tokio::select! {
				changed = self.consumer.changed() => match changed {
					Ok(grant) => match self.token.recheck(&grant) {
						Ok(fresh) if fresh.root != self.token.root => return "root changed".into(),
						Ok(fresh) if !self.token.covered_by(&fresh) => return "grant narrowed".into(),
						Ok(fresh) => {
							if fresh.tier != self.token.tier {
								tracing::info!(from = %self.token.tier, to = %fresh.tier, "tier changed; applies to the next session");
							}
							self.expires = grant.expires;
						}
						Err(err) => {
							tracing::warn!(%err, "re-checked grant cannot scope the session");
							return "unsupported grant".into();
						}
					},
					Err(reason) => return reason,
				},
				() = expire => return lease::Reason::Expired,
			}
		}
	}

	/// End the lease with the session's close classification and the totals it
	/// moved, and learn what it ended with: that, or the decider's reason if it
	/// revoked first. Dropping the consumer reports zero bytes.
	pub fn close(self, reason: impl Into<lease::Reason>, bytes: Bytes) -> lease::Reason {
		self.consumer.close(reason, bytes)
	}
}

enum Mode {
	Server(moq_auth::Client),
	Public(Grant),
	/// The embedding process decides: every session is queued for whoever holds
	/// the [`Admissions`], and admits nothing once they are gone.
	Embedded(mpsc::UnboundedSender<Admission>),
	/// Nothing admits an ordinary session; only a locally decided grant (the LAN
	/// mesh credential) gets through. What a `--cluster-lan` process with no
	/// listener of its own runs.
	Refuse,
}

/// Admits sessions: asks the server, hands out the static public grant, or queues
/// the session for the embedder.
#[derive(Clone)]
pub struct Auth {
	mode: Arc<Mode>,
	node: Arc<str>,
}

impl Auth {
	/// An `Auth` the embedding process answers for: every session is queued on the
	/// returned [`Admissions`] and waits for its [`Admission`] to be granted or
	/// refused. Dropping the `Admissions` fails every later session as unavailable.
	pub fn embedded(node: impl Into<String>) -> (Self, Admissions) {
		let (sender, receiver) = mpsc::unbounded_channel();
		let auth = Self {
			mode: Arc::new(Mode::Embedded(sender)),
			node: Arc::from(node.into()),
		};
		(auth, Admissions(receiver))
	}

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
		Request::new(self.node.as_ref(), transport, path)
	}

	/// Admit a session: the lease it holds, carrying the scope the origin applies.
	pub async fn admit(&self, request: Request) -> Result<Lease, Error> {
		let path = request.path.clone();
		let consumer = match self.mode.as_ref() {
			Mode::Server(client) => client.connect(request).await?,
			// A certificate is a fact for a server to weigh; with no server it admits
			// nothing on its own, so the peer gets what any anonymous session gets.
			Mode::Public(grant) => lease::Consumer::fixed(grant.clone()),
			Mode::Embedded(admissions) => {
				let (reply, answer) = oneshot::channel();
				admissions
					.send(Admission { request, reply })
					.map_err(|_| Error::Unavailable("nobody is answering admissions".into()))?;
				let consumer = tokio::time::timeout(ADMIT_TIMEOUT, answer)
					.await
					.map_err(|_| Error::Unavailable("the admission timed out".into()))?
					.map_err(|_| Error::Unavailable("the admission went unanswered".into()))??;
				// Held to what a server's answer is held to: a grant that admits nothing
				// or asks for a re-check without a bound is the decider's bug, not a refusal.
				consumer.grant().validate()?;
				consumer
			}
			Mode::Refuse => return Err(Error::Refused),
		};
		Lease::new(&path, consumer)
	}

	/// Admit a session on a grant decided locally, bypassing the server: the LAN
	/// mesh credential, which the relay minted for itself.
	pub(crate) fn admit_fixed(&self, path: &str, grant: Grant) -> Result<Lease, Error> {
		Lease::new(path, lease::Consumer::fixed(grant))
	}
}

/// The sessions an embedded [`Auth`] is waiting to admit, in arrival order.
///
/// The transport has already accepted each one; it waits for its answer, so a
/// slow decider holds connects the way a slow auth server would, and one that
/// takes longer than a server may (ten seconds) is refused as unavailable.
/// Answer in place or hand each [`Admission`] to its own task; nothing here
/// serializes them.
pub struct Admissions(mpsc::UnboundedReceiver<Admission>);

impl Admissions {
	/// The next session to decide, or `None` once every clone of the [`Auth`] is gone.
	pub async fn next(&mut self) -> Option<Admission> {
		self.0.recv().await
	}
}

/// One session waiting to be admitted: what the relay knows, and the two answers.
///
/// Dropping it unanswered fails the session as unavailable.
pub struct Admission {
	/// The `connect` request the relay built: every fact the transport knows.
	pub request: Request,
	reply: oneshot::Sender<Result<lease::Consumer, Error>>,
}

impl Admission {
	/// Admit the session on `lease`: [`lease::Consumer::fixed`] for a grant that
	/// never changes, or the consumer of a [`lease::Producer`] the decider keeps to
	/// re-check, update, revoke, and learn when the session ends.
	pub fn grant(self, lease: lease::Consumer) {
		// The session may have given up waiting; nothing to tell it then.
		let _ = self.reply.send(Ok(lease));
	}

	/// Refuse the session, with the reason its transport reports.
	pub fn refuse(self, err: Error) {
		let _ = self.reply.send(Err(err));
	}
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

	fn config(url: Option<&str>, public: &[&str]) -> Config {
		Config {
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

		let split = Config {
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
		let lease = futures::executor::block_on(auth.admit(request)).unwrap();
		assert_eq!(lease.token().root, Path::new("anon/room").to_owned());
		assert_eq!(lease.token().subscribe, patterns(&["anon/**"]));
		assert_eq!(lease.token().tier, Tier::default());
	}

	/// The embedder's answer is the session's verdict; an answer that never comes,
	/// or a decider that is gone, is an outage rather than a refusal.
	#[tokio::test]
	async fn an_embedded_auth_admits_what_the_embedder_answers() {
		let (auth, mut admissions) = Auth::embedded("relay-1");
		let request = || auth.request(moq_auth::Transport::Quic, "/anon/room");

		let decide = async {
			let admission = admissions.next().await.expect("an admission");
			assert_eq!(admission.request.path, "/anon/room");
			admission.grant(lease::Consumer::fixed(Grant::new(patterns(&["**"]), patterns(&["**"]))));
			let admission = admissions.next().await.expect("an admission");
			admission.refuse(Error::Refused);
			// A grant that admits nothing is the decider's mistake, refused like a server's.
			let admission = admissions.next().await.expect("an admission");
			admission.grant(lease::Consumer::fixed(Grant::new(Patterns::new(), Patterns::new())));
			// Dropped without an answer.
			drop(admissions.next().await.expect("an admission"));
			admissions
		};
		let admit = async {
			let lease = auth.admit(request()).await.expect("granted");
			assert_eq!(lease.token().root, Path::new("anon/room").to_owned());
			assert!(matches!(auth.admit(request()).await, Err(Error::Refused)));
			for _ in 0..2 {
				assert!(matches!(auth.admit(request()).await, Err(Error::Unavailable(_))));
			}
		};
		let (admissions, ()) = tokio::join!(decide, admit);

		drop(admissions);
		assert!(matches!(auth.admit(request()).await, Err(Error::Unavailable(_))));
	}

	#[test]
	fn token_reduces_a_grant_and_refuses_what_it_cannot_scope() {
		let mut grant = Grant::new(patterns(&["alice/**"]), patterns(&["**"]));
		grant.root = Some("pid/room".into());
		grant.tier = Some("gold".into());
		let token = Token::new("/vanity/room", &grant).unwrap();
		assert_eq!(token.root, Path::new("pid/room").to_owned());
		assert_eq!(token.publish, patterns(&["alice/**"]));
		assert_eq!(token.subscribe, patterns(&["**"]));
		assert_eq!(token.tier, Tier::new("gold"));

		for pattern in ["*/chat", "alice", ""] {
			let grant = Grant::new(patterns(&[pattern]), Patterns::new());
			let err = Token::new("/", &grant).unwrap_err();
			assert!(
				matches!(&err, Error::UnsupportedPattern(p) if p == pattern),
				"{pattern}: {err}"
			);
		}
	}

	#[test]
	fn a_narrower_recheck_is_not_covered() {
		let wide = Token::new("/room", &Grant::new(patterns(&["**"]), patterns(&["**"]))).unwrap();
		let narrow = Token::new("/room", &Grant::new(patterns(&["alice/**"]), patterns(&["**"]))).unwrap();
		let moved = Token::new("/other", &Grant::new(patterns(&["**"]), patterns(&["**"]))).unwrap();
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
		let token = Token::new("/vanity/room", &aliased).unwrap();
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
