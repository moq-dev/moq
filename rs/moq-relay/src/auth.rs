//! How a session is admitted: through an auth server behind `--auth-url`, with the
//! static grant `--auth-public` names for anonymous sessions, or by the embedding
//! process answering [`Admissions`]. Nothing else admits anyone; a verified client
//! certificate is reported to whoever decides as a fact.

mod refresh;

use std::sync::Arc;

use axum::http;
use moq_auth::{Bytes, Grant, Request, lease};
use moq_auth::{Pattern, Patterns};
use moq_net::{Path, stats::Tier};
use serde::{Deserialize, Serialize};
use serde_with::{OneOrMany, serde_as};
use tokio::sync::{mpsc, oneshot};
use url::Url;

/// The longest a decider may take to verify a token presented in band. A
/// connection's admission has no bound of its own: it runs under the listener's
/// handshake deadline, which a token arriving after the handshake does not.
const ATTACH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Where every session's grant comes from. Exactly one of `url` and the public
/// patterns is set; [`validate`](Self::validate) refuses anything else.
#[serde_as]
#[derive(usage_rs::Args, Clone, Debug, Default, Serialize, Deserialize)]
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

	/// PEM roots used instead of system roots to verify the auth server's hostname.
	#[usage(long = "auth-root", env = "MOQ_AUTH_ROOT", setting = "auth.root")]
	#[usage(value_hint = usage_rs::ValueHint::FilePath, extensions("pem", "crt", "cer"))]
	#[serde(skip_serializing_if = "Vec::is_empty")]
	#[serde_as(as = "OneOrMany<_>")]
	pub root: Vec<std::path::PathBuf>,

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

	#[usage(skip)]
	#[serde(rename = "verifications", skip_serializing)]
	pub(crate) removed_verifications: Option<serde::de::IgnoredAny>,

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

/// Maximum in-band token verifications one session, or requests one HTTP/2
/// connection, may hold at its decider.
const VERIFICATIONS: usize = 128;

/// The `--auth-*` flags 0.14 had and #3688 removed, with their env vars: a relay
/// deployed through the environment would otherwise ignore them without a word.
#[derive(Clone, Debug, Default, usage_rs::Args)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub(crate) struct Legacy {
	#[usage(long = "auth-verifications", env = "MOQ_AUTH_VERIFICATIONS", hide = true)]
	verifications: Option<usize>,

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
		if self.legacy.verifications.is_some() {
			found.changed(
				"--auth-verifications",
				Some("MOQ_AUTH_VERIFICATIONS"),
				"the fixed per-session verification policy",
				"the configurable verification window is removed",
			);
		}
		if self.removed_verifications.is_some() {
			found.toml(
				"[auth] verifications",
				"the fixed per-session verification policy",
				None,
			);
		}

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
				"--connect-tls-cert/--connect-tls-key for identity; --auth-root for server trust",
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

	/// Refuse a configuration that admits nobody, that names both a server and a
	/// static grant, or that pairs public rules with a client CA, so the question
	/// of who decides has one answer. `client_ca` is whether any listener verifies
	/// client certificates, which public rules would grant what they grant anyone.
	///
	/// A public pattern without a wildcard is refused too. 0.14 read `anon` as the
	/// prefix `anon/`, and a pattern reads it as exactly the broadcast `anon`, so
	/// either silent reading would mislead someone upgrading.
	pub fn validate(&self, client_ca: bool) -> anyhow::Result<()> {
		let deprecated = self.deprecated();
		anyhow::ensure!(deprecated.is_empty(), "{deprecated}");
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
			(None, Some(_)) if client_ca => anyhow::bail!(
				"a client CA (--listen-tls-root, --web-https-root) verifies client certificates, which --auth-public ignores; remove it, or grant certificates with --auth-url to `moq auth serve --mtls-*`"
			),
			_ => {}
		}
		Ok(())
	}

	/// Build the [`Auth`] this configuration describes, after [`validate`](Self::validate)
	/// with the same `client_ca`. `tls` is the client identity an `https://` server
	/// is dialed with, never its server trust; `node` names this relay in every request.
	/// Must be called within a Tokio runtime, which drives the admission decider.
	pub fn init(
		&self,
		node: impl Into<String>,
		tls: &moq_tokio::tls::Connect,
		client_ca: bool,
	) -> anyhow::Result<Auth> {
		self.validate(client_ca)?;
		let decider = match (&self.url, self.public_grant()) {
			(Some(url), _) => {
				let tls = tls.build()?;
				let tls = crate::http_client::server_tls(&tls, &self.root)?;
				Decider::Server(moq_auth::Client::new(url.clone(), Some(tls))?)
			}
			(None, Some(grant)) => Decider::Public(
				moq_auth::Claims::default()
					.with_publish(grant.publish)
					.with_subscribe(grant.subscribe),
			),
			(None, None) => unreachable!("validated above"),
		};
		let (mut auth, admissions) = Auth::embedded(node);
		auth.in_band = matches!(decider, Decider::Server(_));
		decider.spawn(admissions);
		Ok(auth)
	}
}

