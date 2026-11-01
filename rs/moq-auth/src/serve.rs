//! The reference auth server: the policy a relay used to hold, answering the contract.
//!
//! [`Policy`] decides a [`Request`] the way `--auth-key`, `--auth-key-dir`, and
//! `--auth-public` did on the relay, plus an explicit grant for mTLS peers and live
//! session caps. Its anonymous and mTLS rules are rooted at `/`, like a token with an
//! empty root, and authorized at the dialed path the same way. [`Server`] serves it
//! on a TCP or unix listener as `POST /`. `moq auth serve` is the CLI; the relay's
//! tests run against it in process.

use std::borrow::Cow;
use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use moq_clock::Instant;

use crate::{Event, Grant, Key, KeyId, KeySet, Permissions, Request, Token};

/// Where the signing keys a `jwt` is verified against come from. Read per request,
/// so a rotated file takes effect without a restart.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum Keys {
	/// One key file; the token's `kid` must match the key.
	File(PathBuf),
	/// A directory of `{kid}.jwk`, selected by the token's `kid`.
	Dir(PathBuf),
	/// A JWK Set file, selected by the token's `kid`.
	Set(PathBuf),
}

/// Caps on live sessions, counted from `connect` and `end` events.
///
/// A nuisance limit, not a security boundary, gating admission and never revoking:
/// a relay that dies without sending `end` holds its slots until they age out after
/// two cadences, a restart empties the table until the fleet's next cadence refills
/// it, and a session admitted while a live one's slot was missing stays over the cap.
/// The cadence is [`Policy::revalidate`]; without one, nothing ages out.
///
/// `#[non_exhaustive]`, so start from [`Limits::default`] and set the fields.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Limits {
	/// The most live sessions presenting one token; `None` is unlimited.
	pub token: Option<usize>,
	/// The most live sessions from one remote address; `None` is unlimited.
	pub remote: Option<usize>,
}

/// A verified certificate's cluster-link role.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
	/// A cluster peer that may exchange routes in either direction.
	Peer,
	/// A peer that is never offered routes learned from another upstream.
	Upstream,
}

impl std::str::FromStr for Link {
	type Err = &'static str;
	fn from_str(value: &str) -> Result<Self, Self::Err> {
		match value {
			"peer" => Ok(Self::Peer),
			"upstream" => Ok(Self::Upstream),
			_ => Err("expected peer or upstream"),
		}
	}
}

impl std::fmt::Display for Link {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(match self {
			Self::Peer => "peer",
			Self::Upstream => "upstream",
		})
	}
}

/// The decisions the server answers with, evaluated in order and stopping at the
/// first that applies: a JWT, then a verified certificate, then the anonymous rules.
/// A malformed or expired token is a refusal, never a fall through.
///
/// The anonymous and mTLS rules name what they grant from `/`, like a token with an
/// empty root: `anon/**` admits a session dialed at `/`, `/anon`, or `/anon/room`,
/// each scoped to `anon/`, and refuses one dialed at `/other`.
///
/// The JWT is the `jwt` query parameter or a moq-transport SETUP token of type 0
/// ([`Token::OUT_OF_BAND`]), verified alike. A session presenting both is verified
/// once when they are the same JWT and refused when they differ, as is a SETUP token
/// of any other type, and one presenting a JWT alongside a
/// certificate: neither can safely win, since a certificate would override a JWT
/// meant to narrow it, and a JWT would narrow or refuse a peer by accident.
///
/// `#[non_exhaustive]`, so start from [`Policy::default`] and set the fields.
#[derive(Clone, Debug)]
#[non_exhaustive]
#[derive(Default)]
pub struct Policy {
	/// The keys a `jwt` is verified against; `None` refuses every token.
	pub keys: Option<Keys>,
	/// What an anonymous session is granted, rooted at `/`; empty refuses it.
	pub public: Permissions,
	/// What a session presenting a verified certificate is granted, rooted at `/`;
	/// empty refuses it.
	pub mtls: Permissions,
	/// The cluster-link role granted only to sessions presenting a verified certificate.
	pub mtls_link: Option<Link>,

	/// The tier stamped on every grant.
	pub tier: Option<String>,
	/// How often the relay re-checks each grant; `None` never re-checks. The contract
	/// refuses a cadence without a bound, so set [`expires`](Self::expires) with it.
	pub revalidate: Option<Duration>,
	/// How long a grant with no bound of its own lasts: an anonymous session, a token
	/// without `exp`, a certificate without one. `None` leaves those unbounded.
	/// A bound past the system clock's range is refused by [`Server::new`], and by
	/// [`decide`](Self::decide) for each session it would bound.
	pub expires: Option<Duration>,
	/// Live session caps.
	pub limits: Limits,
}

/// Why the policy denied a session, mapped to the typed [`Refusal`](crate::Refusal) in the 403 body.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Denial {
	/// No keys are configured, so no token can be verified.
	#[error("no keys are configured, so a token cannot be verified")]
	NoKeys,
	/// The token has no `kid`, and the configured keys need one to choose a key.
	#[error("the token names no key and the configured keys need one")]
	MissingKeyId,
	/// The token's key could not be found or loaded: its `kid` is malformed or matches no key, or the key source is unreadable.
	#[error("the token's key is unknown")]
	UnknownKey,
	/// The token does not parse as a JWT.
	#[error("the token is not a JWT: {0}")]
	MalformedToken(String),
	/// The token failed to verify or authorize.
	#[error("the token is invalid: {0}")]
	InvalidToken(String),
	/// The token's root does not overlap the dialed path.
	#[error("the token root {root:?} does not overlap the dialed path {path:?}")]
	RootMismatch {
		/// The token's root.
		root: String,
		/// The dialed path.
		path: String,
	},
	/// Nothing is granted at the dialed path.
	#[error("nothing is granted at {path:?}")]
	NoAccess {
		/// The dialed path.
		path: String,
		/// The verified token's root, absent without a token.
		root: Option<String>,
	},
	/// A client certificate was presented, but the policy grants nothing to certificates.
	#[error("a certificate was presented but nothing is granted to certificates")]
	NoMtlsGrant,
	/// Neither a token nor a certificate was presented, and anonymous access is not granted.
	#[error("anonymous access is not granted")]
	NoPublicGrant,
	/// The token already holds its limit of live sessions.
	#[error("too many live sessions for this token")]
	TokenLimit {
		/// The verified token's root, absent without a token.
		root: Option<String>,
	},
	/// The remote address already holds its limit of live sessions.
	#[error("too many live sessions from this address")]
	RemoteLimit {
		/// The verified token's root, absent without a token.
		root: Option<String>,
	},
	/// A SETUP token and a different `jwt` query were both presented.
	#[error("a SETUP token and a different `jwt` query were presented; present one")]
	TwoTokens,
	/// The query carries more than one `jwt` parameter.
	#[error("the query carries more than one `jwt`; present one")]
	DuplicateJwt,
	/// The SETUP token type is not 0, a JWT.
	#[error("SETUP token type {0:#x} is not supported; only type 0 (a JWT) is")]
	UnsupportedToken(u64),
	/// Both a JWT and a client certificate were presented.
	#[error("both a JWT and a client certificate were presented; present one")]
	TokenAndCertificate,
	/// The policy's grant bound is beyond what the system clock can represent.
	#[error("the policy's grant bound reaches past the system clock's range")]
	ExpiresOutOfRange,
	/// The token's expiry has passed.
	#[error("the token has expired")]
	Expired {
		/// The token's root.
		root: String,
	},
}

