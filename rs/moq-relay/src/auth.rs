//! How a session is admitted: through an auth server behind `--auth-url`, with the
//! static grant `--auth-public` names for anonymous sessions, or by the embedding
//! process answering [`Admissions`]. Nothing else admits anyone; a verified client
//! certificate is reported to whoever decides as a fact.

use std::sync::Arc;

use axum::http;
use moq_auth::{Bytes, Grant, Request, lease};
use moq_auth::{Pattern, Patterns};
use moq_net::{Path, PathOwned, stats::Tier};
use serde::{Deserialize, Serialize};
use serde_with::{OneOrMany, serde_as};
use tokio::sync::{mpsc, oneshot};
use url::Url;

/// The longest a decider may take to answer an admission, the bound
/// `moq_auth::Client` puts on a server, so a stalled decider refuses rather
/// than parks the session behind it.
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

	/// Patterns an anonymous session may both publish and subscribe to, rooted at
	/// `/`, such as `anon/**`. Repeatable or comma-separated. Sets a static grant
	/// with no expiry and no server.
	#[usage(
		long = "auth-public",
		env = "MOQ_AUTH_PUBLIC",
		setting = "auth.public",
		delimiter = ','
	)]
	#[serde(skip_serializing_if = "Vec::is_empty")]
	#[serde_as(as = "OneOrMany<_>")]
	pub public: Vec<Pattern>,

	/// Patterns an anonymous session may subscribe to, rooted at `/`. Repeatable.
	#[usage(
		long = "auth-public-subscribe",
		env = "MOQ_AUTH_PUBLIC_SUBSCRIBE",
		setting = "auth.public_subscribe",
		delimiter = ','
	)]
	#[serde(skip_serializing_if = "Vec::is_empty")]
	#[serde_as(as = "OneOrMany<_>")]
	pub public_subscribe: Vec<Pattern>,

	/// Patterns an anonymous session may publish, rooted at `/`. Repeatable.
	#[usage(
		long = "auth-public-publish",
		env = "MOQ_AUTH_PUBLIC_PUBLISH",
		setting = "auth.public_publish",
		delimiter = ','
	)]
	#[serde(skip_serializing_if = "Vec::is_empty")]
	#[serde_as(as = "OneOrMany<_>")]
	pub public_publish: Vec<Pattern>,

	/// The 0.14 flags and env vars #3688 removed, kept parsing but hidden. Never
	/// read as settings: [`deprecated`](Self::deprecated) names what replaced each.
	#[usage(flatten)]
	#[serde(skip)]
	pub(crate) legacy: Legacy,

	// The 0.14 `[auth]` keys #3688 removed, kept only so they refuse with their
	// replacement named rather than being ignored.
	#[usage(skip)]
	#[serde(skip_serializing)]
	pub(crate) key: Option<serde::de::IgnoredAny>,
	#[usage(skip)]
	#[serde(skip_serializing)]
	pub(crate) key_dir: Option<serde::de::IgnoredAny>,
	#[usage(skip)]
	#[serde(skip_serializing)]
	pub(crate) auth_api: Option<serde::de::IgnoredAny>,
	#[usage(skip)]
	#[serde(skip_serializing)]
	pub(crate) domains: Option<serde::de::IgnoredAny>,
	#[usage(skip)]
	#[serde(skip_serializing)]
	pub(crate) mtls_tier: Option<serde::de::IgnoredAny>,
	#[usage(skip)]
	#[serde(skip_serializing)]
	pub(crate) tls: Option<serde::de::IgnoredAny>,
}