/// Why a session was refused, with the HTTP status and session code that report it.
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum Error {
	/// The auth server answered and the answer was no.
	#[error(transparent)]
	Refused(moq_auth::Refusal),

	/// The auth server could not be asked, or answered nonsense; nothing is
	/// admitted because the server was down.
	#[error("auth server unavailable: {0}")]
	Unavailable(String),

	/// A valid grant does not cover the requested direction or path.
	#[error("{0}")]
	Forbidden(String),

	/// The relay could not build the request the server needs.
	#[error("{0}")]
	Request(String),
}

impl From<moq_auth::Error> for Error {
	fn from(err: moq_auth::Error) -> Self {
		match err {
			moq_auth::Error::Refused(refusal) => Self::Refused(refusal),
			moq_auth::Error::UselessGrant => Self::Refused(Default::default()),
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
			Error::Forbidden(_) => http::StatusCode::FORBIDDEN,
			Error::Refused(refusal) if refusal.reason == moq_auth::refusal::Reason::Forbidden => {
				http::StatusCode::FORBIDDEN
			}
			_ => http::StatusCode::UNAUTHORIZED,
		}
	}
}

impl From<Error> for http::StatusCode {
	fn from(err: Error) -> Self {
		Self::from(&err)
	}
}

/// The code a refused session closes with, or a refused in-band token is answered with.
impl From<&Error> for moq_net::SessionError {
	fn from(err: &Error) -> Self {
		match err {
			Error::Refused(refusal) => match refusal.reason {
				moq_auth::refusal::Reason::Expired => Self::Expired,
				moq_auth::refusal::Reason::Malformed => Self::MalformedAuthToken,
				_ => Self::Unauthorized,
			},
			Error::Forbidden(_) => Self::Unauthorized,
			// The status is what separates "your credential is bad" from "the auth server is
			// down". Collapsing both into Unauthorized tells a client to stop reconnecting
			// through an outage it could have waited out.
			err => Self::App(http::StatusCode::from(err).as_u16()),
		}
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
	pub root: Path,
	/// Subtrees read from elsewhere on the origin: each path, relative to `root`,
	/// resolves at the absolute path it maps to.
	pub mounts: Vec<(Path, Path)>,
	/// Patterns the session may subscribe to, relative to `root`.
	pub subscribe: moq_auth::Patterns,
	/// Patterns the session may publish, relative to `root`.
	pub publish: moq_auth::Patterns,
	/// The tier this session's stats record under.
	pub tier: Tier,
	/// The path this session's stats record under, empty when the grant names none.
	pub label: Path,
	/// Whether the session is a cluster peer, so its routes entered elsewhere.
	pub peer: bool,
	/// Whether the peer is upstream, so it is never offered another upstream's routes.
	pub upstream: bool,
}

impl Token {
	/// Reduce `grant` for a session that dialed `path`.
	///
	/// The token keeps the patterns the grant yields: the origin scopes by any
	/// pattern union, so `alice`, `*/chat`, and `alice/**` each mean exactly what
	/// they say.
	///
	/// The dialed path is a URL path, whose one leading `/` is syntax rather than a
	/// segment. Fails with [`Error::Request`] if the rest of it is not a valid path,
	/// and with [`Error::Forbidden`] if a root, mount, tier, or label the grant names
	/// is not: one [`Path::new`] refuses.
	pub fn new(path: &str, grant: &Grant) -> Result<Self, Error> {
		let invalid = |err: moq_net::path::Error| Error::Forbidden(err.to_string());
		let root = match &grant.root {
			Some(root) => Path::new(root).map_err(invalid)?,
			None => Path::new(path.strip_prefix('/').unwrap_or(path))
				.map_err(|err| Error::Request(format!("path {path:?}: {err}")))?,
		};
		Ok(Self {
			path: path.to_string(),
			root,
			mounts: grant
				.mounts
				.iter()
				.map(|(at, target)| Ok((Path::new(at)?, Path::new(target)?)))
				.collect::<Result<_, _>>()
				.map_err(invalid)?,
			subscribe: grant.subscribe.clone(),
			publish: grant.publish.clone(),
			tier: crate::configured_tier(grant.tier.clone()).map_err(invalid)?,
			label: Path::new(grant.label.as_deref().unwrap_or_default()).map_err(invalid)?,
			peer: grant.peer,
			upstream: grant.upstream,
		})
	}

	/// Rebuild the token from a re-checked grant, relative to the same dialed path,
	/// so a grant that drops its `root` alias resolves back to what was dialed.
	pub(crate) fn recheck(&self, grant: &Grant) -> Result<Self, Error> {
		Self::new(&self.path, grant)
	}

	/// Whether `other` still covers everything this token scopes: the same root and
	/// mounts, and every path still held.
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
	refresh: Option<refresh::Refresh>,
	source: Option<Source>,
	consumer: lease::Consumer,
	token: Token,
	/// The live session a re-check re-authorizes in place, once attached.
	session: Option<moq_net::auth::Handle>,
	/// When the grant runs out, enforced here whoever drives the lease: a fixed
	/// grant has no driver, and an auth server's may be mid-outage. Fixed on the ambient
	/// clock when the grant arrives, so re-polling [`ended`](Self::ended) never
	/// restarts the countdown.
	expires: Option<moq_clock::Instant>,
	/// The session's stats context, moved to a re-checked tier.
	stats: moq_net::stats::Session,
}

struct Source {
	auth: Auth,
	request: Request,
}

impl Lease {
	/// Hold `consumer` for a session that dialed `path`, reducing its grant to
	/// what the origin scopes by.
	///
	/// Fails if the grant's paths are too long; see [`Token::new`].
	pub fn new(path: &str, consumer: lease::Consumer) -> Result<Self, Error> {
		let grant = consumer.grant();
		Self::granted(path, &grant, consumer)
	}