impl Denial {
	/// The shared refusal contract, with attribution only from a verified token.
	fn body(&self, tier: Option<String>) -> crate::Refusal {
		use crate::refusal::Reason;
		let reason = match self {
			Self::Expired { .. } => Reason::Expired,
			Self::RootMismatch { .. } | Self::NoAccess { .. } => Reason::Forbidden,
			Self::MalformedToken(_) => Reason::Malformed,
			_ => Reason::Refused,
		};
		let root = match self {
			Self::Expired { root } | Self::RootMismatch { root, .. } => Some(root.clone()),
			Self::NoAccess { root, .. } | Self::TokenLimit { root } | Self::RemoteLimit { root } => root.clone(),
			_ => None,
		};
		crate::Refusal { reason, root, tier }
	}
}

impl Policy {
	/// Decide `request` by the policy alone, ignoring session limits.
	pub async fn decide(&self, request: &Request) -> Result<Grant, Denial> {
		Ok(self.decide_root(request).await?.0)
	}

	async fn decide_root(&self, request: &Request) -> Result<(Grant, Option<String>), Denial> {
		let jwt = jwt(request)?;
		if jwt.is_some() && request.tls.is_some() {
			return Err(Denial::TokenAndCertificate);
		}
		let (permissions, expires, certificate, root) = if let Some(jwt) = jwt {
			let claims = self.verify(&jwt).await?;
			let permissions = claims.authorize(&request.path).map_err(|err| match err {
				crate::Error::RootMismatch(path) => Denial::RootMismatch {
					root: claims.root.clone(),
					path,
				},
				crate::Error::NoAccess(path) => Denial::NoAccess {
					path,
					root: Some(claims.root.clone()),
				},
				other => Denial::InvalidToken(other.to_string()),
			})?;
			(permissions, claims.expires, false, Some(claims.root))
		} else if let Some(peer) = &request.tls {
			if self.mtls.is_empty() {
				return Err(Denial::NoMtlsGrant);
			}
			(authorize(&self.mtls, &request.path)?, peer.expires, true, None)
		} else {
			if self.public.is_empty() {
				return Err(Denial::NoPublicGrant);
			}
			(authorize(&self.public, &request.path)?, None, false, None)
		};

		let mut grant = Grant::new(permissions.publish, permissions.subscribe);
		grant.expires = match (expires, self.expires) {
			(Some(expires), _) => Some(expires),
			(None, Some(bound)) => Some(moq_clock::wall().checked_add(bound).ok_or(Denial::ExpiresOutOfRange)?),
			(None, None) => None,
		};
		grant.revalidate = self.revalidate;
		grant.tier = self.tier.clone();
		grant.peer = certificate && self.mtls_link.is_some();
		grant.upstream = certificate && self.mtls_link == Some(Link::Upstream);
		Ok((grant, root))
	}

	async fn verify(&self, jwt: &str) -> Result<crate::Claims, Denial> {
		let keys = self.keys.as_ref().ok_or(Denial::NoKeys)?;
		// Every key source reads the header first, so a token that is not a JWT fails here
		// whichever one is configured.
		let header = jsonwebtoken::decode_header(jwt).map_err(|err| Denial::MalformedToken(err.to_string()))?;
		let claims = match keys {
			Keys::File(path) => Key::from_file_async(path)
				.await
				.map_err(|_| Denial::UnknownKey)?
				.verify_claims(jwt),
			Keys::Dir(dir) => {
				let kid = header.kid.ok_or(Denial::MissingKeyId)?;
				let kid = KeyId::decode(&kid).map_err(|_| Denial::UnknownKey)?;
				Key::from_file_async(dir.join(format!("{kid}.jwk")))
					.await
					.map_err(|_| Denial::UnknownKey)?
					.verify_claims(jwt)
			}
			Keys::Set(path) => {
				let json = tokio::fs::read_to_string(path).await.map_err(|_| Denial::UnknownKey)?;
				json.parse::<KeySet>()
					.map_err(|err| Denial::InvalidToken(err.to_string()))?
					.key(jwt)
					.and_then(|key| key.verify_claims(jwt))
			}
		};
		let claims = claims.map_err(|err| match err {
			crate::Error::Key(crate::KeyError::MissingKid) => Denial::MissingKeyId,
			crate::Error::Key(crate::KeyError::KeyNotFound(_)) => Denial::UnknownKey,
			other => Denial::InvalidToken(other.to_string()),
		})?;
		crate::key::validate_times(&claims, moq_clock::wall()).map_err(|err| match err {
			crate::Error::TokenExpired => Denial::Expired {
				root: claims.root.clone(),
			},
			other => Denial::InvalidToken(other.to_string()),
		})?;
		Ok(claims)
	}
}

/// Authorize rules rooted at `/` at the dialed `path`, exactly as a token with an
/// empty root would be.
fn authorize(rules: &Permissions, path: &str) -> Result<Permissions, Denial> {
	let claims = crate::Claims {
		publish: rules.publish.clone(),
		subscribe: rules.subscribe.clone(),
		..Default::default()
	};
	// An empty root overlaps every path, so the only refusal is reaching nothing.
	claims.authorize(path).map_err(|_| Denial::NoAccess {
		path: crate::path::normalize(path),
		root: None,
	})
}

/// The JWT the request presents: its SETUP token, or else its `jwt` query parameter.
/// Both at once count as one only when they are the same JWT.
fn jwt(request: &Request) -> Result<Option<Cow<'_, str>>, Denial> {
	match (&request.token, query_jwt(request)?) {
		// A client that offers a version without in-band auth copies its SETUP token
		// into the URL too, so the same JWT arrives twice.
		(Some(token), Some(jwt)) if token.kind == Token::OUT_OF_BAND && token.value == jwt.as_bytes() => Ok(Some(jwt)),
		(Some(_), Some(_)) => Err(Denial::TwoTokens),
		(Some(token), None) if token.kind == Token::OUT_OF_BAND => std::str::from_utf8(&token.value)
			.map(|jwt| Some(jwt.into()))
			.map_err(|_| Denial::MalformedToken("the SETUP token is not UTF-8".into())),
		(Some(token), None) => Err(Denial::UnsupportedToken(token.kind)),
		(None, jwt) => Ok(jwt),
	}
}

/// The `jwt` query parameter, percent-decoded, when the request carries a non-empty
/// one. A second `jwt` key, even an empty or escaped one, is refused rather than
/// picking a winner. Decoding is a no-op on a well-formed JWT, which is base64url and dots.
fn query_jwt(request: &Request) -> Result<Option<Cow<'_, str>>, Denial> {
	let mut jwt = None;
	for (key, value) in url::form_urlencoded::parse(request.query.as_deref().unwrap_or_default().as_bytes()) {
		if key == "jwt" && jwt.replace(value).is_some() {
			return Err(Denial::DuplicateJwt);
		}
	}
	Ok(jwt.filter(|jwt| !jwt.is_empty()))
}

/// The remote address without its port, an IPv4-mapped IPv6 address folded to IPv4.
fn remote(request: &Request) -> Option<IpAddr> {
	request.remote.map(|addr| addr.ip().to_canonical())
}