/// The `--auth-*` flags 0.14 had and #3688 removed, with their env vars: a relay
/// deployed through the environment would otherwise ignore them without a word.
#[derive(Clone, Debug, Default, usage::Args)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub(crate) struct Legacy {
	#[usage(name = "auth-key", long = "auth-key", env = "MOQ_AUTH_KEY", hide = true)]
	key: Option<String>,
	#[usage(name = "auth-key-dir", long = "auth-key-dir", env = "MOQ_AUTH_KEY_DIR", hide = true)]
	key_dir: Option<String>,
	#[usage(name = "auth-api", long = "auth-api", env = "MOQ_AUTH_API", hide = true)]
	api: Option<String>,
	#[usage(
		name = "auth-public-api",
		long = "auth-public-api",
		env = "MOQ_AUTH_PUBLIC_API",
		hide = true
	)]
	public_api: Option<String>,
	#[usage(name = "auth-domain", long = "auth-domain", env = "MOQ_AUTH_DOMAIN", hide = true)]
	domain: Vec<String>,
	#[usage(
		name = "auth-mtls-tier",
		long = "auth-mtls-tier",
		env = "MOQ_AUTH_MTLS_TIER",
		hide = true
	)]
	mtls_tier: Option<String>,
	#[usage(
		name = "auth-tls-root",
		long = "auth-tls-root",
		env = "MOQ_AUTH_TLS_ROOT",
		hide = true
	)]
	tls_root: Vec<String>,
	#[usage(
		name = "auth-tls-cert",
		long = "auth-tls-cert",
		env = "MOQ_AUTH_TLS_CERT",
		hide = true
	)]
	tls_cert: Option<String>,
	#[usage(name = "auth-tls-key", long = "auth-tls-key", env = "MOQ_AUTH_TLS_KEY", hide = true)]
	tls_key: Option<String>,
	#[usage(
		name = "auth-tls-disable-verify",
		long = "auth-tls-disable-verify",
		env = "MOQ_AUTH_TLS_DISABLE_VERIFY",
		hide = true,
		default_missing = "true",
		num_args = 0..=1,
		require_equals = true
	)]
	tls_disable_verify: Option<String>,
}

impl Config {
	/// The static grant the public patterns name, or `None` when none is set.
	///
	/// Its patterns are rooted at `/`, not at a dialed path: each session is granted
	/// what they reach from where it dialed, as a token with an empty root would be.
	pub fn public_grant(&self) -> Option<Grant> {
		let publish: Patterns = self.public.iter().chain(&self.public_publish).cloned().collect();
		let subscribe: Patterns = self.public.iter().chain(&self.public_subscribe).cloned().collect();
		(!publish.is_empty() || !subscribe.is_empty()).then(|| Grant::new(publish, subscribe))
	}

	/// The 0.14 settings in use, each paired with what replaced it. A relay refuses
	/// to start on any: `key` with `public` would otherwise boot with every JWT
	/// ignored.
	pub fn deprecated(&self) -> moq_tokio::cli::Deprecated {
		let mut found = moq_tokio::cli::Deprecated::default();
		let legacy = &self.legacy;
		for (set, flag, env, toml, new) in [
			(
				legacy.key.is_some(),
				"--auth-key",
				"MOQ_AUTH_KEY",
				self.key.is_some().then_some("[auth] key"),
				"--auth-url to `moq auth serve --key`",
			),
			(
				legacy.key_dir.is_some(),
				"--auth-key-dir",
				"MOQ_AUTH_KEY_DIR",
				self.key_dir.is_some().then_some("[auth] key_dir"),
				"--auth-url to `moq auth serve --key-dir`",
			),
			(
				legacy.api.is_some(),
				"--auth-api",
				"MOQ_AUTH_API",
				self.auth_api.is_some().then_some("[auth] auth_api"),
				"--auth-url to a server answering the contract",
			),
			(
				legacy.public_api.is_some(),
				"--auth-public-api",
				"MOQ_AUTH_PUBLIC_API",
				None,
				"--auth-url to a server answering the contract",
			),
			(
				!legacy.domain.is_empty(),
				"--auth-domain",
				"MOQ_AUTH_DOMAIN",
				self.domains.is_some().then_some("[auth] domains"),
				"an auth server that reads `server_name`",
			),
			(
				legacy.mtls_tier.is_some(),
				"--auth-mtls-tier",
				"MOQ_AUTH_MTLS_TIER",
				self.mtls_tier.is_some().then_some("[auth] mtls_tier"),
				"--auth-url to `moq auth serve --tier`",
			),
			(
				!legacy.tls_root.is_empty()
					|| legacy.tls_cert.is_some()
					|| legacy.tls_key.is_some()
					|| legacy.tls_disable_verify.is_some(),
				"--auth-tls-*",
				"MOQ_AUTH_TLS_*",
				self.tls.is_some().then_some("[auth.tls]"),
				"--connect-tls-*, which an https:// --auth-url presents",
			),
		] {
			if set {
				found.flag(flag, Some(env), new);
			}
			if let Some(toml) = toml {
				found.toml(toml, new, None);
			}
		}
		found
	}