	/// Hold `consumer` with its token built from `grant`, a snapshot of it. A later
	/// update still reaches [`Self::ended`] as a change, which re-checks it.
	fn granted(path: &str, grant: &Grant, consumer: lease::Consumer) -> Result<Self, Error> {
		Ok(Self {
			refresh: None,
			source: None,
			token: Token::new(path, grant)?,
			expires: grant.deadline(),
			consumer,
			session: None,
			stats: Default::default(),
		})
	}

	/// Take AUTH decisions before the protocol driver chooses its default acceptor.
	pub(crate) fn preparing(
		mut self,
		handle: moq_net::auth::Handle,
		requests: moq_net::auth::Requests,
		role: Option<moq_net::setup::Role>,
	) -> Result<Self, moq_net::Error> {
		let refresh = refresh::Refresh::new(&mut self, handle, requests, role)?;
		self.refresh = Some(refresh);
		Ok(self)
	}

	/// Re-authorize `session` in place when a re-check changes what the grant allows,
	/// narrower or wider. Without it a narrower grant ends the lease.
	pub(crate) fn authorizing(mut self, session: &moq_net::Session) -> Self {
		self.session = Some(session.auth());
		self
	}

	pub(crate) fn close_session(self, error: &moq_net::Error, bytes: Bytes) {
		let reason = match error {
			moq_net::Error::Cancel => "closed".to_string(),
			error => error.to_string(),
		};
		self.close(reason, bytes);
	}

	/// Attach the session's stats context, so a re-checked tier retags it live.
	pub fn with_stats(mut self, stats: moq_net::stats::Session) -> Self {
		self.stats = stats;
		self
	}

	/// The scope the session holds: what it was admitted under, with the patterns of
	/// the latest re-check that resized it.
	pub fn token(&self) -> &Token {
		&self.token
	}

	/// Ask the lease's producer to re-check now. A no-op on a fixed lease.
	pub fn revalidate(&self) {
		if let Some(refresh) = &self.refresh {
			refresh.revalidate();
		} else {
			self.consumer.revalidate();
		}
	}

	/// Wait for the lease to stop covering the session: the grant expired, was
	/// revoked, or was re-checked into one that no longer covers the token.
	///
	/// A grant with the same root and mounts re-authorizes an attached session for its
	/// patterns, narrower or wider, and keeps going; the origin handles the session was
	/// admitted with still bound it, so it never reaches past them. Without an attached
	/// session a narrower grant ends the lease and a wider one changes nothing.
	/// A changed root or mounts ends it, and so does a flipped `peer` or `upstream`,
	/// since the routes it already announced would be misreported as entering here or
	/// from a peer, or offered to the wrong links. A changed tier keeps the session and
	/// moves its [stats](Self::with_stats) to the new tier.
	///
	/// Once the relay answers the session's AUTH requests itself, each of
	/// these ends only that credential's grant; the lease ends when no credential remains.
	pub async fn ended(&mut self) -> lease::Reason {
		if let Some(refresh) = &mut self.refresh {
			return refresh.ended().await;
		}
		loop {
			let expire = async {
				match self.expires {
					Some(at) => moq_clock::sleep_until(at).await,
					None => std::future::pending().await,
				}
			};
			tokio::select! {
				changed = self.consumer.changed() => match changed {
					Ok(grant) => {
						if let Err(reason) = still_valid(&grant) {
							return reason;
						}
						let fresh = match self.token.recheck(&grant) {
							Ok(fresh) => fresh,
							Err(err) => return err.to_string().into(),
						};
						if fresh.root != self.token.root {
							return lease::Reason::Narrowed;
						}
						if fresh.mounts != self.token.mounts {
							return "mounts changed".into();
						}
						// Routes the session already announced were recorded as
						// entering here or from a peer; a flip would misreport them.
						if fresh.peer != self.token.peer {
							return "peer changed".into();
						}
						if fresh.upstream != self.token.upstream {
							return "upstream changed".into();
						}
						if fresh.publish != self.token.publish || fresh.subscribe != self.token.subscribe {
							match &self.session {
								Some(session) => {
									tracing::info!(
										publish = %join(&fresh.publish),
										subscribe = %join(&fresh.subscribe),
										"grant changed; re-authorizing the session",
									);
									session.authorize(&moq_net::auth::Grant {
										publish: fresh.publish.clone(),
										subscribe: fresh.subscribe.clone(),
										expires: None,
									});
									self.token.publish = fresh.publish;
									self.token.subscribe = fresh.subscribe;
								}
								None if !self.token.covered_by(&fresh) => return lease::Reason::Narrowed,
								None => {}
							}
						}
						if fresh.tier != self.token.tier || fresh.label != self.token.label {
							tracing::info!(tier = %fresh.tier, label = %fresh.label, "stats tier or label changed");
							self.stats.retag(fresh.tier.clone(), &fresh.label);
							self.token.tier = fresh.tier;
							self.token.label = fresh.label;
						}
						self.expires = grant.deadline();
					},
					Err(reason) => return reason,
				},
				() = expire => return lease::Reason::Expired,
			}
		}
	}