/// One live session's share of the limits.
struct Slot {
	/// Every JWT presented for this session: its connect, plus each one attached in band.
	tokens: Vec<u64>,
	remote: Option<IpAddr>,
	seen: Instant,
}

/// The live session table the limits count over.
#[derive(Default)]
struct Sessions {
	slots: HashMap<String, Slot>,
}

impl Sessions {
	/// Drop sessions that missed two cadences: their relay died without an `end`.
	fn sweep(&mut self, cadence: Duration) {
		let now = Instant::now();
		self.slots
			.retain(|_, slot| now.saturating_duration_since(slot.seen) < cadence.saturating_mul(2));
	}

	/// Admit a `connect`, refusing over the cap. A known id is the same session
	/// attaching another credential: it holds no new remote slot, but a JWT new to
	/// it counts against that JWT's cap until the session ends.
	fn connect(&mut self, request: &Request, limits: Limits, root: Option<String>) -> Result<(), Denial> {
		// Decided before counting, so the credential is already known to be one JWT.
		let token = jwt(request).ok().flatten().as_deref().map(hash);
		let known = self.slots.get(&request.id);
		let new_token = token.filter(|token| !known.is_some_and(|slot| slot.tokens.contains(token)));
		if let (Some(cap), Some(token)) = (limits.token, new_token)
			&& self.slots.values().filter(|slot| slot.tokens.contains(&token)).count() >= cap
		{
			return Err(Denial::TokenLimit { root });
		}
		if let Some(slot) = self.slots.get_mut(&request.id) {
			slot.tokens.extend(new_token);
			slot.seen = Instant::now();
			return Ok(());
		}
		let remote = remote(request);
		if let (Some(cap), Some(remote)) = (limits.remote, remote)
			&& self.slots.values().filter(|slot| slot.remote == Some(remote)).count() >= cap
		{
			return Err(Denial::RemoteLimit { root });
		}
		self.slots.insert(
			request.id.clone(),
			Slot {
				tokens: token.into_iter().collect(),
				remote,
				seen: Instant::now(),
			},
		);
		Ok(())
	}

	/// A `revalidate` keeps the slot alive, re-registering one that aged out or was
	/// lost to a restart. The cap is not enforced here: the session was admitted, and
	/// which survivor to revoke would be an accident of arrival order.
	fn revalidate(&mut self, request: &Request) {
		let token = jwt(request).ok().flatten().as_deref().map(hash);
		let slot = self.slots.entry(request.id.clone()).or_insert_with(|| Slot {
			tokens: Vec::new(),
			remote: remote(request),
			seen: Instant::now(),
		});
		if let Some(token) = token
			&& !slot.tokens.contains(&token)
		{
			slot.tokens.push(token);
		}
		slot.seen = Instant::now();
	}

	fn end(&mut self, id: &str) {
		self.slots.remove(id);
	}
}

/// A token's identity in the table, without holding the credential itself.
fn hash(token: &str) -> u64 {
	use std::hash::{Hash, Hasher};
	let mut hasher = std::hash::DefaultHasher::new();
	token.hash(&mut hasher);
	hasher.finish()
}

/// The policy behind `POST /`, with the session table the limits need.
#[derive(Clone)]
pub struct Server {
	policy: Arc<Policy>,
	sessions: Arc<Mutex<Sessions>>,
}

impl Server {
	/// A server answering with `policy`, refusing one that asks for a re-check without
	/// a bound, bounds grants past the clock's range, caps sessions without the
	/// re-check that ages out a dead relay's, or stamps an invalid tier.
	pub fn new(policy: Policy) -> crate::Result<Self> {
		if let Some(tier) = &policy.tier {
			crate::tier::validate(tier)?;
		}
		if policy.revalidate.is_some() && policy.expires.is_none() {
			return Err(crate::Error::UnboundedRevalidate);
		}
		if policy
			.expires
			.is_some_and(|bound| moq_clock::wall().checked_add(bound).is_none())
		{
			return Err(crate::Error::ExpiresOutOfRange);
		}
		let limited = policy.limits.token.is_some() || policy.limits.remote.is_some();
		if limited && policy.revalidate.is_none() {
			return Err(crate::Error::LimitsWithoutRevalidate);
		}
		Ok(Self {
			policy: Arc::new(policy),
			sessions: Default::default(),
		})
	}

	/// The cadence the session table ages out by, or `None` when no limit needs a table.
	fn cadence(&self) -> Option<Duration> {
		let limits = self.policy.limits;
		(limits.token.is_some() || limits.remote.is_some())
			.then_some(self.policy.revalidate)
			.flatten()
	}

	/// Answer one event: the grant, or why the session is refused.
	pub async fn answer(&self, request: &Request) -> Result<Option<Grant>, Denial> {
		match request.event {
			Event::Connect => {
				let (grant, root) = self.policy.decide_root(request).await?;
				if let Some(cadence) = self.cadence() {
					let mut sessions = self.sessions.lock().unwrap();
					sessions.sweep(cadence);
					sessions.connect(request, self.policy.limits, root)?;
				}
				Ok(Some(grant))
			}
			Event::Revalidate => {
				let grant = self.policy.decide(request).await?;
				if let Some(cadence) = self.cadence() {
					let mut sessions = self.sessions.lock().unwrap();
					sessions.sweep(cadence);
					sessions.revalidate(request);
				}
				Ok(Some(grant))
			}
			Event::End { .. } => {
				self.sessions.lock().unwrap().end(&request.id);
				Ok(None)
			}
		}
	}

	/// The router: `POST /` reading a [`Request`] and writing a [`Grant`].
	pub fn router(&self) -> Router {
		Router::new().route("/", post(handle)).with_state(self.clone())
	}

	/// Serve on a TCP listener until the task is dropped.
	pub async fn serve(&self, listener: tokio::net::TcpListener) -> std::io::Result<()> {
		axum::serve(listener, self.router()).await
	}

	/// Serve on a unix listener until the task is dropped.
	#[cfg(unix)]
	pub async fn serve_unix(&self, listener: tokio::net::UnixListener) -> std::io::Result<()> {
		axum::serve(listener, self.router()).await
	}
}