	/// Whether no source is named at all. Such a relay admits nothing on its own:
	/// [`Relay::load`](crate::Relay::load) hands its sessions to the embedder as
	/// [`Admissions`], and [`validate`](Self::validate) refuses it for a binary.
	pub fn is_empty(&self) -> bool {
		self.url.is_none() && self.public_grant().is_none()
	}

	/// Refuse a configuration that admits nobody, or that names both a server and
	/// a static grant, so the question of who decides has one answer.
	///
	/// A public pattern without a wildcard is refused too. 0.14 read `anon` as the
	/// prefix `anon/`, and a pattern reads it as exactly the broadcast `anon`, so
	/// either silent reading would mislead someone upgrading.
	pub fn validate(&self) -> anyhow::Result<()> {
		let flags = [
			("--auth-public", &self.public),
			("--auth-public-publish", &self.public_publish),
			("--auth-public-subscribe", &self.public_subscribe),
		];
		for (flag, patterns) in flags {
			for pattern in patterns.iter().filter(|pattern| pattern.is_literal()) {
				// A literal at the maximum depth is already its own subtree.
				let subtree = Pattern::subtree(pattern.as_str())?;
				if subtree != *pattern {
					anyhow::bail!(
						"{flag} `{pattern}` has no wildcard, so it names exactly one broadcast; write `{subtree}` for the subtree"
					);
				}
			}
		}
		match (&self.url, self.public_grant()) {
			(Some(_), Some(_)) => anyhow::bail!("--auth-url and --auth-public cannot both be set; the server decides"),
			(None, None) => anyhow::bail!(
				"no --auth-url or --auth-public configured; nobody can authenticate (a client certificate admits nothing on its own)"
			),
			_ => Ok(()),
		}
	}

	/// Refuse a client CA under public rules, which grant a certificate what they
	/// grant anyone. `client_ca` is whether any listener verifies client certificates.
	pub fn validate_client_ca(&self, client_ca: bool) -> anyhow::Result<()> {
		if client_ca && self.url.is_none() && self.public_grant().is_some() {
			anyhow::bail!(
				"a client CA (--listen-tls-root, --web-https-root) verifies client certificates, which --auth-public ignores; remove it, or grant certificates with --auth-url to `moq auth serve --mtls-*`"
			);
		}
		Ok(())
	}