	/// Resolve once the lease has ended and the decider heard; see
	/// [`moq_auth::lease::Consumer::reported`]. Take it before [`close`](Self::close).
	pub(crate) fn reported(&self) -> impl Future<Output = ()> + Send + 'static {
		self.consumer.reported()
	}

	/// End the lease with the session's close classification and the totals it
	/// moved, and learn what ended the session: that, or for a gateway the
	/// decider's reason if it revoked first. Dropping the consumer reports zero bytes.
	pub fn close(mut self, reason: impl Into<lease::Reason>, bytes: Bytes) -> lease::Reason {
		use futures::FutureExt;

		let mut reason = reason.into();
		// A gateway has one credential, so its revocation caused the transfer to
		// end. A refreshed session instead reports the result of its whole union,
		// which may have ended before the supervisor saw it: its first credential
		// may have ended with a reason a replacement outlived.
		match &mut self.refresh {
			None => {
				if let std::task::Poll::Ready(ended) = self.consumer.poll_closed(&moq_net::kio::Waiter::noop()) {
					reason = ended;
				}
				self.consumer.close(reason, bytes)
			}
			Some(refresh) => {
				// The session has closed by now, withdrawing every grant it was issued.
				// That is the effect of `reason`, not a verdict that overrides it.
				if let Some(ended) = refresh.ended().now_or_never()
					&& ended != lease::Reason::Dropped
				{
					reason = ended;
				}
				self.consumer.close(reason.clone(), bytes);
				reason
			}
		}
	}
}

/// Patterns as one comma-separated line, for logs.
fn join(patterns: &Patterns) -> String {
	patterns
		.iter()
		.map(|pattern| pattern.as_str())
		.collect::<Vec<_>>()
		.join(",")
}

/// The session failure a lease end exposes to a client, which is also how the session's
/// stats count it: `expired` for an expired grant, a clean close for a shutdown, and
/// `application` for any other reason.
pub(crate) fn error(reason: &lease::Reason) -> moq_net::Error {
	match reason {
		lease::Reason::Expired => moq_net::Error::Expired,
		lease::Reason::Shutdown => moq_net::Error::GoingAway,
		_ => moq_net::Error::Unauthorized,
	}
}

