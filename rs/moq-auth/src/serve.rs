//! The reference auth server: the policy a relay used to hold, answering the contract.
//!
//! [`Policy`] decides a [`Request`] the way `--auth-key`, `--auth-key-dir`, and
//! `--auth-public` did on the relay, plus an explicit grant for mTLS peers and live
//! session caps. Its anonymous and mTLS rules are rooted at `/`, like a token with an
//! empty root, and authorized at the dialed path the same way. [`Server`] serves it
//! on a TCP or unix listener as `POST /`. `moq auth serve` is the CLI; the relay's
//! tests run against it in process.

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use tokio::time::Instant;

use crate::{Event, Grant, Key, KeyId, Permissions, Request, Token};

/// Where the signing keys a `jwt` is verified against come from. Read per request,
/// so a rotated file takes effect without a restart.
#[derive(Clone, Debug)]
pub enum Keys {
	/// One key file; a token's `kid` is not checked against it.
	File(PathBuf),
	/// A directory of `{kid}.jwk`, selected by the token's `kid`.
	Dir(PathBuf),
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

/// Why a session was refused; the body of the 403.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Refusal {
	#[error("no keys are configured, so a token cannot be verified")]
	NoKeys,
	#[error("the token names no key and the key directory needs one")]
	MissingKeyId,
	#[error("the token's key is unknown")]
	UnknownKey,
	#[error("the token is invalid: {0}")]
	InvalidToken(String),
	#[error("the token root `{root}` does not overlap the dialed path `{path}`")]
	RootMismatch { root: String, path: String },
	#[error("nothing is granted at `{path}`")]
	NoAccess { path: String },
	#[error("a certificate was presented but nothing is granted to certificates")]
	NoMtlsGrant,
	#[error("anonymous access is not granted")]
	NoPublicGrant,
	#[error("too many live sessions for this token")]
	TokenLimit,
	#[error("too many live sessions from this address")]
	RemoteLimit,
	#[error("a SETUP token and a different `jwt` query were presented; present one")]
	TwoTokens,
	#[error("SETUP token type {0:#x} is not supported; only type 0 (a JWT) is")]
	UnsupportedToken(u64),
	#[error("both a JWT and a client certificate were presented; present one")]
	TokenAndCertificate,
	#[error("the policy's grant bound reaches past the system clock's range")]
	ExpiresOutOfRange,
}

impl Policy {
	/// Decide `request` by the policy alone, ignoring session limits.
	pub async fn decide(&self, request: &Request) -> Result<Grant, Refusal> {
		let jwt = jwt(request)?;
		if jwt.is_some() && request.tls.is_some() {
			return Err(Refusal::TokenAndCertificate);
		}
		let (permissions, expires) = if let Some(jwt) = jwt {
			let key = self.key(jwt).await?;
			let claims = key.verify(jwt).map_err(|err| Refusal::InvalidToken(err.to_string()))?;
			let permissions = claims.authorize(&request.path).map_err(|err| match err {
				crate::Error::RootMismatch(path) => Refusal::RootMismatch {
					root: claims.root.clone(),
					path,
				},
				crate::Error::NoAccess(path) => Refusal::NoAccess { path },
				other => Refusal::InvalidToken(other.to_string()),
			})?;
			(permissions, claims.expires)
		} else if let Some(peer) = &request.tls {
			if self.mtls.is_empty() {
				return Err(Refusal::NoMtlsGrant);
			}
			(authorize(&self.mtls, &request.path)?, peer.expires)
		} else {
			if self.public.is_empty() {
				return Err(Refusal::NoPublicGrant);
			}
			(authorize(&self.public, &request.path)?, None)
		};

		let mut grant = Grant::new(permissions.publish, permissions.subscribe);
		grant.expires = match (expires, self.expires) {
			(Some(expires), _) => Some(expires),
			(None, Some(bound)) => Some(SystemTime::now().checked_add(bound).ok_or(Refusal::ExpiresOutOfRange)?),
			(None, None) => None,
		};
		grant.revalidate = self.revalidate;
		grant.tier = self.tier.clone();
		Ok(grant)
	}