async fn handle(State(server): State<Server>, Json(request): Json<Request>) -> Response {
	match server.answer(&request).await {
		Ok(Some(grant)) => Json(grant).into_response(),
		Ok(None) => StatusCode::NO_CONTENT.into_response(),
		// The reason rides in the body, so an operator reading the relay's log knows
		// which rule refused without the server saying anything to the client.
		Err(denial) => {
			tracing::debug!(id = ?request.id, path = ?request.path, ?denial, "refused");
			(StatusCode::FORBIDDEN, Json(denial.body(server.policy.tier.clone()))).into_response()
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{Algorithm, Bytes, Claims, Client, Error, Peer, Transport, lease::Reason};
	use moq_pattern::Patterns;

	fn patterns(texts: &[&str]) -> Patterns {
		texts.iter().map(|text| text.parse().unwrap()).collect()
	}

	fn rules(publish: &[&str], subscribe: &[&str]) -> Permissions {
		Permissions::new(patterns(publish), patterns(subscribe))
	}

	fn request(path: &str) -> Request {
		let mut request = Request::new("relay-1", Transport::Quic, path);
		request.remote = Some("203.0.113.9:4433".parse().unwrap());
		request
	}

	#[tokio::test(start_paused = true)]
	#[tracing_test::traced_test]
	async fn refusal_fields_are_escaped_and_credentials_are_redacted() {
		let secret = "credential-that-must-not-be-logged";
		let mut facts = with_token(request("/room\nforged-record\r\x1b[31m"), secret);
		facts.id = "id\nforged-id".into();
		facts.token = Some(Token {
			kind: Token::OUT_OF_BAND,
			value: secret.as_bytes().to_vec(),
		});
		tracing::trace!(request = ?facts, "admission diagnostics");
		let response = handle(State(Server::new(Policy::default()).unwrap()), Json(facts)).await;
		assert_eq!(response.status(), StatusCode::FORBIDDEN);
		assert!(logs_contain("refused"));
		assert!(!logs_contain(secret));
		let decimal = format!("{:?}", secret.as_bytes());
		assert!(!logs_contain(&decimal[1..decimal.len() - 1]));
		assert!(!logs_contain("\nforged-record"));
		assert!(!logs_contain("\nforged-id"));

		let (dir, key) = key_dir();
		let root = "root\nforged-root\r\x1b[31m";
		let jwt = sign(&key, root, &["**"], &[], None);
		let policy = Policy {
			keys: Some(Keys::Dir(dir.path().into())),
			..Default::default()
		};
		let response = handle(
			State(Server::new(policy).unwrap()),
			Json(with_token(request("/elsewhere"), &jwt)),
		)
		.await;
		assert_eq!(response.status(), StatusCode::FORBIDDEN);
		assert!(logs_contain("RootMismatch"));
		assert!(!logs_contain(&jwt));
		assert!(!logs_contain("\nforged-root"));
		for err in [
			Denial::RootMismatch {
				root: root.into(),
				path: "path\nforged-path".into(),
			}
			.to_string(),
			Denial::NoAccess {
				root: None,
				path: "path\nforged-path".into(),
			}
			.to_string(),
			crate::Error::RootMismatch("path\nforged-path".into()).to_string(),
			crate::Error::NoAccess("path\nforged-path".into()).to_string(),
		] {
			assert!(!err.contains('\n'));
		}
	}

	#[moq_clock::test]
	async fn default_expiry_uses_the_ambient_wall() {
		let policy = Policy {
			public: rules(&["**"], &[]),
			expires: Some(Duration::from_secs(30)),
			..Default::default()
		};
		let grant = policy.decide(&request("/demo")).await.unwrap();
		assert_eq!(grant.expires, Some(moq_clock::wall() + Duration::from_secs(30)));
	}

	fn with_token(mut request: Request, jwt: &str) -> Request {
		request.query = Some(format!("jwt={jwt}"));
		request
	}

	fn with_peer(mut request: Request, expires: Option<moq_clock::Wall>) -> Request {
		request.tls = Some(Peer {
			name: "edge0".into(),
			fingerprint: "ab".repeat(32),
			expires,
			issuer: "CN=cluster".into(),
		});
		request
	}

	/// A key on disk under its kid, the way `--key-dir` reads it.
	fn key_dir() -> (tempfile::TempDir, Key) {
		let dir = tempfile::tempdir().unwrap();
		let key = Key::generate(Algorithm::HS256, Some(KeyId::decode("kid1").unwrap())).unwrap();
		key.to_file(dir.path().join("kid1.jwk")).unwrap();
		(dir, key)
	}

	fn sign(key: &Key, root: &str, publish: &[&str], subscribe: &[&str], expires: Option<moq_clock::Wall>) -> String {
		let claims = Claims::default()
			.with_root(root)
			.with_publish(patterns(publish))
			.with_subscribe(patterns(subscribe))
			.with_expires(expires);
		key.sign(&claims).unwrap()
	}

	/// The server behind a client, with a signal for each `end` it has handled.
	async fn serve(policy: Policy) -> (Client, Arc<tokio::sync::Notify>) {
		let server = Server::new(policy).unwrap();
		let ended = Arc::new(tokio::sync::Notify::new());
		let router = Router::new()
			.route(
				"/",
				post({
					let ended = ended.clone();
					move |state: State<Server>, Json(request): Json<Request>| async move {
						let is_end = matches!(request.event, Event::End { .. });
						let response = handle(state, Json(request)).await;
						if is_end {
							ended.notify_one();
						}
						response
					}
				}),
			)
			.with_state(server);
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let url = format!("http://{}/", listener.local_addr().unwrap());
		tokio::spawn(async move { axum::serve(listener, router).await });
		(Client::new(url.parse().unwrap(), None).unwrap(), ended)
	}

	#[tokio::test]
	async fn a_token_is_its_own_grant() {
		let (dir, key) = key_dir();
		let policy = Policy {
			keys: Some(Keys::Dir(dir.path().into())),
			tier: Some("gold".into()),
			..Default::default()
		};
		// `exp` is whole seconds on the wire.
		let secs = moq_clock::wall()
			.checked_duration_since(moq_clock::Wall::UNIX_EPOCH)
			.unwrap()
			.as_secs()
			+ 600;
		let exp = moq_clock::Wall::UNIX_EPOCH + Duration::from_secs(secs);
		let jwt = sign(&key, "demo/room", &["alice/**"], &["**"], Some(exp));

		let grant = policy.decide(&with_token(request("/demo/room/"), &jwt)).await.unwrap();
		assert_eq!(grant.publish, patterns(&["alice/**"]));
		assert_eq!(grant.subscribe, patterns(&["**"]));
		assert_eq!(grant.expires, Some(exp));
		assert_eq!(grant.revalidate, None);
		assert_eq!(grant.tier.as_deref(), Some("gold"));
		assert_eq!(grant.root, None);
	}

	#[tokio::test]
	async fn a_token_without_exp_gets_the_default_bound() {
		let (dir, key) = key_dir();
		let policy = Policy {
			keys: Some(Keys::Dir(dir.path().into())),
			expires: Some(Duration::from_secs(3600)),
			..Default::default()
		};
		let jwt = sign(&key, "demo", &["**"], &[], None);
		let before = moq_clock::wall();
		let grant = policy.decide(&with_token(request("/demo"), &jwt)).await.unwrap();
		let expires = grant.expires.unwrap();
		assert!(expires >= before + Duration::from_secs(3600));
		assert!(expires <= moq_clock::wall() + Duration::from_secs(3600));
	}

	#[tokio::test]
	async fn a_token_is_authorized_at_the_dialed_path() {
		let (dir, key) = key_dir();
		let policy = Policy {
			keys: Some(Keys::Dir(dir.path().into())),
			..Default::default()
		};
		let jwt = sign(&key, "demo", &["alice/**"], &["**"], None);
		let grant = policy.decide(&with_token(request("/demo/alice"), &jwt)).await.unwrap();
		assert_eq!(grant.publish, patterns(&["**"]));
		assert_eq!(grant.subscribe, patterns(&["**"]));

		let err = policy.decide(&with_token(request("/other"), &jwt)).await.unwrap_err();
		assert!(matches!(err, Denial::RootMismatch { .. }), "{err}");

		let jwt = sign(&key, "", &[], &["demo/**"], None);
		let err = policy.decide(&with_token(request("/other"), &jwt)).await.unwrap_err();
		assert!(matches!(err, Denial::NoAccess { .. }), "{err}");
	}

	#[tokio::test]
	async fn a_bad_token_never_falls_through_to_public() {
		let (dir, key) = key_dir();
		let policy = Policy {
			keys: Some(Keys::Dir(dir.path().into())),
			public: rules(&["**"], &["**"]),
			..Default::default()
		};

		// Expired.
		let jwt = sign(
			&key,
			"demo",
			&["**"],
			&[],
			Some(moq_clock::wall() - Duration::from_secs(1)),
		);
		let err = policy.decide(&with_token(request("/demo"), &jwt)).await.unwrap_err();
		assert!(matches!(err, Denial::Expired { .. }), "{err}");

		// Signed by a stranger.
		let stranger = Key::generate(Algorithm::HS256, Some(KeyId::decode("kid1").unwrap())).unwrap();
		let jwt = sign(&stranger, "demo", &["**"], &[], None);
		let err = policy.decide(&with_token(request("/demo"), &jwt)).await.unwrap_err();
		assert!(matches!(err, Denial::InvalidToken(_)), "{err}");

		// Unknown kid, and no kid at all.
		let other = Key::generate(Algorithm::HS256, Some(KeyId::decode("kid2").unwrap())).unwrap();
		let jwt = sign(&other, "demo", &["**"], &[], None);
		assert_eq!(
			policy.decide(&with_token(request("/demo"), &jwt)).await.unwrap_err(),
			Denial::UnknownKey
		);
		let bare = Key::generate(Algorithm::HS256, None).unwrap();
		let jwt = sign(&bare, "demo", &["**"], &[], None);
		assert_eq!(
			policy.decide(&with_token(request("/demo"), &jwt)).await.unwrap_err(),
			Denial::MissingKeyId
		);

		// Garbage, and a token with no keys configured at all.
		let err = policy.decide(&with_token(request("/demo"), "nope")).await.unwrap_err();
		assert!(matches!(err, Denial::MalformedToken(_)), "{err}");
		let keyless = Policy {
			public: rules(&["**"], &["**"]),
			..Default::default()
		};
		assert_eq!(
			keyless.decide(&with_token(request("/demo"), &jwt)).await.unwrap_err(),
			Denial::NoKeys
		);

		// The same request with no token is admitted by the public rules.
		assert!(policy.decide(&request("/demo")).await.is_ok());
	}

	#[tokio::test]
	async fn a_single_key_file_checks_the_kid() {
		let dir = tempfile::tempdir().unwrap();
		let key = Key::generate(Algorithm::ES256, None).unwrap();
		let path = dir.path().join("key.jwk");
		key.to_file(&path).unwrap();
		let policy = Policy {
			keys: Some(Keys::File(path)),
			..Default::default()
		};
		let jwt = sign(&key, "demo", &["**"], &[], None);
		assert!(policy.decide(&with_token(request("/demo"), &jwt)).await.is_ok());

		let mut wrong = key.export();
		wrong.kid = Some(KeyId::decode("unexpected").unwrap());
		let wrong = wrong.import().unwrap();
		let mismatched = sign(&wrong, "demo", &["**"], &[], None);
		assert_eq!(
			policy
				.decide(&with_token(request("/demo"), &mismatched))
				.await
				.unwrap_err(),
			Denial::UnknownKey
		);
	}

	#[tokio::test]
	async fn a_key_set_selects_by_kid_and_refuses_a_mismatch() {
		let dir = tempfile::tempdir().unwrap();
		let key = Key::generate(Algorithm::HS256, Some(KeyId::decode("kid1").unwrap())).unwrap();
		let other = Key::generate(Algorithm::HS256, Some(KeyId::decode("kid2").unwrap())).unwrap();
		let set = KeySet {
			keys: vec![Arc::new(key.clone()), Arc::new(other)],
		};
		let path = dir.path().join("keys.jwks");
		set.to_file(&path).unwrap();
		let policy = Policy {
			keys: Some(Keys::Set(path)),
			..Default::default()
		};
		let jwt = sign(&key, "demo", &["**"], &[], None);
		assert!(policy.decide(&with_token(request("/demo"), &jwt)).await.is_ok());

		let mut wrong = key.export();
		wrong.kid = Some(KeyId::decode("kid2").unwrap());
		let wrong = wrong.import().unwrap();
		let mismatched = sign(&wrong, "demo", &["**"], &[], None);
		assert!(matches!(
			policy.decide(&with_token(request("/demo"), &mismatched)).await,
			Err(Denial::InvalidToken(_))
		));
	}

	#[tokio::test]
	async fn gateway_transports_count_toward_session_limits() {
		let server = Server::new(Policy {
			public: rules(&["**"], &[]),
			limits: Limits {
				remote: Some(1),
				..Default::default()
			},
			..limited()
		})
		.unwrap();
		for transport in [Transport::Rtmp, Transport::Srt, Transport::WebRtc] {
			let mut first = request("/room");
			first.transport = transport;
			assert!(server.answer(&first).await.unwrap().is_some());
			let mut second = request("/room");
			second.transport = transport;
			assert_eq!(
				server.answer(&second).await.unwrap_err(),
				Denial::RemoteLimit { root: None }
			);
			first.event = Event::End {
				reason: Reason::Shutdown,
				duration: Duration::ZERO,
				bytes: Bytes::default(),
			};
			assert!(server.answer(&first).await.unwrap().is_none());
		}
	}

	fn with_setup_token(mut request: Request, kind: u64, value: &str) -> Request {
		request.token = Some(Token {
			kind,
			value: value.as_bytes().to_vec(),
		});
		request
	}

	/// A type-0 SETUP token is the deployment's JWT, verified exactly like `?jwt=`.
	#[tokio::test]
	async fn a_type_zero_setup_token_is_a_jwt() {
		let (dir, key) = key_dir();
		let policy = Policy {
			keys: Some(Keys::Dir(dir.path().into())),
			..Default::default()
		};
		let jwt = sign(&key, "demo", &["alice/**"], &[], None);
		let grant = policy
			.decide(&with_setup_token(request("/demo"), Token::OUT_OF_BAND, &jwt))
			.await
			.unwrap();
		assert_eq!(grant.publish, patterns(&["alice/**"]));

		// And refused like one: a stranger's key never falls through to public.
		let stranger = Key::generate(Algorithm::HS256, Some(KeyId::decode("kid1").unwrap())).unwrap();
		let forged = sign(&stranger, "demo", &["**"], &[], None);
		let err = policy
			.decide(&with_setup_token(request("/demo"), Token::OUT_OF_BAND, &forged))
			.await
			.unwrap_err();
		assert!(matches!(err, Denial::InvalidToken(_)), "{err}");

		// Bytes that are not text cannot be a JWT, whoever signed them.
		let mut binary = request("/demo");
		binary.token = Some(Token {
			kind: Token::OUT_OF_BAND,
			value: vec![0xff],
		});
		let err = policy.decide(&binary).await.unwrap_err();
		assert!(matches!(err, Denial::MalformedToken(_)), "{err}");
	}

	#[tokio::test]
	async fn a_setup_token_of_another_type_is_refused() {
		let (dir, key) = key_dir();
		let policy = Policy {
			keys: Some(Keys::Dir(dir.path().into())),
			public: rules(&["**"], &["**"]),
			..Default::default()
		};
		let jwt = sign(&key, "demo", &["**"], &[], None);
		let err = policy
			.decide(&with_setup_token(request("/demo"), Token::CAT, &jwt))
			.await
			.unwrap_err();
		assert_eq!(err, Denial::UnsupportedToken(Token::CAT));
		assert_eq!(
			err.to_string(),
			"SETUP token type 0x1 is not supported; only type 0 (a JWT) is"
		);
	}

	/// The same JWT in the SETUP token and the query is one credential, evaluated once.
	#[tokio::test]
	async fn a_setup_token_equal_to_the_query_jwt_is_admitted() {
		let (dir, key) = key_dir();
		let policy = Policy {
			keys: Some(Keys::Dir(dir.path().into())),
			..Default::default()
		};
		let jwt = sign(&key, "demo", &["**"], &[], None);
		let request = with_setup_token(with_token(request("/demo"), &jwt), Token::OUT_OF_BAND, &jwt);
		let grant = policy.decide(&request).await.unwrap();
		assert_eq!(grant.publish, patterns(&["**"]));
	}

	/// Two different credentials would leave the server guessing which one the client meant.
	#[tokio::test]
	async fn a_setup_token_and_a_different_query_jwt_are_refused() {
		let (dir, key) = key_dir();
		let policy = Policy {
			keys: Some(Keys::Dir(dir.path().into())),
			..Default::default()
		};
		let setup = sign(&key, "demo", &["**"], &[], None);
		let query = sign(&key, "demo", &[], &["**"], None);
		let different = with_setup_token(with_token(request("/demo"), &query), Token::OUT_OF_BAND, &setup);
		assert_eq!(policy.decide(&different).await.unwrap_err(), Denial::TwoTokens);

		// The same bytes under another token type are still a second credential.
		let other_type = with_setup_token(with_token(request("/demo"), &setup), Token::CAT, &setup);
		assert_eq!(policy.decide(&other_type).await.unwrap_err(), Denial::TwoTokens);
	}

	/// Two `jwt` parameters would leave the server guessing which one the client meant.
	#[tokio::test]
	async fn a_duplicate_jwt_is_refused() {
		let (dir, key) = key_dir();
		let policy = Policy {
			keys: Some(Keys::Dir(dir.path().into())),
			public: rules(&["**"], &["**"]),
			..Default::default()
		};
		let jwt = sign(&key, "demo", &["**"], &[], None);

		let mut request = request("/demo");
		request.query = Some(format!("a=1&jwt={jwt}"));
		assert_eq!(query_jwt(&request).unwrap().as_deref(), Some(jwt.as_str()));
		assert!(policy.decide(&request).await.is_ok());

		// Even the same token twice, an empty second one, or an escaped key.
		for query in [
			format!("jwt={jwt}&jwt={jwt}"),
			format!("jwt={jwt}&jwt="),
			format!("jwt=&jwt={jwt}"),
			format!("jwt={jwt}&j%77t=stale"),
		] {
			request.query = Some(query);
			assert_eq!(policy.decide(&request).await.unwrap_err(), Denial::DuplicateJwt);
		}

		// An empty one alone is no token, so the session is anonymous.
		request.query = Some("jwt=&b=2".into());
		assert_eq!(query_jwt(&request).unwrap(), None);
		assert!(policy.decide(&request).await.is_ok());

		// The value is percent-decoded once.
		request.query = Some(format!("jwt={}", jwt.replace('.', "%2E")));
		assert_eq!(query_jwt(&request).unwrap().as_deref(), Some(jwt.as_str()));
	}

	#[tokio::test]
	async fn a_certificate_is_a_fact_and_admits_only_what_is_granted() {
		let none = Policy::default();
		assert_eq!(
			none.decide(&with_peer(request("/"), None)).await.unwrap_err(),
			Denial::NoMtlsGrant
		);

		let policy = Policy {
			mtls: rules(&["**"], &["**"]),
			..Default::default()
		};
		let not_after = moq_clock::wall() + Duration::from_secs(86400 * 30);
		let grant = policy.decide(&with_peer(request("/"), Some(not_after))).await.unwrap();
		assert_eq!(grant.publish, patterns(&["**"]));
		assert_eq!(grant.expires, Some(not_after));

		// A certificate without a bound gets none by default, like 0.14.
		let grant = policy.decide(&with_peer(request("/"), None)).await.unwrap();
		assert_eq!(grant.expires, None);
		// A certificate is a client unless the policy says it is a relay.
		assert!(!grant.peer);

		// The certificate does not stand in for a public grant.
		assert_eq!(policy.decide(&request("/")).await.unwrap_err(), Denial::NoPublicGrant);
	}

	/// The link role marks only a certificate's grant, never an anonymous session or JWT.
	#[tokio::test]
	async fn mtls_peer_marks_only_certificates() {
		let (dir, key) = key_dir();
		let policy = Policy {
			keys: Some(Keys::Dir(dir.path().into())),
			public: rules(&["**"], &["**"]),
			mtls: rules(&["**"], &["**"]),
			mtls_link: Some(Link::Peer),
			..Default::default()
		};
		let grant = policy.decide(&with_peer(request("/"), None)).await.unwrap();
		assert!(grant.peer && !grant.upstream);

		let policy = Policy {
			mtls_link: Some(Link::Upstream),
			..policy
		};
		let grant = policy.decide(&with_peer(request("/"), None)).await.unwrap();
		assert!(grant.peer && grant.upstream);
		grant.validate().unwrap();
		let grant = policy.decide(&request("/")).await.unwrap();
		assert!(!grant.peer && !grant.upstream);
		let jwt = sign(&key, "", &["**"], &[], None);
		let grant = policy.decide(&with_token(request("/"), &jwt)).await.unwrap();
		assert!(!grant.peer && !grant.upstream);
	}

	/// Every representable mTLS link role produces a valid wire grant.
	#[tokio::test]
	async fn mtls_link_roles_preserve_the_grant_contract() {
		for (link, peer, upstream) in [
			(None, false, false),
			(Some(Link::Peer), true, false),
			(Some(Link::Upstream), true, true),
		] {
			let policy = Policy {
				mtls: rules(&["**"], &["**"]),
				mtls_link: link,
				..Default::default()
			};
			let grant = policy.decide(&with_peer(request("/"), None)).await.unwrap();
			assert_eq!((grant.peer, grant.upstream), (peer, upstream));
			grant.validate().unwrap();
		}
	}

	/// A certificate would override a JWT meant to narrow it, and a JWT would narrow or
	/// refuse a peer by accident, so presenting both is refused, even with a bad JWT.
	#[tokio::test]
	async fn a_jwt_and_a_certificate_together_are_refused() {
		let (dir, key) = key_dir();
		let policy = Policy {
			keys: Some(Keys::Dir(dir.path().into())),
			mtls: rules(&["**"], &["**"]),
			..Default::default()
		};
		let jwt = sign(&key, "demo", &["**"], &[], None);
		for request in [
			with_peer(with_token(request("/demo"), &jwt), None),
			with_peer(with_token(request("/demo"), "garbage"), None),
			with_peer(with_setup_token(request("/demo"), Token::OUT_OF_BAND, &jwt), None),
		] {
			assert_eq!(policy.decide(&request).await.unwrap_err(), Denial::TokenAndCertificate);
		}
		assert!(policy.decide(&with_peer(request("/demo"), None)).await.is_ok());
		assert!(policy.decide(&with_token(request("/demo"), &jwt)).await.is_ok());
	}

	#[tokio::test]
	async fn anonymous_gets_the_public_rules() {
		let policy = Policy {
			public: rules(&[], &["anon/**"]),
			..Default::default()
		};
		let grant = policy.decide(&request("/")).await.unwrap();
		assert_eq!(grant.subscribe, patterns(&["anon/**"]));
		assert!(grant.publish.is_empty());
		// Never re-checked or closed by default, like 0.14.
		assert_eq!(grant.expires, None);
		assert_eq!(grant.revalidate, None);
	}

	/// The rules are rooted at `/`, not at the dialed path: `anon/**` scopes a session
	/// dialed anywhere to `anon/`, and refuses one dialed outside it. Bare `**` reads
	/// the same either way, which is how rooting them at the dialed path went unnoticed.
	#[tokio::test]
	async fn public_and_mtls_rules_are_rooted_at_slash() {
		let policy = Policy {
			public: rules(&["anon/**"], &["anon/**", "*/chat"]),
			mtls: rules(&["origin/*"], &["origin/*"]),
			..Default::default()
		};

		for (path, publish, subscribe) in [
			("/", &["anon/**"][..], &["anon/**", "*/chat"][..]),
			("/anon", &["**"], &["**", "chat"]),
			("/anon/room", &["**"], &["**"]),
			("/rooms", &[], &["chat"]),
		] {
			let grant = policy.decide(&request(path)).await.unwrap();
			assert_eq!(grant.publish, patterns(publish), "{path}");
			assert_eq!(grant.subscribe, patterns(subscribe), "{path}");
			assert_eq!(grant.root, None, "{path}");
		}

		// Before, `/rooms/123` got `rooms/123/anon/**`: into any room, anonymously.
		for path in ["/rooms/123", "/other/room", "/anonymous/room"] {
			assert_eq!(
				policy.decide(&request(path)).await.unwrap_err(),
				Denial::NoAccess {
					path: path.trim_start_matches('/').into(),
					root: None
				},
			);
		}

		let grant = policy.decide(&with_peer(request("/origin/edge0"), None)).await.unwrap();
		assert_eq!(grant.publish, patterns(&[""]));
		let err = policy.decide(&with_peer(request("/anon"), None)).await.unwrap_err();
		assert!(matches!(err, Denial::NoAccess { .. }), "{err}");
	}

	#[tokio::test]
	async fn limits_count_connect_end_and_aging() {
		tokio::time::pause();
		let server = Server::new(Policy {
			public: rules(&["**"], &["**"]),
			limits: Limits {
				token: None,
				remote: Some(2),
			},
			revalidate: Some(Duration::from_secs(60)),
			expires: Some(Duration::from_secs(24 * 60 * 60)),
			..Default::default()
		})
		.unwrap();

		let first = request("/");
		let second = request("/");
		let third = request("/");
		server.answer(&first).await.unwrap();
		server.answer(&second).await.unwrap();
		assert_eq!(
			server.answer(&third).await.unwrap_err(),
			Denial::RemoteLimit { root: None }
		);

		// A repeated connect for a known id refreshes rather than double-counting.
		server.answer(&first).await.unwrap();
		assert_eq!(
			server.answer(&third).await.unwrap_err(),
			Denial::RemoteLimit { root: None }
		);

		// Another address is unaffected.
		let mut elsewhere = request("/");
		elsewhere.remote = Some("[::ffff:198.51.100.7]:1".parse().unwrap());
		server.answer(&elsewhere).await.unwrap();

		// An end frees the slot.
		let mut ended = first.clone();
		ended.event = Event::End {
			reason: Reason::Dropped,
			duration: Duration::ZERO,
			bytes: Bytes::default(),
		};
		assert!(server.answer(&ended).await.unwrap().is_none());
		server.answer(&third).await.unwrap();

		// A session that misses two cadences ages out; one that revalidates stays.
		let mut revalidate = second.clone();
		revalidate.event = Event::Revalidate;
		tokio::time::advance(Duration::from_secs(90)).await;
		server.answer(&revalidate).await.unwrap();
		tokio::time::advance(Duration::from_secs(90)).await;
		// `third` is 180s old and gone; `second` was seen 90s ago and holds its slot.
		let fourth = request("/");
		server.answer(&fourth).await.unwrap();
		assert_eq!(
			server.answer(&request("/")).await.unwrap_err(),
			Denial::RemoteLimit { root: None }
		);
	}

	/// The re-check a session table ages out by, which every limit needs.
	fn limited() -> Policy {
		Policy {
			revalidate: Some(Duration::from_secs(60)),
			expires: Some(Duration::from_secs(3600)),
			..Default::default()
		}
	}

	/// Without a re-check, a relay that died would hold its slots forever, and a
	/// re-check without a bound would outlive an outage.
	#[test]
	fn a_server_refuses_limits_without_a_cadence() {
		let capped = Policy {
			limits: Limits {
				token: Some(1),
				remote: None,
			},
			..Default::default()
		};
		assert!(matches!(Server::new(capped), Err(Error::LimitsWithoutRevalidate)));
		let unbounded = Policy {
			revalidate: Some(Duration::from_secs(60)),
			..Default::default()
		};
		assert!(matches!(Server::new(unbounded), Err(Error::UnboundedRevalidate)));
		let quoted = Policy {
			tier: Some("edge\"".into()),
			..Default::default()
		};
		assert!(matches!(Server::new(quoted), Err(Error::InvalidTier(_))));
	}

	/// Regression: `--expires` past the system clock's range panicked every answer.
	/// The server refuses it up front, and the policy alone refuses the session
	/// rather than grant it no bound.
	#[tokio::test]
	async fn a_bound_past_the_clock_is_refused() {
		let endless = Policy {
			public: rules(&["**"], &["**"]),
			expires: Some(Duration::MAX),
			..Default::default()
		};
		assert_eq!(
			endless.decide(&request("/")).await.unwrap_err(),
			Denial::ExpiresOutOfRange
		);
		assert!(matches!(Server::new(endless), Err(Error::ExpiresOutOfRange)));
	}

	/// Regression: `--revalidate` near `Duration::MAX` overflowed the two-cadence
	/// sweep and panicked every answer once a limit kept a session table.
	#[tokio::test]
	async fn a_cadence_past_the_clock_keeps_every_slot() {
		let server = Server::new(Policy {
			public: rules(&["**"], &["**"]),
			limits: Limits {
				token: None,
				remote: Some(1),
			},
			revalidate: Some(Duration::MAX),
			..limited()
		})
		.unwrap();
		server.answer(&request("/")).await.unwrap();
		assert_eq!(
			server.answer(&request("/")).await.unwrap_err(),
			Denial::RemoteLimit { root: None }
		);
	}

	/// With no limit to count against, nothing is kept per session, so a relay that
	/// dies without sending `end` leaks nothing here.
	#[tokio::test]
	async fn no_limits_keep_no_sessions() {
		let server = Server::new(Policy {
			public: rules(&["**"], &["**"]),
			..Default::default()
		})
		.unwrap();
		for _ in 0..3 {
			server.answer(&request("/")).await.unwrap();
		}
		assert!(server.sessions.lock().unwrap().slots.is_empty());
	}

	#[tokio::test]
	async fn limits_count_sessions_per_token_and_fold_mapped_addresses() {
		let (dir, key) = key_dir();
		let server = Server::new(Policy {
			keys: Some(Keys::Dir(dir.path().into())),
			limits: Limits {
				token: Some(1),
				remote: Some(1),
			},
			..limited()
		})
		.unwrap();
		let jwt = sign(&key, "demo", &["**"], &[], None);

		server.answer(&with_token(request("/demo"), &jwt)).await.unwrap();
		let mut mapped = with_token(request("/demo"), &jwt);
		mapped.remote = Some("[::ffff:203.0.113.9]:9".parse().unwrap());
		assert_eq!(
			server.answer(&mapped).await.unwrap_err(),
			Denial::TokenLimit {
				root: Some("demo".into())
			}
		);

		let other = sign(
			&key,
			"demo",
			&["**"],
			&[],
			Some(moq_clock::wall() + Duration::from_secs(60)),
		);
		let mut same_host = with_token(request("/demo"), &other);
		same_host.remote = Some("[::ffff:203.0.113.9]:9".parse().unwrap());
		assert_eq!(
			server.answer(&same_host).await.unwrap_err(),
			Denial::RemoteLimit {
				root: Some("demo".into())
			}
		);
	}

	/// An in-band credential arrives as a `connect` for a live session id. It takes
	/// no new remote slot, but counts against its own JWT's cap like a session would.
	#[tokio::test]
	async fn attached_tokens_count_against_their_own_cap() {
		let (dir, key) = key_dir();
		let server = Server::new(Policy {
			keys: Some(Keys::Dir(dir.path().into())),
			limits: Limits {
				token: Some(1),
				remote: None,
			},
			..limited()
		})
		.unwrap();
		let exp = Some(moq_clock::wall() + Duration::from_secs(60));
		let a = sign(&key, "demo", &["**"], &[], None);
		let b = sign(&key, "demo", &["**"], &[], exp);
		let c = sign(
			&key,
			"demo",
			&["**"],
			&[],
			Some(moq_clock::wall() + Duration::from_secs(61)),
		);

		let mut first = with_token(request("/demo"), &a);
		first.id = "first".into();
		let mut second = with_token(request("/demo"), &c);
		second.id = "second".into();
		server.answer(&first).await.unwrap();
		server.answer(&second).await.unwrap();

		let attach = |session: &Request| {
			let mut attach = with_token(session.clone(), &b);
			attach.id = session.id.clone();
			attach
		};
		server.answer(&attach(&first)).await.unwrap();
		// Re-presenting a JWT the session already holds is not another use of it.
		server.answer(&attach(&first)).await.unwrap();
		assert_eq!(
			server.answer(&attach(&second)).await.unwrap_err(),
			Denial::TokenLimit {
				root: Some("demo".into())
			}
		);
		assert_eq!(server.sessions.lock().unwrap().slots.len(), 2);
	}

	#[tokio::test]
	async fn the_client_admits_and_reads_the_refusal() {
		let (client, ended) = serve(Policy {
			public: rules(&["**"], &[]),
			limits: Limits {
				token: None,
				remote: Some(1),
			},
			..limited()
		})
		.await;

		let consumer = client.connect(request("/demo")).await.unwrap();
		assert_eq!(consumer.grant().publish, patterns(&["**"]));

		let err = client.connect(request("/demo")).await.unwrap_err();
		assert!(matches!(err, Error::Refused(_)), "{err}");

		// The end frees the slot for the next session.
		consumer.close("done", Bytes::default());
		ended.notified().await;
		client.connect(request("/demo")).await.unwrap();
	}

	#[tokio::test]
	async fn serve_client_refusals_attribute_only_verified_roots() {
		use crate::refusal::Reason;
		let (dir, key) = key_dir();
		let stranger = Key::generate(Algorithm::HS256, key.kid.clone()).unwrap();
		let expired = Some(moq_clock::Wall::UNIX_EPOCH + Duration::from_secs(1));
		for tier in [None, Some("edge")] {
			let (client, _) = serve(Policy {
				keys: Some(Keys::Dir(dir.path().into())),
				tier: tier.map(str::to_owned),
				..Default::default()
			})
			.await;
			let expired_jwt = sign(&key, "tenant", &["media/**"], &[], expired);
			let valid_jwt = sign(&key, "tenant", &["media/**"], &[], None);
			let forged_jwt = sign(&stranger, "forged", &["**"], &[], expired);
			for (path, token, reason, root) in [
				(
					"/attacker/path",
					Some(expired_jwt.as_str()),
					Reason::Expired,
					Some("tenant"),
				),
				(
					"/attacker/path",
					Some(valid_jwt.as_str()),
					Reason::Forbidden,
					Some("tenant"),
				),
				(
					"/tenant/no-access",
					Some(valid_jwt.as_str()),
					Reason::Forbidden,
					Some("tenant"),
				),
				("/attacker/path", Some(forged_jwt.as_str()), Reason::Refused, None),
				("/attacker/path", Some("nope"), Reason::Malformed, None),
				("/attacker/path", None, Reason::Refused, None),
			] {
				let mut request = request(path);
				if let Some(jwt) = token {
					request = with_token(request, jwt);
				}
				let Error::Refused(refusal) = client.connect(request).await.unwrap_err() else {
					panic!("expected refusal")
				};
				assert_eq!(refusal.reason, reason);
				assert_eq!(refusal.root.as_deref(), root);
				assert_eq!(refusal.tier.as_deref(), tier);
			}
		}
	}

	#[tokio::test]
	async fn expired_out_of_scope_claims_are_never_attributed() {
		let (dir, key) = key_dir();
		let scoped = key
			.clone()
			.with_scope(crate::Scope {
				root: "tenant".into(),
				publish: patterns(&["allowed/**"]),
				subscribe: Patterns::default(),
			})
			.unwrap();
		scoped.to_file(dir.path().join("kid1.jwk")).unwrap();
		let (client, _) = serve(Policy {
			keys: Some(Keys::Dir(dir.path().into())),
			..Default::default()
		})
		.await;
		let token = sign(
			&key,
			"attacker",
			&["**"],
			&[],
			Some(moq_clock::Wall::UNIX_EPOCH + Duration::from_secs(1)),
		);
		let Error::Refused(refusal) = client
			.connect(with_token(request("/attacker"), &token))
			.await
			.unwrap_err()
		else {
			panic!("expected refusal")
		};
		assert_eq!(refusal, crate::Refusal::default());
	}

	#[cfg(unix)]
	#[tokio::test]
	async fn serves_on_a_unix_socket() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("auth.sock");
		let listener = tokio::net::UnixListener::bind(&path).unwrap();
		let server = Server::new(Policy {
			public: rules(&["**"], &["**"]),
			..Default::default()
		})
		.unwrap();
		tokio::spawn(async move { server.serve_unix(listener).await });

		let url = url::Url::from_file_path(&path).unwrap();
		let url = format!("unix://{}", url.path()).parse().unwrap();
		let client = Client::new(url, None).unwrap();
		let consumer = client.connect(request("/")).await.unwrap();
		assert_eq!(consumer.grant().subscribe, patterns(&["**"]));
	}
}