/// Run gateway work while its admission lease still covers the session.
///
/// Work is dropped when the lease expires, narrows, or is revoked. A completed
/// work future ends the lease with zero byte totals. A gateway that tracks
/// transport totals can use [`Lease::ended`] and [`Lease::close`] directly.
#[cfg(test)]
pub(crate) async fn hold<T>(mut lease: Lease, work: impl std::future::Future<Output = T>) -> Result<T, lease::Reason> {
	tokio::select! {
		biased;
		reason = lease.ended() => {
			lease.close(reason.clone(), Bytes::default());
			Err(reason)
		},
		result = work => {
			lease.close("done", Bytes::default());
			Ok(result)
		},
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
			while let Some(mut admission) = admissions.next().await {
				match &self {
					Self::Server(client) => {
						let client = client.clone();
						// A panic drops the admission, which answers it as unavailable.
						tokio::spawn(moq_tokio::panic::isolate(async move {
							let ask = async {
								match &admission.connection {
									Some(connection) => client.attach(connection, admission.request.clone()).await,
									None => client.connect(admission.request.clone()).await,
								}
							};
							// The relay stopped waiting (its handshake deadline passed): drop the
							// request to the server, so an expired admission leaves nothing in flight.
							let lease = tokio::select! {
								biased;
								() = admission.reply.closed() => return,
								lease = ask => lease,
							};
							match lease {
								Ok(lease) => admission.grant(lease),
								Err(err) => admission.refuse(err.into()),
							}
						}));
					}
					// Nothing here can verify a token, so one is refused rather than ignored:
					// its holder expects it to count, and the public grant is not what it says.
					Self::Public(_) if presents_token(&admission.request) => {
						tracing::debug!(path = %admission.request.path, "a token was presented to public rules");
						admission.refuse(Error::Refused(Default::default()));
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
							admission.refuse(Error::Refused(Default::default()));
						}
					},
					Self::Refuse => admission.refuse(Error::Refused(Default::default())),
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
	/// Whether the decider can verify a token presented in band; public rules cannot.
	in_band: bool,
	/// How many in-band tokens one session may have the decider verifying at once.
	#[cfg(test)]
	verifications: usize,
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
			in_band: true,
			#[cfg(test)]
			verifications: VERIFICATIONS,
		};
		(auth, Admissions(receiver))
	}

	#[cfg(test)]
	pub(crate) fn with_verifications(mut self, verifications: usize) -> Self {
		assert!(verifications > 0);
		self.verifications = verifications;
		self
	}

	/// How many credentials one session, or one HTTP/2 connection, may have the decider verifying at once.
	pub(crate) fn verifications(&self) -> usize {
		#[cfg(test)]
		return self.verifications;
		#[cfg(not(test))]
		VERIFICATIONS
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
	///
	/// Waits on the decider for as long as the caller does. The caller owns the
	/// deadline: a listener bounds it with the connection's handshake deadline
	/// ([`moq_tokio::server::Request::within`]), and dropping the future withdraws
	/// the admission from the decider.
	pub async fn admit(&self, request: Request) -> Result<Lease, Error> {
		// Build the token from the grant the admission checked: re-reading it could
		// pick up an update that skipped the check.
		let (consumer, grant) = self.decide(request.clone()).await?;
		let mut lease = Lease::granted(&request.path, &grant, consumer)?;
		lease.source = Some(Source {
			auth: self.clone(),
			request,
		});
		Ok(lease)
	}

	async fn decide(&self, request: Request) -> Result<(lease::Consumer, Grant), Error> {
		let answer = self.submit(request, None)?;
		answered_with(answer.await)
	}

	/// Queue one admission for the decider, returning its pending answer.
	fn submit(&self, request: Request, connection: Option<lease::Consumer>) -> Result<Answer, Error> {
		let (reply, answer) = oneshot::channel();
		self.admissions
			.send(Admission {
				request,
				connection,
				reply,
			})
			.map_err(|_| Error::Unavailable("nobody is answering admissions".into()))?;
		Ok(answer)
	}

	/// Admit a session on a grant decided locally, bypassing the server: the LAN
	/// mesh credential, which the relay minted for itself.
	pub(crate) fn admit_fixed(&self, path: &str, grant: Grant) -> Lease {
		Lease::new(path, lease::Consumer::fixed(grant)).expect("the relay's own grant fits")
	}
}

/// A decider's pending answer to one [`Admission`].
type Answer = oneshot::Receiver<Result<lease::Consumer, Error>>;

fn timed_out() -> Error {
	Error::Unavailable("the admission timed out".into())
}

/// Check a decider's answer against the contract every decider is held to: a
/// grant that admits nothing or asks for a re-check without a bound is a bug, not
/// a refusal. Returns the grant snapshot it checked.
fn answered_with(
	answered: Result<Result<lease::Consumer, Error>, oneshot::error::RecvError>,
) -> Result<(lease::Consumer, Grant), Error> {
	let consumer = answered.map_err(|_| Error::Unavailable("the admission went unanswered".into()))??;
	let grant = consumer.grant();
	grant.validate()?;
	Ok((consumer, grant))
}

/// Hold a changed grant to the contract its first answer met: one that admits nothing
/// narrowed the session away, and one already past its expiry ended it.
fn still_valid(grant: &Grant) -> Result<(), lease::Reason> {
	grant.validate().map_err(|err| match err {
		moq_auth::Error::UselessGrant => lease::Reason::Narrowed,
		moq_auth::Error::GrantExpired => lease::Reason::Expired,
		err => err.to_string().into(),
	})
}

/// The sessions an embedded [`Auth`] is waiting to admit, in arrival order.
///
/// The transport has already accepted each one; it waits for its answer, so a
/// slow decider holds connects the way a slow auth server would. A connection's
/// admission has no bound of its own: whoever awaits [`Auth::admit`] owns the
/// deadline, which the relay's listeners set to their handshake deadline. An
/// [attached](Admission::attached) token is refused as unavailable after ten
/// seconds, but holds one of its session's verification slots until the decider
/// answers or drops it, even past that, and the session's later tokens wait for a
/// slot before they reach the decider.
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
	connection: Option<lease::Consumer>,
	reply: oneshot::Sender<Result<lease::Consumer, Error>>,
}

#[cfg(test)]
impl Admission {
	/// Resolve once the relay stops waiting for this decision.
	pub(crate) async fn abandoned(&mut self) {
		self.reply.closed().await
	}
}

impl Admission {
	/// Whether this verifies another credential presented in band for the session
	/// already admitted as `request.id`, rather than admitting a new session.
	pub fn attached(&self) -> bool {
		self.connection.is_some()
	}

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
	let transport = transport(request.transport());
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
	// Only the SNI: a WebTransport CONNECT authority or WebSocket `Host` is client-chosen
	// without having selected a certificate, so a session without SNI reports none.
	out.server_name = request.server_name().map(str::to_owned);
	out.alpn = request.alpn().map(str::to_owned);
	out.role = request.role().and_then(role);
	out.implementation = request.peer_implementation().map(str::to_owned);
	out.tls = request.peer_identity().as_ref().and_then(peer);
	out
}

/// Build an auth request from the path, credential, role, and implementation carried in SETUP.
#[cfg(any(feature = "test-support", all(target_os = "linux", feature = "_uring")))]
pub(crate) fn request_for_setup(
	auth: &Auth,
	setup: &moq_net::server::Handshake,
	transport: moq_auth::Transport,
) -> Request {
	let (path, query) = match setup.path().split_once('?') {
		Some((path, query)) => (path, Some(query.to_string())),
		None => (setup.path(), None),
	};
	let mut request = auth.request(transport, if path.is_empty() { "/" } else { path });
	request.query = query;
	request.token = setup.token().map(|token| moq_auth::Token {
		kind: token.kind,
		value: token.value.clone(),
	});
	request.role = setup.role().and_then(role);
	request.implementation = setup.peer_implementation().map(str::to_owned);
	request
}