	async fn key(&self, jwt: &str) -> Result<Key, Refusal> {
		let path = match self.keys.as_ref().ok_or(Refusal::NoKeys)? {
			Keys::File(path) => path.clone(),
			Keys::Dir(dir) => {
				let header = jsonwebtoken::decode_header(jwt).map_err(|err| Refusal::InvalidToken(err.to_string()))?;
				let kid = header.kid.ok_or(Refusal::MissingKeyId)?;
				let kid = KeyId::decode(&kid).map_err(|_| Refusal::UnknownKey)?;
				dir.join(format!("{kid}.jwk"))
			}
		};
		Key::from_file_async(&path).await.map_err(|_| Refusal::UnknownKey)
	}
}

/// Authorize rules rooted at `/` at the dialed `path`, exactly as a token with an
/// empty root would be.
fn authorize(rules: &Permissions, path: &str) -> Result<Permissions, Refusal> {
	let claims = crate::Claims {
		publish: rules.publish.clone(),
		subscribe: rules.subscribe.clone(),
		..Default::default()
	};
	// An empty root overlaps every path, so the only refusal is reaching nothing.
	claims.authorize(path).map_err(|_| Refusal::NoAccess {
		path: crate::path::normalize(path),
	})
}

/// The JWT the request presents: its SETUP token, or else its `jwt` query parameter.
/// Both at once count as one only when they are the same JWT.
fn jwt(request: &Request) -> Result<Option<&str>, Refusal> {
	match (&request.token, query_jwt(request)) {
		// A client that offers a version without in-band auth copies its SETUP token
		// into the URL too, so the same JWT arrives twice.
		(Some(token), Some(jwt)) if token.kind == Token::OUT_OF_BAND && token.value == jwt.as_bytes() => Ok(Some(jwt)),
		(Some(_), Some(_)) => Err(Refusal::TwoTokens),
		(Some(token), None) if token.kind == Token::OUT_OF_BAND => std::str::from_utf8(&token.value)
			.map(Some)
			.map_err(|_| Refusal::InvalidToken("the SETUP token is not UTF-8".into())),
		(Some(token), None) => Err(Refusal::UnsupportedToken(token.kind)),
		(None, jwt) => Ok(jwt),
	}
}

/// The `jwt` query parameter, when the request carries a non-empty one. The last one
/// wins, as it did on the relay, so a client that appends a fresh token is believed.
fn query_jwt(request: &Request) -> Option<&str> {
	let query = request.query.as_deref()?;
	// Borrow rather than decode: a JWT is base64url and never needs unescaping.
	query
		.split('&')
		.filter_map(|pair| pair.strip_prefix("jwt="))
		.rfind(|jwt| !jwt.is_empty())
}

/// The remote address without its port, an IPv4-mapped IPv6 address folded to IPv4.
fn remote(request: &Request) -> Option<IpAddr> {
	request.remote.map(|addr| addr.ip().to_canonical())
}

/// One live session's share of the limits.
struct Slot {
	token: Option<u64>,
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