	/// Build the [`Auth`] this configuration describes. `tls` is the client
	/// identity an `https://` server is dialed with; `node` names this relay in
	/// every request. Must be called within a Tokio runtime, which drives the
	/// admission decider.
	pub fn init(&self, node: impl Into<String>, tls: &moq_tokio::tls::Connect) -> anyhow::Result<Auth> {
		self.validate()?;
		let decider = match (&self.url, self.public_grant()) {
			(Some(url), _) => {
				let tls = tls.build()?;
				Decider::Server(moq_auth::Client::new(url.clone(), Some(tls))?)
			}
			(None, Some(grant)) => Decider::Public(
				moq_auth::Claims::default()
					.with_publish(grant.publish)
					.with_subscribe(grant.subscribe),
			),
			(None, None) => unreachable!("validated above"),
		};
		let (auth, admissions) = Auth::embedded(node);
		decider.spawn(admissions);
		Ok(auth)
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

	/// The relay could not build the request the server needs.
	#[error("{0}")]
	Request(String),
}

impl From<moq_auth::Error> for Error {
	fn from(err: moq_auth::Error) -> Self {
		match err {
			moq_auth::Error::Refused | moq_auth::Error::UselessGrant => Self::Refused,
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
	/// Subtrees read from elsewhere on the origin: each path, relative to `root`,
	/// resolves at the absolute path it maps to.
	pub mounts: Vec<(PathOwned, PathOwned)>,
	/// The patterns the holder may subscribe to, relative to `root`.
	pub subscribe: Patterns,
	/// The patterns the holder may publish to, relative to `root`.
	pub publish: Patterns,
	/// The tier this session's stats record under.
	pub tier: Tier,
	/// Whether the session is a cluster peer, so its routes entered elsewhere.
	pub peer: bool,
}

impl Token {
	/// Reduce `grant` for a session that dialed `path`.
	///
	/// The token keeps the patterns the grant yields: the origin scopes by any
	/// pattern union, so `alice`, `*/chat`, and `alice/**` each mean exactly what
	/// they say.
	pub fn new(path: &str, grant: &Grant) -> Self {
		let root = grant.root.as_deref().unwrap_or(path);
		Self {
			path: path.to_string(),
			root: Path::new(root).to_owned(),
			mounts: grant
				.mounts
				.iter()
				.map(|(at, target)| (Path::new(at).to_owned(), Path::new(target).to_owned()))
				.collect(),
			subscribe: grant.subscribe.clone(),
			publish: grant.publish.clone(),
			tier: crate::configured_tier(grant.tier.clone()),
			peer: grant.peer,
		}
	}

	/// Rebuild the token from a re-checked grant, relative to the same dialed path,
	/// so a grant that drops its `root` alias resolves back to what was dialed.
	pub(crate) fn recheck(&self, grant: &Grant) -> Self {
		Self::new(&self.path, grant)
	}

	/// Whether `other` still covers everything this token scopes: the same root and
	/// mounts, and every grant still held. A narrower re-check closes the session until
	/// pattern scopes can resize it in place.
	pub(crate) fn covered_by(&self, other: &Self) -> bool {
		self.root == other.root
			&& self.mounts == other.mounts
			&& other.subscribe.covers(&self.subscribe)
			&& other.publish.covers(&self.publish)
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
	/// grant has no driver, and an auth server's may be mid-outage. Fixed on tokio's
	/// clock when the grant arrives, so re-polling [`ended`](Self::ended) never
	/// restarts the countdown.
	expires: Option<tokio::time::Instant>,
	/// The session's stats context, moved to a re-checked tier.
	stats: moq_net::stats::Session,
}

impl Lease {
	/// Hold `consumer` for a session that dialed `path`, reducing its grant to
	/// what the origin scopes by.
	pub fn new(path: &str, consumer: lease::Consumer) -> Self {
		let grant = consumer.grant();
		Self {
			token: Token::new(path, &grant),
			expires: grant.deadline(),
			consumer,
			stats: Default::default(),
		}
	}

	/// Attach the session's stats context, so a re-checked tier retags it live.
	pub fn with_stats(mut self, stats: moq_net::stats::Session) -> Self {
		self.stats = stats;
		self
	}

	/// The scope the session was admitted under.
	pub fn token(&self) -> &Token {
		&self.token
	}

	/// Ask the lease's producer to re-check now. A no-op on a fixed lease.
	pub fn revalidate(&self) {
		self.consumer.revalidate();
	}

	/// Wait for the lease to stop covering the session: the grant expired, was
	/// revoked, or was re-checked into one that no longer covers the token.
	///
	/// A changed root or mounts, or a narrower grant, ends it: origin handles cannot yet narrow
	/// a live scope in place (tracked by `quest/m1/auth/narrowing.md`). A flipped
	/// `peer` ends it too, since the routes it already announced would be
	/// misreported as entering here or from a peer. A changed tier keeps the
	/// session and moves its [stats](Self::with_stats) to the new tier.
	pub async fn ended(&mut self) -> lease::Reason {
		loop {
			let expire = async {
				match self.expires {
					Some(at) => tokio::time::sleep_until(at).await,
					None => std::future::pending().await,
				}
			};
			tokio::select! {
				changed = self.consumer.changed() => match changed {
					Ok(grant) => {
						let fresh = self.token.recheck(&grant);
						if fresh.root != self.token.root {
							return "root changed".into();
						}
						if fresh.mounts != self.token.mounts {
							return "mounts changed".into();
						}
						// Routes the session already announced were recorded as
						// entering here or from a peer; a flip would misreport them.
						if fresh.peer != self.token.peer {
							return "peer changed".into();
						}
						if !self.token.covered_by(&fresh) {
							return "grant narrowed".into();
						}
						if fresh.tier != self.token.tier {
							tracing::info!(from = %self.token.tier, to = %fresh.tier, "tier changed");
							self.stats.set_tier(fresh.tier.clone());
							self.token.tier = fresh.tier;
						}
						self.expires = grant.deadline();
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

enum Decider {
	Server(moq_auth::Client),
	/// The public rules, as a token with an empty root.
	Public(moq_auth::Claims),
	Refuse,
}

impl Decider {
	fn spawn(self, mut admissions: Admissions) {
		tokio::spawn(async move {
			while let Some(admission) = admissions.next().await {
				match &self {
					Self::Server(client) => {
						let client = client.clone();
						tokio::spawn(async move {
							match client.connect(admission.request.clone()).await {
								Ok(lease) => admission.grant(lease),
								Err(err) => admission.refuse(err.into()),
							}
						});
					}
					// Nothing here can verify a token, so one is refused rather than ignored:
					// its holder expects it to count, and the public grant is not what it says.
					Self::Public(_) if presents_token(&admission.request) => {
						tracing::debug!(path = %admission.request.path, "a token was presented to public rules");
						admission.refuse(Error::Refused);
					}
					Self::Public(rules) => match rules.authorize(&admission.request.path) {
						Ok(access) => {
							let grant = Grant::new(access.publish, access.subscribe);
							admission.grant(lease::Consumer::fixed(grant));
						}
						// A path the rules don't reach is a refusal, never an outage a
						// client would retry.
						Err(err) => {
							tracing::debug!(path = %admission.request.path, %err, "public rules refused");
							admission.refuse(Error::Refused);
						}
					},
					Self::Refuse => admission.refuse(Error::Refused),
				}
			}
		});
	}
}

/// Whether `request` carries a token: a SETUP token of any type, or a non-empty
/// `jwt` query parameter, the convention `moq auth serve` reads.
fn presents_token(request: &Request) -> bool {
	request.token.is_some()
		|| request.query.as_deref().is_some_and(|query| {
			query
				.split('&')
				.any(|pair| pair.strip_prefix("jwt=").is_some_and(|jwt| !jwt.is_empty()))
		})
}

/// Admits sessions by queueing every request for one admission decider.
#[derive(Clone)]
pub struct Auth {
	admissions: mpsc::UnboundedSender<Admission>,
	node: Arc<str>,
}

impl Auth {
	/// An `Auth` the embedding process answers for: every session is queued on the
	/// returned [`Admissions`] and waits for its [`Admission`] to be granted or
	/// refused. Dropping the `Admissions` fails every later session as unavailable.
	pub fn embedded(node: impl Into<String>) -> (Self, Admissions) {
		let (sender, receiver) = mpsc::unbounded_channel();
		let auth = Self {
			admissions: sender,
			node: Arc::from(node.into()),
		};
		(auth, Admissions(receiver))
	}

	/// An `Auth` that refuses every session a server or a public grant would have
	/// decided, admitting only what the relay decides for itself (a LAN peer).
	/// Must be called within a Tokio runtime, which drives the refusal decider.
	pub fn refuse(node: impl Into<String>) -> Self {
		let (auth, admissions) = Self::embedded(node);
		Decider::Refuse.spawn(admissions);
		auth
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
		let (reply, answer) = oneshot::channel();
		self.admissions
			.send(Admission { request, reply })
			.map_err(|_| Error::Unavailable("nobody is answering admissions".into()))?;
		let consumer = tokio::time::timeout(ADMIT_TIMEOUT, answer)
			.await
			.map_err(|_| Error::Unavailable("the admission timed out".into()))?
			.map_err(|_| Error::Unavailable("the admission went unanswered".into()))??;
		// Every decider is held to the same answer contract: a grant that admits
		// nothing or asks for a re-check without a bound is a bug, not a refusal.
		consumer.grant().validate()?;
		Ok(Lease::new(&path, consumer))
	}

	/// Admit a session on a grant decided locally, bypassing the server: the LAN
	/// mesh credential, which the relay minted for itself.
	pub(crate) fn admit_fixed(&self, path: &str, grant: Grant) -> Lease {
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
pub fn request_for(auth: &Auth, request: &moq_tokio::server::Request) -> Request {
	let transport = match request.transport() {
		moq_tokio::server::Transport::Quic => moq_auth::Transport::Quic,
		moq_tokio::server::Transport::Iroh => moq_auth::Transport::Iroh,
		moq_tokio::server::Transport::WebSocket => moq_auth::Transport::WebSocket,
		moq_tokio::server::Transport::Tcp => moq_auth::Transport::Tcp,
		moq_tokio::server::Transport::Unix => moq_auth::Transport::Unix,
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
	out.token = request.token().map(|token| moq_auth::Token {
		kind: token.kind,
		value: token.value.clone(),
	});
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
	use std::time::SystemTime;

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

	/// The public rules are rooted at `/`, like a token with an empty root. Bare `**`
	/// reads the same either way, which is how rooting them at the dialed path went
	/// unnoticed: `anon/**` at `/rooms/123` granted `rooms/123/anon/**`.
	#[tokio::test]
	async fn public_rules_are_rooted_at_slash() {
		let auth = Config {
			public_publish: patterns(&["anon/**"]).into_iter().collect(),
			public_subscribe: patterns(&["anon/**", "*/chat"]).into_iter().collect(),
			..Default::default()
		}
		.init("relay-1", &moq_tokio::tls::Connect::default())
		.unwrap();

		for (path, root, publish, subscribe) in [
			("/", "", &["anon/**"][..], &["anon/**", "*/chat"][..]),
			("/anon", "anon", &["**"], &["**", "chat"]),
			("/anon/room", "anon/room", &["**"], &["**"]),
			("/rooms", "rooms", &[], &["chat"]),
		] {
			let lease = auth.admit(auth.request(moq_auth::Transport::Quic, path)).await.unwrap();
			assert_eq!(lease.token().root, Path::new(root).to_owned(), "{path}");
			assert_eq!(lease.token().publish, patterns(publish), "{path}");
			assert_eq!(lease.token().subscribe, patterns(subscribe), "{path}");
		}

		// A path the rules don't reach is refused, never an outage the client retries.
		for path in ["/rooms/123", "/other/room", "/anonymous/room"] {
			let Err(err) = auth.admit(auth.request(moq_auth::Transport::Quic, path)).await else {
				panic!("{path} was admitted");
			};
			assert!(matches!(err, Error::Refused), "{path}: {err}");
			assert_eq!(http::StatusCode::from(&err), http::StatusCode::UNAUTHORIZED);
		}
	}

	/// Public rules verify nothing, so a token is refused rather than ignored, in
	/// whichever form it arrives.
	#[tokio::test]
	async fn public_rules_refuse_a_token() {
		let auth = config(None, &["**"])
			.init("relay-1", &moq_tokio::tls::Connect::default())
			.unwrap();
		let mut query = auth.request(moq_auth::Transport::WebSocket, "/");
		query.query = Some("a=1&jwt=eyJ".into());
		let mut setup = auth.request(moq_auth::Transport::Quic, "/");
		setup.token = Some(moq_auth::Token {
			kind: moq_auth::Token::CAT,
			value: b"anything".to_vec(),
		});
		let mut http = auth.request(moq_auth::Transport::Http, "/");
		http.query = Some("jwt=eyJ".into());
		for request in [query, setup, http] {
			let Err(err) = auth.admit(request).await else {
				panic!("a token was admitted on the public grant");
			};
			assert!(matches!(err, Error::Refused), "{err}");
		}

		// An empty `jwt=` is no token.
		let mut empty = auth.request(moq_auth::Transport::WebSocket, "/");
		empty.query = Some("jwt=&b=2".into());
		assert!(auth.admit(empty).await.is_ok());
	}

	/// A certificate is a fact the public rules ignore: it gets exactly what anyone does.
	#[tokio::test]
	async fn a_public_config_admits_anonymous_and_certificate_alike() {
		let auth = config(None, &["anon/**"])
			.init("relay-1", &moq_tokio::tls::Connect::default())
			.unwrap();
		let anonymous = auth.request(moq_auth::Transport::Quic, "/anon/room");
		let mut certificate = anonymous.clone();
		certificate.tls = Some(moq_auth::Peer {
			name: "edge0".into(),
			fingerprint: "ab".repeat(32),
			expires: None,
			issuer: "CN=cluster".into(),
		});
		for request in [anonymous, certificate] {
			let lease = auth.admit(request).await.unwrap();
			assert_eq!(lease.token().root, Path::new("anon/room").to_owned());
			assert_eq!(lease.token().subscribe, patterns(&["**"]));
			assert_eq!(lease.token().tier, Tier::default());
		}
	}

	/// 0.14 read `anon` as a prefix and a pattern reads it as one broadcast, so a
	/// wildcard-free public pattern refuses to start rather than pick silently.
	#[test]
	fn a_public_pattern_without_a_wildcard_refuses_to_start() {
		for (public, hint) in [("anon", "anon/**"), ("anon/room", "anon/room/**"), ("", "**")] {
			let err = config(None, &[public]).validate().unwrap_err().to_string();
			assert!(err.contains(&format!("`{hint}`")), "{public}: {err}");
		}
		let split = Config {
			public_subscribe: patterns(&["anon/**", "live"]).into_iter().collect(),
			..Default::default()
		};
		let err = split.validate().unwrap_err().to_string();
		assert!(err.starts_with("--auth-public-subscribe `live`"), "{err}");
		assert!(config(None, &["anon/*", "*/chat", "**"]).validate().is_ok());

		// Nothing sits beneath a literal at the maximum depth, so it is its own subtree.
		let deepest = vec!["a"; Pattern::MAX_SEGMENTS].join("/");
		assert!(config(None, &[&deepest]).validate().is_ok());
	}

	#[test]
	fn config_init_requires_a_runtime() {
		let result = std::panic::catch_unwind(|| {
			let _ = config(None, &["anon/**"])
				.init("relay-1", &moq_tokio::tls::Connect::default())
				.unwrap();
		});
		assert!(result.is_err());
	}

	#[test]
	fn refuse_requires_a_runtime() {
		let result = std::panic::catch_unwind(|| Auth::refuse("relay-1"));
		assert!(result.is_err());
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
			assert!(matches!(auth.admit(request()).await, Err(Error::Refused)));
			assert!(matches!(auth.admit(request()).await, Err(Error::Unavailable(_))));
		};
		let (admissions, ()) = tokio::join!(decide, admit);

		drop(admissions);
		assert!(matches!(auth.admit(request()).await, Err(Error::Unavailable(_))));
	}

	#[tokio::test]
	async fn dropping_the_last_auth_ends_admissions() {
		let (auth, mut admissions) = Auth::embedded("relay-1");
		let clone = auth.clone();
		drop(auth);
		assert!(
			tokio::time::timeout(std::time::Duration::ZERO, admissions.next())
				.await
				.is_err()
		);
		drop(clone);
		assert!(admissions.next().await.is_none());
	}

	#[tokio::test]
	async fn refuse_answers_the_admission_queue() {
		let auth = Auth::refuse("relay-1");
		let request = auth.request(moq_auth::Transport::Quic, "/anon/room");
		assert!(matches!(auth.admit(request).await, Err(Error::Refused)));
	}

	/// The session supervisor re-polls `ended` on every nudge; that must not
	/// restart the countdown to `expires`.
	#[tokio::test(start_paused = true)]
	async fn re_polling_keeps_the_expiry_deadline() {
		use std::time::Duration;

		let mut grant = Grant::new(patterns(&["**"]), patterns(&["**"]));
		grant.expires = Some(SystemTime::now() + Duration::from_secs(10));
		let mut lease = Lease::new("/room", lease::Consumer::fixed(grant));
		for _ in 0..9 {
			assert!(
				tokio::time::timeout(Duration::from_secs(1), lease.ended())
					.await
					.is_err()
			);
		}
		let reason = tokio::time::timeout(Duration::from_secs(2), lease.ended())
			.await
			.expect("expired at the deadline despite re-polling");
		assert_eq!(reason, lease::Reason::Expired);
	}

	#[tokio::test(start_paused = true)]
	async fn an_expired_grant_ends_at_once() {
		use std::time::Duration;

		let mut grant = Grant::new(patterns(&["**"]), patterns(&["**"]));
		grant.expires = Some(SystemTime::now() - Duration::from_secs(1));
		assert!(matches!(grant.validate(), Err(moq_auth::Error::GrantExpired)));

		let start = tokio::time::Instant::now();
		let mut lease = Lease::new("/room", lease::Consumer::fixed(grant));
		assert_eq!(lease.ended().await, lease::Reason::Expired);
		// A millisecond for Tokio's timer resolution.
		assert!(start.elapsed() <= Duration::from_millis(1), "no grace after expiry");
	}

	#[test]
	fn token_keeps_every_grant_pattern() {
		let mut grant = Grant::new(patterns(&["alice/**"]), patterns(&["**"]));
		grant.root = Some("pid/room".into());
		grant.tier = Some("gold".into());
		let token = Token::new("/vanity/room", &grant);
		assert_eq!(token.root, Path::new("pid/room").to_owned());
		assert_eq!(token.publish, patterns(&["alice/**"]));
		assert_eq!(token.subscribe, patterns(&["**"]));
		assert_eq!(token.tier, Tier::new("gold"));

		let patterns = patterns(&["*/chat", "alice", ""]);
		let token = Token::new("/", &Grant::new(patterns.clone(), Patterns::new()));
		assert_eq!(token.publish, patterns);
	}

	#[test]
	fn a_narrower_recheck_is_not_covered() {
		let wide = Token::new("/room", &Grant::new(patterns(&["**"]), patterns(&["**"])));
		let narrow = Token::new("/room", &Grant::new(patterns(&["alice/**"]), patterns(&["**"])));
		let moved = Token::new("/other", &Grant::new(patterns(&["**"]), patterns(&["**"])));
		let mut mounted = Grant::new(patterns(&["**"]), patterns(&["**"]));
		mounted.mounts.insert(".svc".into(), ".svc/room".into());
		let mounted = Token::new("/room", &mounted);
		assert_eq!(
			mounted.mounts,
			[(Path::new(".svc").to_owned(), Path::new(".svc/room").to_owned())]
		);
		assert!(wide.covered_by(&wide));
		assert!(narrow.covered_by(&wide));
		assert!(!wide.covered_by(&narrow));
		assert!(!wide.covered_by(&moved));
		// A mount moves what a path resolves to, so either way it is a new scope.
		assert!(!wide.covered_by(&mounted));
		assert!(!mounted.covered_by(&wide));
	}

	/// Routes a session announced were recorded as a peer's or not; a re-check that
	/// flips it closes the session rather than misreport them.
	#[tokio::test]
	async fn a_recheck_that_flips_peer_closes() {
		let grant = Grant::new(patterns(&["**"]), patterns(&["**"]));
		let (producer, consumer) = lease::Producer::new(grant.clone());
		let mut lease = Lease::new("/", consumer);
		assert!(!lease.token().peer);

		let mut peer = grant;
		peer.peer = true;
		producer.update(peer);
		assert_eq!(lease.ended().await.to_string(), "peer changed");
	}

	#[test]
	fn a_recheck_resolves_a_dropped_root_to_the_dialed_path() {
		let everything = || Grant::new(patterns(&["**"]), patterns(&["**"]));
		let mut aliased = everything();
		aliased.root = Some("pid/room".into());
		let token = Token::new("/vanity/room", &aliased);
		assert_eq!(token.root, Path::new("pid/room").to_owned());

		// The same alias still names the same root.
		assert_eq!(token.recheck(&aliased).root, token.root);
		// A grant without the alias is relative to what was dialed, not to the old root.
		assert_eq!(token.recheck(&everything()).root, Path::new("vanity/room").to_owned());
	}
}