/// The auth transport for a session's transport.
fn transport(transport: moq_tokio::Transport) -> moq_auth::Transport {
	match transport {
		moq_tokio::Transport::Quic => moq_auth::Transport::Quic,
		moq_tokio::Transport::WebTransport => moq_auth::Transport::WebTransport,
		moq_tokio::Transport::Iroh => moq_auth::Transport::Iroh,
		moq_tokio::Transport::WebSocket => moq_auth::Transport::WebSocket,
		moq_tokio::Transport::Tcp => moq_auth::Transport::Tcp,
		moq_tokio::Transport::Unix => moq_auth::Transport::Unix,
		moq_tokio::Transport::WebRtc => moq_auth::Transport::WebRtc,
		// A transport this build does not know is still a session on the wire; the
		// server sees the same facts either way.
		other => unreachable!("unknown transport {other}"),
	}
}

/// The auth role for a session role, `None` for one `moq-auth` cannot name yet.
pub(crate) fn role(role: moq_net::setup::Role) -> Option<moq_auth::Role> {
	match role {
		moq_net::setup::Role::Publisher => Some(moq_auth::Role::Publisher),
		moq_net::setup::Role::Subscriber => Some(moq_auth::Role::Subscriber),
		_ => None,
	}
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
#[expect(clippy::disallowed_methods, reason = "tests read the host clock directly")]
mod tests {
	use super::*;

	fn patterns(texts: &[&str]) -> moq_auth::Patterns {
		texts.iter().map(|text| text.parse().unwrap()).collect()
	}

	fn config(url: Option<&str>, public: &[&str]) -> Config {
		Config {
			url: url.map(|url| url.parse().unwrap()),
			public: public.iter().map(|p| p.parse().unwrap()).collect(),
			..Default::default()
		}
	}

	#[tokio::test(start_paused = true)]
	async fn an_invalid_label_at_revalidation_ends_the_lease() {
		let grant = Grant::new(moq_net::Pattern::all(), moq_net::Pattern::all());
		let (producer, consumer) = lease::Producer::new(grant.clone());
		let mut lease = Lease::new("/room", consumer).unwrap();
		let mut wildcard = grant;
		wildcard.label = Some("project/*".into());
		producer.update(wildcard);
		assert!(lease.ended().await.to_string().contains("invalid stats label"));
		assert!(lease.token().label.is_empty(), "the label never applied");
	}

	#[test]
	fn typed_refusal_preserves_transport_status() {
		use moq_auth::refusal::Reason;
		use moq_net::SessionError;
		for (reason, status, code) in [
			(
				Reason::Refused,
				http::StatusCode::UNAUTHORIZED,
				SessionError::Unauthorized,
			),
			(Reason::Expired, http::StatusCode::UNAUTHORIZED, SessionError::Expired),
			(
				Reason::Forbidden,
				http::StatusCode::FORBIDDEN,
				SessionError::Unauthorized,
			),
			(
				Reason::Malformed,
				http::StatusCode::UNAUTHORIZED,
				SessionError::MalformedAuthToken,
			),
		] {
			let mut refusal = moq_auth::Refusal::default();
			refusal.reason = reason;
			let error = Error::from(moq_auth::Error::Refused(refusal));
			assert_eq!(http::StatusCode::from(&error), status);
			assert_eq!(SessionError::from(&error), code);
		}
		let outage = Error::Unavailable("down".into());
		assert_eq!(SessionError::from(&outage), SessionError::App(502));
	}

	#[test]
	fn every_transport_has_an_auth_name() {
		use moq_tokio::Transport as T;
		for transport in [
			T::Quic,
			T::WebTransport,
			T::Iroh,
			T::WebSocket,
			T::Tcp,
			T::Unix,
			T::WebRtc,
		] {
			assert_eq!(super::transport(transport).as_str(), transport.as_str());
		}
	}

	#[test]
	fn exactly_one_source() {
		assert!(config(None, &[]).validate(false).is_err());
		assert!(config(Some("http://127.0.0.1:4440/"), &["**"]).validate(false).is_err());
		assert!(config(Some("http://127.0.0.1:4440/"), &[]).validate(false).is_ok());
		assert!(config(None, &["anon/**"]).validate(false).is_ok());

		let split = Config {
			public_subscribe: patterns(&["anon/**"]).into_iter().collect(),
			..Default::default()
		};
		assert!(split.validate(false).is_ok());
		let grant = split.public_grant().unwrap();
		assert_eq!(grant.subscribe, patterns(&["anon/**"]));
		assert!(grant.publish.is_empty());
	}

	/// Removed explicit window values, including zero, are refused by name.
	#[test]
	fn configurable_verifications_are_refused() {
		for value in [0, 4] {
			let parsed: Config =
				toml::from_str(&format!("url = \"http://127.0.0.1:4440/\"\nverifications = {value}\n")).unwrap();
			let error = parsed.validate(false).unwrap_err();
			assert!(error.to_string().contains("verifications"), "{error}");
		}
	}

	/// Public rules grant a certificate what they grant anyone, so a client CA
	/// needs an auth server, and `init` refuses it like `validate` does.
	#[test]
	fn a_client_ca_needs_an_auth_server() {
		let public = config(None, &["anon/**"]);
		let err = public.validate(true).unwrap_err().to_string();
		assert!(err.contains("--auth-public ignores"), "{err}");
		let Err(err) = public.init("relay-1", &moq_tokio::tls::Connect::default(), true) else {
			panic!("init accepted a client CA under public rules");
		};
		assert!(err.to_string().contains("--auth-public ignores"), "{err}");
		assert!(config(Some("http://127.0.0.1:4440/"), &[]).validate(true).is_ok());
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
		.init("relay-1", &moq_tokio::tls::Connect::default(), false)
		.unwrap();

		for (path, root, publish, subscribe) in [
			("/", "", &["anon/**"][..], &["anon/**", "*/chat"][..]),
			("/anon", "anon", &["**"], &["**", "chat"]),
			("/anon/room", "anon/room", &["**"], &["**"]),
			("/rooms", "rooms", &[], &["chat"]),
		] {
			let lease = auth.admit(auth.request(moq_auth::Transport::Quic, path)).await.unwrap();
			assert_eq!(lease.token().root, Path::new(root).unwrap(), "{path}");
			assert_eq!(lease.token().publish, patterns(publish), "{path}");
			assert_eq!(lease.token().subscribe, patterns(subscribe), "{path}");
		}

		// A path the rules don't reach is refused, never an outage the client retries.
		for path in ["/rooms/123", "/other/room", "/anonymous/room"] {
			let Err(err) = auth.admit(auth.request(moq_auth::Transport::Quic, path)).await else {
				panic!("{path} was admitted");
			};
			assert!(matches!(err, Error::Refused(_)), "{path}: {err}");
			assert_eq!(http::StatusCode::from(&err), http::StatusCode::UNAUTHORIZED);
		}
	}

	/// Public rules verify nothing, so a token is refused rather than ignored, in
	/// whichever form it arrives.
	#[tokio::test]
	async fn public_rules_refuse_a_token() {
		let auth = config(None, &["**"])
			.init("relay-1", &moq_tokio::tls::Connect::default(), false)
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
			assert!(matches!(err, Error::Refused(_)), "{err}");
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
			.init("relay-1", &moq_tokio::tls::Connect::default(), false)
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
			assert_eq!(lease.token().root, Path::from_static("anon/room"));
			assert_eq!(lease.token().subscribe, patterns(&["**"]));
			assert_eq!(lease.token().tier, Tier::default());
		}
	}

	/// 0.14 read `anon` as a prefix and a pattern reads it as one broadcast, so a
	/// wildcard-free public pattern refuses to start rather than pick silently.
	#[test]
	fn a_public_pattern_without_a_wildcard_refuses_to_start() {
		for (public, hint) in [("anon", "anon/**"), ("anon/room", "anon/room/**"), ("", "**")] {
			let err = config(None, &[public]).validate(false).unwrap_err().to_string();
			assert!(err.contains(&format!("`{hint}`")), "{public}: {err}");
		}
		let split = Config {
			public_subscribe: patterns(&["anon/**", "live"]).into_iter().collect(),
			..Default::default()
		};
		let err = split.validate(false).unwrap_err().to_string();
		assert!(err.starts_with("--auth-public-subscribe `live`"), "{err}");
		assert!(config(None, &["anon/*", "*/chat", "**"]).validate(false).is_ok());

		// Nothing sits beneath a literal at the maximum depth, so it is its own subtree.
		let deepest = vec!["a"; Pattern::MAX_SEGMENTS].join("/");
		assert!(config(None, &[&deepest]).validate(false).is_ok());
	}

	#[test]
	fn config_init_requires_a_runtime() {
		let result = std::panic::catch_unwind(|| {
			let _ = config(None, &["anon/**"])
				.init("relay-1", &moq_tokio::tls::Connect::default(), false)
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
			admission.refuse(Error::Refused(Default::default()));
			// A grant that admits nothing is the decider's mistake, refused like a server's.
			let admission = admissions.next().await.expect("an admission");
			admission.grant(lease::Consumer::fixed(Grant::new(Patterns::new(), Patterns::new())));
			// Dropped without an answer.
			drop(admissions.next().await.expect("an admission"));
			admissions
		};
		let admit = async {
			let lease = auth.admit(request()).await.expect("granted");
			assert_eq!(lease.token().root, Path::from_static("anon/room"));
			assert!(matches!(auth.admit(request()).await, Err(Error::Refused(_))));
			assert!(matches!(auth.admit(request()).await, Err(Error::Refused(_))));
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
		assert!(matches!(auth.admit(request).await, Err(Error::Refused(_))));
	}

	/// The session supervisor re-polls `ended` on every nudge; that must not
	/// restart the countdown to `expires`.
	#[moq_clock::test]
	async fn re_polling_keeps_the_expiry_deadline() {
		use std::time::Duration;

		let start = moq_clock::now();
		let mut grant = Grant::new(patterns(&["**"]), patterns(&["**"]));
		grant.expires = Some(moq_clock::wall() + Duration::from_secs(10));
		let mut lease = Lease::new("/room", lease::Consumer::fixed(grant)).unwrap();
		for _ in 0..9 {
			assert!(moq_clock::timeout(Duration::from_secs(1), lease.ended()).await.is_err());
		}
		assert_eq!(lease.ended().await, lease::Reason::Expired);
		assert_eq!(
			moq_clock::now(),
			start + Duration::from_secs(10),
			"expired at the deadline despite re-polling"
		);
	}

	#[moq_clock::test]
	async fn an_expired_grant_ends_at_once() {
		use std::time::Duration;

		let mut grant = Grant::new(patterns(&["**"]), patterns(&["**"]));
		grant.expires = Some(moq_clock::wall() - Duration::from_secs(1));
		assert!(matches!(grant.validate(), Err(moq_auth::Error::GrantExpired)));

		let start = moq_clock::now();
		let mut lease = Lease::new("/room", lease::Consumer::fixed(grant)).unwrap();
		assert_eq!(lease.ended().await, lease::Reason::Expired);
		assert_eq!(moq_clock::now(), start, "no grace after expiry");
	}

	#[test]
	fn token_keeps_every_grant_pattern() {
		let mut grant = Grant::new(patterns(&["alice/**"]), patterns(&["**"]));
		grant.root = Some("pid/room".into());
		grant.tier = Some("gold".into());
		grant.label = Some("project/key/123".into());
		let token = Token::new("/vanity/room", &grant).unwrap();
		assert_eq!(token.root, Path::from_static("pid/room"));
		assert_eq!(token.publish, patterns(&["alice/**"]));
		assert_eq!(token.subscribe, patterns(&["**"]));
		assert_eq!(token.tier, Tier::new(moq_net::Path::from_static("gold")));
		assert_eq!(token.label, Path::from_static("project/key/123"));

		let patterns = patterns(&["*/chat", "alice", ""]);
		let token = Token::new("/", &Grant::new(patterns.clone(), Patterns::new())).unwrap();
		assert_eq!(token.publish, patterns);
		assert!(
			token.label.is_empty(),
			"an absent label is the empty label, not the root"
		);
	}

	#[test]
	fn token_refuses_empty_segments() {
		let all = || Grant::new(patterns(&["**"]), patterns(&["**"]));
		// The dialed URL path's one leading `/` is syntax; any other empty segment is the client's.
		assert_eq!(Token::new("/room", &all()).unwrap().root, Path::from_static("room"));
		for dialed in ["/room/", "//room", "/room//cam"] {
			assert!(matches!(Token::new(dialed, &all()), Err(Error::Request(_))), "{dialed}");
		}

		// A grant naming one is the auth server's.
		let mut grant = all();
		grant.root = Some("room/".into());
		assert!(matches!(Token::new("/room", &grant), Err(Error::Forbidden(_))));
	}

	#[test]
	fn a_narrower_recheck_is_not_covered() {
		let wide = Token::new("/room", &Grant::new(patterns(&["**"]), patterns(&["**"]))).unwrap();
		let narrow = Token::new("/room", &Grant::new(patterns(&["alice/**"]), patterns(&["**"]))).unwrap();
		let moved = Token::new("/other", &Grant::new(patterns(&["**"]), patterns(&["**"]))).unwrap();
		let mut mounted = Grant::new(patterns(&["**"]), patterns(&["**"]));
		mounted.mounts.insert(".svc".into(), ".svc/room".into());
		let mounted = Token::new("/room", &mounted).unwrap();
		assert_eq!(
			mounted.mounts,
			[(Path::from_static(".svc"), Path::from_static(".svc/room"))]
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
	async fn a_recheck_that_flips_the_link_closes() {
		let grant = Grant::new(patterns(&["**"]), patterns(&["**"]));
		let (producer, consumer) = lease::Producer::new(grant.clone());
		let mut lease = Lease::new("/", consumer).unwrap();
		assert!(!lease.token().peer);

		let mut peer = grant;
		peer.peer = true;
		producer.update(peer.clone());
		assert_eq!(lease.ended().await.to_string(), "peer changed");

		// Routes already offered to (or withheld from) the session were filtered
		// by whether it is upstream, so flipping that closes it too.
		let (producer, consumer) = lease::Producer::new(peer.clone());
		let mut lease = Lease::new("/", consumer).unwrap();
		let mut upstream = peer;
		upstream.upstream = true;
		producer.update(upstream);
		assert_eq!(lease.ended().await.to_string(), "upstream changed");
	}

	#[test]
	fn a_recheck_resolves_a_dropped_root_to_the_dialed_path() {
		let everything = || Grant::new(patterns(&["**"]), patterns(&["**"]));
		let mut aliased = everything();
		aliased.root = Some("pid/room".into());
		let token = Token::new("/vanity/room", &aliased).unwrap();
		assert_eq!(token.root, Path::from_static("pid/room"));

		// The same alias still names the same root.
		assert_eq!(token.recheck(&aliased).unwrap().root, token.root);
		// A grant without the alias is relative to what was dialed, not to the old root.
		assert_eq!(
			token.recheck(&everything()).unwrap().root,
			Path::from_static("vanity/room")
		);
	}
}

#[cfg(test)]
#[expect(clippy::disallowed_methods, reason = "tests read the host clock directly")]
mod admission_tests;