	/// Admit a `connect`, refusing over the cap. A known id refreshes instead.
	fn connect(&mut self, request: &Request, limits: Limits) -> Result<(), Refusal> {
		if let Some(slot) = self.slots.get_mut(&request.id) {
			slot.seen = Instant::now();
			return Ok(());
		}
		// Decided before counting, so the credential is already known to be one JWT.
		let token = jwt(request).ok().flatten().map(hash);
		let remote = remote(request);
		if let (Some(cap), Some(token)) = (limits.token, token)
			&& self.slots.values().filter(|slot| slot.token == Some(token)).count() >= cap
		{
			return Err(Refusal::TokenLimit);
		}
		if let (Some(cap), Some(remote)) = (limits.remote, remote)
			&& self.slots.values().filter(|slot| slot.remote == Some(remote)).count() >= cap
		{
			return Err(Refusal::RemoteLimit);
		}
		self.slots.insert(
			request.id.clone(),
			Slot {
				token,
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
		let slot = self.slots.entry(request.id.clone()).or_insert_with(|| Slot {
			token: jwt(request).ok().flatten().map(hash),
			remote: remote(request),
			seen: Instant::now(),
		});
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
	/// a bound, bounds grants past the clock's range, or caps sessions without the
	/// re-check that ages out a dead relay's.
	pub fn new(policy: Policy) -> crate::Result<Self> {
		if policy.revalidate.is_some() && policy.expires.is_none() {
			return Err(crate::Error::UnboundedRevalidate);
		}
		if policy
			.expires
			.is_some_and(|bound| SystemTime::now().checked_add(bound).is_none())
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
	pub async fn answer(&self, request: &Request) -> Result<Option<Grant>, Refusal> {
		match request.event {
			Event::Connect => {
				let grant = self.policy.decide(request).await?;
				if let Some(cadence) = self.cadence() {
					let mut sessions = self.sessions.lock().unwrap();
					sessions.sweep(cadence);
					sessions.connect(request, self.policy.limits)?;
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
		Err(refusal) => {
			tracing::debug!(id = %request.id, path = %request.path, %refusal, "refused");
			(StatusCode::FORBIDDEN, refusal.to_string()).into_response()
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

	fn with_token(mut request: Request, jwt: &str) -> Request {
		request.query = Some(format!("jwt={jwt}"));
		request
	}

	fn with_peer(mut request: Request, expires: Option<SystemTime>) -> Request {
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

	fn sign(key: &Key, root: &str, publish: &[&str], subscribe: &[&str], expires: Option<SystemTime>) -> String {
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
		let secs = SystemTime::now()
			.duration_since(SystemTime::UNIX_EPOCH)
			.unwrap()
			.as_secs()
			+ 600;
		let exp = SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
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
		let before = SystemTime::now();
		let grant = policy.decide(&with_token(request("/demo"), &jwt)).await.unwrap();
		let expires = grant.expires.unwrap();
		assert!(expires >= before + Duration::from_secs(3600));
		assert!(expires <= SystemTime::now() + Duration::from_secs(3600));
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
		assert!(matches!(err, Refusal::RootMismatch { .. }), "{err}");

		let jwt = sign(&key, "", &[], &["demo/**"], None);
		let err = policy.decide(&with_token(request("/other"), &jwt)).await.unwrap_err();
		assert!(matches!(err, Refusal::NoAccess { .. }), "{err}");
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
			Some(SystemTime::now() - Duration::from_secs(1)),
		);
		let err = policy.decide(&with_token(request("/demo"), &jwt)).await.unwrap_err();
		assert!(matches!(err, Refusal::InvalidToken(_)), "{err}");

		// Signed by a stranger.
		let stranger = Key::generate(Algorithm::HS256, Some(KeyId::decode("kid1").unwrap())).unwrap();
		let jwt = sign(&stranger, "demo", &["**"], &[], None);
		let err = policy.decide(&with_token(request("/demo"), &jwt)).await.unwrap_err();
		assert!(matches!(err, Refusal::InvalidToken(_)), "{err}");

		// Unknown kid, and no kid at all.
		let other = Key::generate(Algorithm::HS256, Some(KeyId::decode("kid2").unwrap())).unwrap();
		let jwt = sign(&other, "demo", &["**"], &[], None);
		assert_eq!(
			policy.decide(&with_token(request("/demo"), &jwt)).await.unwrap_err(),
			Refusal::UnknownKey
		);
		let bare = Key::generate(Algorithm::HS256, None).unwrap();
		let jwt = sign(&bare, "demo", &["**"], &[], None);
		assert_eq!(
			policy.decide(&with_token(request("/demo"), &jwt)).await.unwrap_err(),
			Refusal::MissingKeyId
		);

		// Garbage, and a token with no keys configured at all.
		let err = policy.decide(&with_token(request("/demo"), "nope")).await.unwrap_err();
		assert!(matches!(err, Refusal::InvalidToken(_)), "{err}");
		let keyless = Policy {
			public: rules(&["**"], &["**"]),
			..Default::default()
		};
		assert_eq!(
			keyless.decide(&with_token(request("/demo"), &jwt)).await.unwrap_err(),
			Refusal::NoKeys
		);

		// The same request with no token is admitted by the public rules.
		assert!(policy.decide(&request("/demo")).await.is_ok());
	}

	#[tokio::test]
	async fn a_single_key_file_ignores_the_kid() {
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
		assert!(matches!(err, Refusal::InvalidToken(_)), "{err}");
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
		assert_eq!(err, Refusal::UnsupportedToken(Token::CAT));
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
		assert_eq!(policy.decide(&different).await.unwrap_err(), Refusal::TwoTokens);

		// The same bytes under another token type are still a second credential.
		let other_type = with_setup_token(with_token(request("/demo"), &setup), Token::CAT, &setup);
		assert_eq!(policy.decide(&other_type).await.unwrap_err(), Refusal::TwoTokens);
	}

	#[tokio::test]
	async fn the_last_jwt_in_the_query_wins() {
		let (dir, key) = key_dir();
		let policy = Policy {
			keys: Some(Keys::Dir(dir.path().into())),
			..Default::default()
		};
		let fresh = sign(&key, "demo", &["**"], &[], None);

		// A client that appends a fresh token after a stale one is believed, as on the
		// relay; a trailing empty value does not blank it out.
		let mut request = request("/demo");
		request.query = Some(format!("a=1&jwt=stale&jwt={fresh}&jwt="));
		assert_eq!(query_jwt(&request), Some(fresh.as_str()));
		assert!(policy.decide(&request).await.is_ok());

		request.query = Some(format!("jwt={fresh}&jwt=stale"));
		let err = policy.decide(&request).await.unwrap_err();
		assert!(matches!(err, Refusal::InvalidToken(_)), "{err}");

		request.query = Some("jwt=&b=2".into());
		assert_eq!(query_jwt(&request), None);
	}

	#[tokio::test]
	async fn a_certificate_is_a_fact_and_admits_only_what_is_granted() {
		let none = Policy::default();
		assert_eq!(
			none.decide(&with_peer(request("/"), None)).await.unwrap_err(),
			Refusal::NoMtlsGrant
		);

		let policy = Policy {
			mtls: rules(&["**"], &["**"]),
			..Default::default()
		};
		let not_after = SystemTime::now() + Duration::from_secs(86400 * 30);
		let grant = policy.decide(&with_peer(request("/"), Some(not_after))).await.unwrap();
		assert_eq!(grant.publish, patterns(&["**"]));
		assert_eq!(grant.expires, Some(not_after));

		// A certificate without a bound gets none by default, like 0.14.
		let grant = policy.decide(&with_peer(request("/"), None)).await.unwrap();
		assert_eq!(grant.expires, None);

		// The certificate does not stand in for a public grant.
		assert_eq!(policy.decide(&request("/")).await.unwrap_err(), Refusal::NoPublicGrant);
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
			assert_eq!(policy.decide(&request).await.unwrap_err(), Refusal::TokenAndCertificate);
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
				Refusal::NoAccess {
					path: path.trim_start_matches('/').into()
				},
			);
		}

		let grant = policy.decide(&with_peer(request("/origin/edge0"), None)).await.unwrap();
		assert_eq!(grant.publish, patterns(&[""]));
		let err = policy.decide(&with_peer(request("/anon"), None)).await.unwrap_err();
		assert!(matches!(err, Refusal::NoAccess { .. }), "{err}");
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
		assert_eq!(server.answer(&third).await.unwrap_err(), Refusal::RemoteLimit);

		// A repeated connect for a known id refreshes rather than double-counting.
		server.answer(&first).await.unwrap();
		assert_eq!(server.answer(&third).await.unwrap_err(), Refusal::RemoteLimit);

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
		assert_eq!(server.answer(&request("/")).await.unwrap_err(), Refusal::RemoteLimit);
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
			Refusal::ExpiresOutOfRange
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
		assert_eq!(server.answer(&request("/")).await.unwrap_err(), Refusal::RemoteLimit);
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
		assert_eq!(server.answer(&mapped).await.unwrap_err(), Refusal::TokenLimit);

		let other = sign(
			&key,
			"demo",
			&["**"],
			&[],
			Some(SystemTime::now() + Duration::from_secs(60)),
		);
		let mut same_host = with_token(request("/demo"), &other);
		same_host.remote = Some("[::ffff:203.0.113.9]:9".parse().unwrap());
		assert_eq!(server.answer(&same_host).await.unwrap_err(), Refusal::RemoteLimit);
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
		assert!(matches!(err, Error::Refused), "{err}");

		// The end frees the slot for the next session.
		consumer.close("done", Bytes::default());
		ended.notified().await;
		client.connect(request("/demo")).await.unwrap();
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
