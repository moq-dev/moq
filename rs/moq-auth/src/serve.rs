//! The reference auth server: the policy a relay used to hold, answering the contract.
//!
//! [`Policy`] decides a [`Request`] the way `--auth-key`, `--auth-key-dir`, and
//! `--auth-public` did on the relay, plus an explicit grant for mTLS peers, rules
//! scoping particular certificates, and live session caps. Its anonymous and mTLS rules are rooted at `/`, like a token with an
//! empty root, and authorized at the dialed path the same way. [`Server`] serves it
//! on a TCP or unix listener as `POST /`. `moq auth serve` is the CLI; the relay's
//! tests run against it in process.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use kio::clock::Instant;

use crate::{Event, Grant, Key, KeyId, KeySet, Patterns, Permissions, Rate, Request, Token};

/// Where the signing keys a `jwt` is verified against come from. Each file is parsed
/// once and re-read when its modification time or size changes, so a rotated file
/// takes effect without a restart.
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

impl serde::Serialize for Link {
	fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		serializer.collect_str(self)
	}
}

impl<'de> serde::Deserialize<'de> for Link {
	fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		let text = <Cow<'de, str>>::deserialize(deserializer)?;
		text.parse().map_err(serde::de::Error::custom)
	}
}

/// The SHA-256 of a DER certificate, written as 64 hex digits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Fingerprint([u8; 32]);

impl std::str::FromStr for Fingerprint {
	type Err = &'static str;
	fn from_str(text: &str) -> Result<Self, Self::Err> {
		const INVALID: &str = "expected a SHA-256 fingerprint: 64 hex digits";
		// Checked first: `from_str_radix` would also take a leading `+`.
		if text.len() != 64 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
			return Err(INVALID);
		}
		let mut bytes = [0; 32];
		for (byte, pair) in bytes.iter_mut().zip(text.as_bytes().as_chunks::<2>().0) {
			let pair = std::str::from_utf8(pair).map_err(|_| INVALID)?;
			*byte = u8::from_str_radix(pair, 16).map_err(|_| INVALID)?;
		}
		Ok(Self(bytes))
	}
}

impl std::fmt::Display for Fingerprint {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
	}
}

impl serde::Serialize for Fingerprint {
	fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		serializer.collect_str(self)
	}
}

impl<'de> serde::Deserialize<'de> for Fingerprint {
	fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		let text = <Cow<'de, str>>::deserialize(deserializer)?;
		text.parse().map_err(serde::de::Error::custom)
	}
}

/// A rule's cluster-link role: one of [`Link`], or `none` to grant no link where the
/// server-wide role would.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleLink {
	/// No cluster-link role.
	None,
	/// [`Link::Peer`].
	Peer,
	/// [`Link::Upstream`].
	Upstream,
}

impl From<RuleLink> for Option<Link> {
	fn from(link: RuleLink) -> Self {
		match link {
			RuleLink::None => None,
			RuleLink::Peer => Some(Link::Peer),
			RuleLink::Upstream => Some(Link::Upstream),
		}
	}
}

/// What sessions presenting one certificate, or any certificate issued under it, are
/// granted in place of the server-wide mTLS grant. Each field left `None` inherits
/// the server-wide value; patterns that are set replace the shared ones, never add to them.
///
/// `#[non_exhaustive]`, so start from [`Rule::default`] and set the fields.
#[serde_with::skip_serializing_none]
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Rule {
	/// Patterns it may publish, rooted at `/`.
	pub publish: Option<Patterns>,
	/// Patterns it may subscribe to, rooted at `/`.
	pub subscribe: Option<Patterns>,
	/// Its cluster-link role.
	pub link: Option<RuleLink>,
	/// The tier stamped on its grants.
	pub tier: Option<String>,
	/// The bitrate cap on what it publishes.
	pub publish_rate: Option<Rate>,
	/// The bitrate cap on what it subscribes to.
	pub subscribe_rate: Option<Rate>,
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
	/// Rules for particular certificates, keyed by the fingerprint of a certificate in a
	/// session's [`chain`](crate::Peer::chain). The rule naming the certificate nearest
	/// the leaf applies; a certificate no rule names gets [`mtls`](Self::mtls).
	pub certificates: BTreeMap<Fingerprint, Rule>,

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
	/// The key files as last parsed.
	cache: KeyCache,
}

/// Parsed key files, keyed by path and format, so a token costs a `stat` rather than a
/// read and parse.
#[derive(Clone, Default)]
struct KeyCache(Arc<Mutex<HashMap<(PathBuf, Format), Cached>>>);

/// How a key file is read.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Format {
	/// One key, as a set holding just it, which selects it by `kid` exactly as
	/// verifying with the key alone would.
	Key,
	/// A JWK Set.
	Set,
}

impl Format {
	fn parse(self, json: &str) -> Result<KeySet, Denial> {
		match self {
			Self::Key => {
				let key = json.parse::<Key>().map_err(|_| Denial::UnknownKey)?;
				Ok(KeySet {
					keys: vec![Arc::new(key)],
				})
			}
			Self::Set => json
				.parse()
				.map_err(|err: crate::Error| Denial::InvalidToken(err.to_string())),
		}
	}
}

struct Cached {
	stamp: (kio::clock::Wall, u64),
	keys: Arc<KeySet>,
}

impl std::fmt::Debug for KeyCache {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str("KeyCache")
	}
}

impl KeyCache {
	/// The keys in `path`, parsed again only when its modification time or size changed
	/// since they were cached. A file that is gone forgets its keys.
	async fn load(&self, path: &Path, format: Format) -> Result<Arc<KeySet>, Denial> {
		let lock = || self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
		let entry = (path.to_owned(), format);
		let Ok(meta) = tokio::fs::metadata(path).await else {
			lock().remove(&entry);
			return Err(Denial::UnknownKey);
		};
		// Stamped before reading, so a write racing the read is caught at the next token.
		let modified = meta.modified().ok().and_then(|modified| modified.try_into().ok());
		let stamp = modified.map(|modified| (modified, meta.len()));
		if let Some(cached) = lock().get(&entry)
			&& Some(cached.stamp) == stamp
		{
			return Ok(cached.keys.clone());
		}
		let json = tokio::fs::read_to_string(path).await.map_err(|_| Denial::UnknownKey)?;
		let keys = Arc::new(format.parse(&json)?);
		// A file without a modification time this clock holds parses every time.
		if let Some(stamp) = stamp {
			lock().insert(
				entry,
				Cached {
					stamp,
					keys: keys.clone(),
				},
			);
		}
		Ok(keys)
	}
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
	/// A client certificate was presented, but the policy grants it nothing.
	#[error("a certificate was presented but nothing is granted to it")]
	NoMtlsGrant,
	/// The certificate chain the relay reported holds something other than fingerprints.
	#[error("the certificate chain is not a list of SHA-256 fingerprints")]
	MalformedChain,
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
		let mut tier = self.tier.clone();
		let mut link = None;
		let (permissions, rates, expires, root) = if let Some(jwt) = jwt {
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
			let rates = (claims.publish_rate, claims.subscribe_rate);
			(permissions, rates, claims.expires, Some(claims.root))
		} else if let Some(peer) = &request.tls {
			let rule = self.rule(peer)?.cloned().unwrap_or_default();
			let rules = Permissions::new(
				rule.publish.unwrap_or_else(|| self.mtls.publish.clone()),
				rule.subscribe.unwrap_or_else(|| self.mtls.subscribe.clone()),
			);
			if rules.is_empty() {
				return Err(Denial::NoMtlsGrant);
			}
			tier = rule.tier.or(tier);
			link = rule.link.map_or(self.mtls_link, Into::into);
			let rates = (rule.publish_rate, rule.subscribe_rate);
			(authorize(&rules, &request.path)?, rates, peer.expires, None)
		} else {
			if self.public.is_empty() {
				return Err(Denial::NoPublicGrant);
			}
			(authorize(&self.public, &request.path)?, (None, None), None, None)
		};

		let mut grant = Grant::new(permissions.publish, permissions.subscribe);
		(grant.publish_rate, grant.subscribe_rate) = rates;
		grant.expires = match (expires, self.expires) {
			(Some(expires), _) => Some(expires),
			(None, Some(bound)) => Some(kio::clock::wall().checked_add(bound).ok_or(Denial::ExpiresOutOfRange)?),
			(None, None) => None,
		};
		grant.revalidate = self.revalidate;
		grant.tier = tier;
		grant.peer = link.is_some();
		grant.upstream = link == Some(Link::Upstream);
		Ok((grant, root))
	}

	/// The rule naming the certificate nearest the leaf, if any. A relay that predates
	/// [`chain`](crate::Peer::chain) reports none, which reads as the leaf alone.
	fn rule(&self, peer: &crate::Peer) -> Result<Option<&Rule>, Denial> {
		if self.certificates.is_empty() {
			return Ok(None);
		}
		let chain = match peer.chain.is_empty() {
			true => std::slice::from_ref(&peer.fingerprint),
			false => peer.chain.as_slice(),
		};
		for hash in chain {
			let fingerprint = hash.parse::<Fingerprint>().map_err(|_| Denial::MalformedChain)?;
			if let Some(rule) = self.certificates.get(&fingerprint) {
				return Ok(Some(rule));
			}
		}
		Ok(None)
	}

	async fn verify(&self, jwt: &str) -> Result<crate::Claims, Denial> {
		let keys = self.keys.as_ref().ok_or(Denial::NoKeys)?;
		// Every key source reads the header first, so a token that is not a JWT fails here
		// whichever one is configured.
		let header = jsonwebtoken::decode_header(jwt).map_err(|err| Denial::MalformedToken(err.to_string()))?;
		let set = match keys {
			Keys::File(path) => self.cache.load(path, Format::Key).await?,
			Keys::Dir(dir) => {
				let kid = header.kid.ok_or(Denial::MissingKeyId)?;
				let kid = KeyId::decode(&kid).map_err(|_| Denial::UnknownKey)?;
				self.cache.load(&dir.join(format!("{kid}.jwk")), Format::Key).await?
			}
			Keys::Set(path) => self.cache.load(path, Format::Set).await?,
		};
		let claims = set.key(jwt).and_then(|key| key.verify_claims(jwt));
		let claims = claims.map_err(|err| match err {
			crate::Error::Key(crate::KeyError::MissingKid) => Denial::MissingKeyId,
			crate::Error::Key(crate::KeyError::KeyNotFound(_)) => Denial::UnknownKey,
			other => Denial::InvalidToken(other.to_string()),
		})?;
		crate::key::validate_times(&claims, kio::clock::wall()).map_err(|err| match err {
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
	tokens: HashMap<Option<String>, Option<u64>>,
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
	/// it counts against that JWT's cap until its last credential ends.
	fn connect(&mut self, request: &Request, limits: Limits, root: Option<String>) -> Result<(), Denial> {
		// Decided before counting, so the credential is already known to be one JWT.
		let token = jwt(request).ok().flatten().as_deref().map(hash);
		let known = self.slots.get(&request.id);
		let new_token = token
			.filter(|token| !known.is_some_and(|slot| slot.tokens.values().any(|value| value.as_ref() == Some(token))));
		if let (Some(cap), Some(token)) = (limits.token, new_token)
			&& self
				.slots
				.values()
				.filter(|slot| slot.tokens.values().any(|value| *value == Some(token)))
				.count() >= cap
		{
			return Err(Denial::TokenLimit { root });
		}
		if let Some(slot) = self.slots.get_mut(&request.id) {
			slot.tokens.insert(request.auth.clone(), token);
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
				tokens: [(request.auth.clone(), token)].into(),
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
			tokens: HashMap::new(),
			remote: remote(request),
			seen: Instant::now(),
		});
		slot.tokens.insert(request.auth.clone(), token);
		slot.seen = Instant::now();
	}

	fn end(&mut self, request: &Request) {
		match &request.auth {
			Some(_) => {
				if let Some(slot) = self.slots.get_mut(&request.id) {
					slot.tokens.remove(&request.auth);
				}
			}
			None => {
				self.slots.remove(&request.id);
			}
		}
	}
}

/// A token's identity in the table, without holding the credential itself: its signing
/// input, so an ECDSA signature malleated into a second valid string counts as the same token.
fn hash(token: &str) -> u64 {
	use std::hash::{Hash, Hasher};
	let mut hasher = std::hash::DefaultHasher::new();
	token
		.rsplit_once('.')
		.map_or(token, |(input, _)| input)
		.hash(&mut hasher);
	hasher.finish()
}

/// The policy behind `POST /`, with the session table the limits need.
#[derive(Clone)]
pub struct Server {
	policy: Arc<std::sync::RwLock<Arc<Policy>>>,
	sessions: Arc<Mutex<Sessions>>,
	mesh: Option<crate::mesh::Coordinator>,
	secret: Option<Arc<str>>,
}

impl Server {
	/// A server answering with `policy`, refusing one that asks for a re-check without
	/// a bound, bounds grants past the clock's range, caps sessions without the
	/// re-check that ages out a dead relay's, or stamps an invalid tier.
	pub fn new(policy: Policy) -> crate::Result<Self> {
		Ok(Self {
			policy: Arc::new(std::sync::RwLock::new(Arc::new(Self::validate(policy)?))),
			sessions: Default::default(),
			mesh: None,
			secret: None,
		})
	}

	/// Answer with `policy` from now on, refusing it as [`new`](Self::new) would and
	/// keeping the current one then. Live sessions take it at their next re-check, and
	/// the session table carries over.
	pub fn update(&self, policy: Policy) -> crate::Result<()> {
		let policy = Arc::new(Self::validate(policy)?);
		*self.policy.write().unwrap_or_else(std::sync::PoisonError::into_inner) = policy;
		Ok(())
	}

	fn policy(&self) -> Arc<Policy> {
		self.policy
			.read()
			.unwrap_or_else(std::sync::PoisonError::into_inner)
			.clone()
	}

	fn validate(policy: Policy) -> crate::Result<Policy> {
		let tiers = policy.certificates.values().filter_map(|rule| rule.tier.as_ref());
		for tier in policy.tier.iter().chain(tiers) {
			crate::tier::validate(tier)?;
		}
		if policy.revalidate.is_some() && policy.expires.is_none() {
			return Err(crate::Error::UnboundedRevalidate);
		}
		if policy
			.expires
			.is_some_and(|bound| kio::clock::wall().checked_add(bound).is_none())
		{
			return Err(crate::Error::ExpiresOutOfRange);
		}
		let limited = policy.limits.token.is_some() || policy.limits.remote.is_some();
		if limited && policy.revalidate.is_none() {
			return Err(crate::Error::LimitsWithoutRevalidate);
		}
		Ok(policy)
	}

	/// Refuse with 401 every request whose HTTP Basic password is not `secret`, so only
	/// the relays can drive the session table. A relay sends it from its URL's userinfo,
	/// as in `https://relay:<secret>@auth.example/`.
	pub fn with_secret(mut self, secret: impl Into<Arc<str>>) -> Self {
		self.secret = Some(secret.into());
		self
	}

	/// Also answer the mesh owner's API under `/mesh` with `coordinator`, behind the same
	/// [`secret`](Self::with_secret) as `POST /`.
	pub fn with_mesh(mut self, coordinator: crate::mesh::Coordinator) -> Self {
		self.mesh = Some(coordinator);
		self
	}

	/// The cadence the session table ages out by, or `None` when no limit needs a table.
	fn cadence(policy: &Policy) -> Option<Duration> {
		let limits = policy.limits;
		(limits.token.is_some() || limits.remote.is_some())
			.then_some(policy.revalidate)
			.flatten()
	}

	/// Answer one event: the grant, or why the session is refused.
	pub async fn answer(&self, request: &Request) -> Result<Option<Grant>, Denial> {
		if matches!(request.event, Event::Attach) && request.auth.is_none() {
			return Err(Denial::MalformedToken("an attachment needs an auth id".into()));
		}
		let policy = self.policy();
		match request.event {
			Event::Connect | Event::Attach => {
				let (grant, root) = policy.decide_root(request).await?;
				if let Some(cadence) = Self::cadence(&policy) {
					let mut sessions = self.sessions.lock().unwrap();
					sessions.sweep(cadence);
					sessions.connect(request, policy.limits, root)?;
				}
				Ok(Some(grant))
			}
			Event::Revalidate => {
				let grant = policy.decide(request).await?;
				if let Some(cadence) = Self::cadence(&policy) {
					let mut sessions = self.sessions.lock().unwrap();
					sessions.sweep(cadence);
					sessions.revalidate(request);
				}
				Ok(Some(grant))
			}
			Event::End { .. } => {
				self.sessions.lock().unwrap().end(request);
				Ok(None)
			}
		}
	}

	/// The router: `POST /` reading a [`Request`] and writing a [`Grant`], plus the mesh
	/// owner's API when [`with_mesh`](Self::with_mesh) set one, all behind the
	/// [`secret`](Self::with_secret) when there is one.
	pub fn router(&self) -> Router {
		let mut router = Router::new().route("/", post(handle)).with_state(self.clone());
		if let Some(mesh) = &self.mesh {
			router = router.merge(mesh.admin_router());
		}
		match &self.secret {
			Some(secret) => router.layer(axum::middleware::from_fn_with_state(secret.clone(), guard)),
			None => router,
		}
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

/// Pass only a request whose HTTP Basic password is `secret`; the user name is ignored.
async fn guard(
	State(secret): State<Arc<str>>,
	request: axum::extract::Request,
	next: axum::middleware::Next,
) -> Response {
	use base64::Engine;
	let password = request
		.headers()
		.get(axum::http::header::AUTHORIZATION)
		.and_then(|value| value.to_str().ok())
		// RFC 7235 auth schemes are case-insensitive.
		.and_then(|value| value.split_once(' '))
		.filter(|(scheme, _)| scheme.eq_ignore_ascii_case("Basic"))
		.map(|(_, encoded)| encoded)
		.and_then(|encoded| base64::engine::general_purpose::STANDARD.decode(encoded.trim()).ok())
		.and_then(|decoded| {
			let colon = decoded.iter().position(|&byte| byte == b':')?;
			Some(decoded[colon + 1..].to_vec())
		});
	// Compare digests so the comparison does not end early on a length mismatch.
	let digest = |bytes: &[u8]| rind::digest::digest(&rind::digest::SHA256, bytes);
	match password {
		Some(password)
			if rind::constant_time::verify(digest(&password).as_ref(), digest(secret.as_bytes()).as_ref()).is_ok() =>
		{
			next.run(request).await
		}
		_ => {
			// A relay reads this 401 as a refusal, so its log alone cannot tell a wrong secret from a denied token.
			tracing::debug!(path = %request.uri().path(), presented = password.is_some(), "listen secret mismatch");
			StatusCode::UNAUTHORIZED.into_response()
		}
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
			(StatusCode::FORBIDDEN, Json(denial.body(server.policy().tier.clone()))).into_response()
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

	#[kio::test]
	async fn default_expiry_uses_the_ambient_wall() {
		let policy = Policy {
			public: rules(&["**"], &[]),
			expires: Some(Duration::from_secs(30)),
			..Default::default()
		};
		let grant = policy.decide(&request("/demo")).await.unwrap();
		assert_eq!(grant.expires, Some(kio::clock::wall() + Duration::from_secs(30)));
	}

	fn with_token(mut request: Request, jwt: &str) -> Request {
		request.query = Some(format!("jwt={jwt}"));
		request
	}

	fn with_peer(mut request: Request, expires: Option<kio::clock::Wall>) -> Request {
		request.tls = Some(Peer {
			name: "edge0".into(),
			fingerprint: "ab".repeat(32),
			chain: vec!["ab".repeat(32)],
			expires,
			issuer: "CN=cluster".into(),
		});
		request
	}

	/// A certificate session whose leaf is `leaf`, issued under each of `ancestors` in turn.
	fn with_chain(mut request: Request, leaf: &str, ancestors: &[&str]) -> Request {
		let chain: Vec<String> = std::iter::once(leaf)
			.chain(ancestors.iter().copied())
			.map(fingerprint)
			.collect();
		request.tls = Some(Peer {
			name: "affiliate".into(),
			fingerprint: chain[0].clone(),
			chain,
			expires: None,
			issuer: "CN=affiliates".into(),
		});
		request
	}

	/// A distinct fingerprint per name of up to 32 bytes: its bytes in hex, zero padded.
	fn fingerprint(name: &str) -> String {
		let hex: String = name.bytes().map(|byte| format!("{byte:02x}")).collect();
		format!("{hex:0<64}")
	}

	fn rule(publish: &[&str], subscribe: &[&str]) -> Rule {
		Rule {
			publish: Some(patterns(publish)),
			subscribe: Some(patterns(subscribe)),
			..Default::default()
		}
	}

	fn certificates(rules: impl IntoIterator<Item = (&'static str, Rule)>) -> BTreeMap<Fingerprint, Rule> {
		rules
			.into_iter()
			.map(|(name, rule)| (fingerprint(name).parse().unwrap(), rule))
			.collect()
	}

	/// A key on disk under its kid, the way `--key-dir` reads it.
	fn key_dir() -> (tempfile::TempDir, Key) {
		let dir = tempfile::tempdir().unwrap();
		let key = Key::generate(Algorithm::HS256, Some(KeyId::decode("kid1").unwrap())).unwrap();
		key.to_file(dir.path().join("kid1.jwk")).unwrap();
		(dir, key)
	}

	fn sign(key: &Key, root: &str, publish: &[&str], subscribe: &[&str], expires: Option<kio::clock::Wall>) -> String {
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
		let secs = kio::clock::wall()
			.checked_duration_since(kio::clock::Wall::UNIX_EPOCH)
			.unwrap()
			.as_secs()
			+ 600;
		let exp = kio::clock::Wall::UNIX_EPOCH + Duration::from_secs(secs);
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
	async fn the_secret_guards_every_route() {
		let key = Key::generate(Algorithm::HS256, None).unwrap();
		let config = crate::mesh::Config::new("https://mesh.example".parse().unwrap(), key);
		let server = Server::new(Policy {
			public: rules(&["**"], &[]),
			..Default::default()
		})
		.unwrap()
		.with_mesh(crate::mesh::Coordinator::new(config).unwrap())
		.with_secret("hunter2:x");
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let addr = listener.local_addr().unwrap();
		tokio::spawn(async move { server.serve(listener).await });

		let http = reqwest::Client::builder()
			.use_preconfigured_tls(crate::crypto::tls().unwrap())
			.build()
			.unwrap();
		for (userinfo, admitted) in [
			("", false),
			("relay:wrong@", false),
			("hunter2:x@", false),
			("relay:hunter2%3Ax@", true),
			(":hunter2%3Ax@", true),
		] {
			let status = |path: &'static str| {
				let url = format!("http://{userinfo}{addr}{path}");
				let http = http.clone();
				async move {
					match path {
						"/" => http.post(url).json(&request("/demo")).send().await,
						_ => http.get(url).send().await,
					}
					.unwrap()
					.status()
				}
			};
			for path in ["/", "/mesh/keys"] {
				assert_eq!(
					status(path).await == StatusCode::UNAUTHORIZED,
					!admitted,
					"{userinfo}{path}"
				);
			}
		}

		// The relay's client sends the secret from its URL's userinfo.
		let client = Client::new(format!("http://relay:hunter2%3Ax@{addr}/").parse().unwrap(), None).unwrap();
		client.connect(request("/demo")).await.unwrap();
		let client = Client::new(format!("http://{addr}/").parse().unwrap(), None).unwrap();
		client.connect(request("/demo")).await.unwrap_err();

		// The scheme matches case-insensitively.
		use base64::Engine;
		let credentials = base64::engine::general_purpose::STANDARD.encode("relay:hunter2:x");
		for scheme in ["basic", "BASIC", "Basic", "Bearer"] {
			let status = http
				.get(format!("http://{addr}/mesh/keys"))
				.header(reqwest::header::AUTHORIZATION, format!("{scheme} {credentials}"))
				.send()
				.await
				.unwrap()
				.status();
			assert_eq!(status == StatusCode::UNAUTHORIZED, scheme == "Bearer", "{scheme}");
		}
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
		let before = kio::clock::wall();
		let grant = policy.decide(&with_token(request("/demo"), &jwt)).await.unwrap();
		let expires = grant.expires.unwrap();
		assert!(expires >= before + Duration::from_secs(3600));
		assert!(expires <= kio::clock::wall() + Duration::from_secs(3600));
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
			Some(kio::clock::wall() - Duration::from_secs(1)),
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

	/// A key file is parsed once and re-read only when it changes, so rotating it still
	/// takes effect without a restart.
	#[tokio::test]
	async fn a_key_file_is_cached_until_it_changes() {
		let (dir, key) = key_dir();
		let path = dir.path().join("kid1.jwk");
		let policy = Policy {
			keys: Some(Keys::Dir(dir.path().into())),
			..Default::default()
		};
		let old = with_token(request("/demo"), &sign(&key, "demo", &["**"], &[], None));
		policy.decide(&old).await.unwrap();

		// Rewritten at the same size and modification time, the cached key still answers.
		let before = std::fs::metadata(&path).unwrap();
		let rotated = Key::generate(Algorithm::HS256, Some(KeyId::decode("kid1").unwrap())).unwrap();
		rotated.to_file(&path).unwrap();
		let file = std::fs::File::options().write(true).open(&path).unwrap();
		assert_eq!(file.metadata().unwrap().len(), before.len());
		file.set_modified(before.modified().unwrap()).unwrap();
		policy.decide(&old).await.unwrap();

		// A new modification time reads the rotated key.
		file.set_modified(before.modified().unwrap() + Duration::from_secs(1))
			.unwrap();
		assert!(matches!(policy.decide(&old).await, Err(Denial::InvalidToken(_))));
		let new = with_token(request("/demo"), &sign(&rotated, "demo", &["**"], &[], None));
		policy.decide(&new).await.unwrap();

		// A removed file forgets its key.
		std::fs::remove_file(&path).unwrap();
		assert_eq!(policy.decide(&new).await.unwrap_err(), Denial::UnknownKey);
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

		// The same file read as a single key is no key, whatever the set cached.
		let mut single = policy.clone();
		single.keys = Some(Keys::File(dir.path().join("keys.jwks")));
		assert_eq!(
			single.decide(&with_token(request("/demo"), &jwt)).await.unwrap_err(),
			Denial::UnknownKey
		);

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
		let not_after = kio::clock::wall() + Duration::from_secs(86400 * 30);
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

	/// A rule scopes a leaf, or every leaf under an intermediate; the one nearest the
	/// leaf wins, and a certificate no rule names falls back to the shared grant.
	#[tokio::test]
	async fn certificate_rules_scope_by_the_nearest_named_certificate() {
		let policy = Policy {
			mtls: rules(&["shared/**"], &[]),
			mtls_link: Some(Link::Peer),
			tier: Some("base".into()),
			certificates: certificates([
				(
					"affiliate-ca",
					Rule {
						tier: Some("affiliate".into()),
						link: Some(RuleLink::None),
						publish_rate: Some(Rate::from_bps(1_000_000).unwrap()),
						..rule(&["affiliate/**"], &["affiliate/**"])
					},
				),
				(
					"vip",
					Rule {
						subscribe: Some(patterns(&["**"])),
						..Default::default()
					},
				),
			]),
			..Default::default()
		};

		// An intermediate rule covers a leaf issued under it, replacing the shared patterns.
		let grant = policy
			.decide(&with_chain(request("/"), "alice", &["affiliate-ca", "root"]))
			.await
			.unwrap();
		assert_eq!(grant.publish, patterns(&["affiliate/**"]));
		assert_eq!(grant.subscribe, patterns(&["affiliate/**"]));
		assert_eq!(grant.tier.as_deref(), Some("affiliate"));
		assert_eq!(grant.publish_rate, Rate::from_bps(1_000_000).ok());
		assert!(!grant.peer, "the rule drops the shared link role");

		// A leaf rule beats its intermediate's, and what it omits inherits the server-wide
		// values, not the intermediate rule's.
		let grant = policy
			.decide(&with_chain(request("/"), "vip", &["affiliate-ca", "root"]))
			.await
			.unwrap();
		assert_eq!(grant.publish, patterns(&["shared/**"]));
		assert_eq!(grant.subscribe, patterns(&["**"]));
		assert_eq!(grant.tier.as_deref(), Some("base"));
		assert_eq!(grant.publish_rate, None);
		assert!(grant.peer);

		// No rule names the chain: the shared grant.
		let grant = policy
			.decide(&with_chain(request("/"), "bob", &["other-ca"]))
			.await
			.unwrap();
		assert_eq!(grant.publish, patterns(&["shared/**"]));
		assert!(grant.subscribe.is_empty());

		// A relay that predates the chain reports only the leaf, which a leaf rule matches.
		let mut legacy = with_chain(request("/"), "vip", &[]);
		legacy.tls.as_mut().unwrap().chain.clear();
		let grant = policy.decide(&legacy).await.unwrap();
		assert_eq!(grant.subscribe, patterns(&["**"]));

		// A rule that grants nothing refuses, even where the shared grant would admit.
		let revoked = Policy {
			certificates: certificates([("affiliate-ca", rule(&[], &[]))]),
			..policy.clone()
		};
		assert_eq!(
			revoked
				.decide(&with_chain(request("/"), "alice", &["affiliate-ca"]))
				.await
				.unwrap_err(),
			Denial::NoMtlsGrant
		);

		let mut malformed = with_chain(request("/"), "alice", &[]);
		malformed.tls.as_mut().unwrap().chain.push("not hex".into());
		assert_eq!(policy.decide(&malformed).await.unwrap_err(), Denial::MalformedChain);
	}

	/// An update narrows or revokes a live session at its next re-check, and one the
	/// server would refuse leaves the current policy answering.
	#[tokio::test]
	async fn an_update_applies_at_the_next_recheck_and_a_bad_one_is_refused() {
		let provisioned = Policy {
			certificates: certificates([("alice", rule(&["**"], &["**"]))]),
			..Default::default()
		};
		let server = Server::new(provisioned.clone()).unwrap();
		let mut session = with_chain(request("/"), "alice", &["ca"]);
		assert_eq!(
			server.answer(&session).await.unwrap().unwrap().publish,
			patterns(&["**"])
		);

		session.event = Event::Revalidate;
		server
			.update(Policy {
				certificates: certificates([("alice", rule(&["alice/**"], &[]))]),
				..Default::default()
			})
			.unwrap();
		assert_eq!(
			server.answer(&session).await.unwrap().unwrap().publish,
			patterns(&["alice/**"])
		);

		let invalid = Policy {
			certificates: certificates([(
				"alice",
				Rule {
					tier: Some("not a tier!".into()),
					..rule(&["**"], &["**"])
				},
			)]),
			..Default::default()
		};
		assert!(server.update(invalid).is_err());
		assert_eq!(
			server.answer(&session).await.unwrap().unwrap().publish,
			patterns(&["alice/**"])
		);

		server.update(Policy::default()).unwrap();
		assert_eq!(server.answer(&session).await.unwrap_err(), Denial::NoMtlsGrant);
	}

	#[test]
	fn fingerprints_are_64_hex_digits() {
		let text = "AB".repeat(32);
		let parsed: Fingerprint = text.parse().unwrap();
		assert_eq!(parsed.to_string(), "ab".repeat(32));
		for bad in [
			"ab".repeat(31),
			"ab".repeat(33),
			"zz".repeat(32),
			"é".repeat(32),
			"+1".repeat(32),
		] {
			assert!(bad.parse::<Fingerprint>().is_err(), "{bad}");
		}
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
			Some(kio::clock::wall() + Duration::from_secs(60)),
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

	/// An ECDSA signature `(r, s)` has a twin `(r, n - s)` that verifies the same
	/// claims, so the cap must count the signing input, not the compact string.
	#[tokio::test]
	async fn a_malleated_signature_shares_its_tokens_cap() {
		use base64::Engine;
		const N: [u8; 32] = [
			0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xbc, 0xe6,
			0xfa, 0xad, 0xa7, 0x17, 0x9e, 0x84, 0xf3, 0xb9, 0xca, 0xc2, 0xfc, 0x63, 0x25, 0x51,
		];
		let dir = tempfile::tempdir().unwrap();
		let key = Key::generate(Algorithm::ES256, Some(KeyId::decode("kid1").unwrap())).unwrap();
		key.to_file(dir.path().join("kid1.jwk")).unwrap();
		let server = Server::new(Policy {
			keys: Some(Keys::Dir(dir.path().into())),
			limits: Limits {
				token: Some(1),
				remote: None,
			},
			..limited()
		})
		.unwrap();
		let jwt = sign(&key, "demo", &["**"], &[], None);

		let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
		let (input, signature) = jwt.rsplit_once('.').unwrap();
		let mut signature = b64.decode(signature).unwrap();
		let mut borrow = 0i16;
		for i in (0..32).rev() {
			let diff = N[i] as i16 - signature[32 + i] as i16 - borrow;
			signature[32 + i] = diff.rem_euclid(256) as u8;
			borrow = (diff < 0) as i16;
		}
		let twin = format!("{input}.{}", b64.encode(signature));
		assert_ne!(twin, jwt);

		server.answer(&with_token(request("/demo"), &jwt)).await.unwrap();
		let mut second = with_token(request("/demo"), &twin);
		second.id = "second".into();
		assert_eq!(
			server.answer(&second).await.unwrap_err(),
			Denial::TokenLimit {
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
		let exp = Some(kio::clock::wall() + Duration::from_secs(60));
		let a = sign(&key, "demo", &["**"], &[], None);
		let b = sign(&key, "demo", &["**"], &[], exp);
		let c = sign(
			&key,
			"demo",
			&["**"],
			&[],
			Some(kio::clock::wall() + Duration::from_secs(61)),
		);

		let mut first = with_token(request("/demo"), &a);
		first.id = "first".into();
		let mut second = with_token(request("/demo"), &c);
		second.id = "second".into();
		server.answer(&first).await.unwrap();
		server.answer(&second).await.unwrap();

		let attach = |session: &Request| {
			let mut attach = with_token(session.clone(), &b);
			attach.auth = Some("attachment".into());
			attach.event = Event::Attach;
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
		let mut duplicate = attach(&first);
		duplicate.auth = Some("duplicate".into());
		server.answer(&duplicate).await.unwrap();
		let end = |mut request: Request| {
			request.event = Event::End {
				reason: crate::lease::Reason::Dropped,
				duration: Duration::ZERO,
				bytes: Default::default(),
			};
			request
		};
		server.answer(&end(attach(&first))).await.unwrap();
		assert!(matches!(
			server.answer(&attach(&second)).await,
			Err(Denial::TokenLimit { .. })
		));
		server.answer(&end(duplicate)).await.unwrap();
		server.answer(&attach(&second)).await.unwrap();
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
		let expired = Some(kio::clock::Wall::UNIX_EPOCH + Duration::from_secs(1));
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
			Some(kio::clock::Wall::UNIX_EPOCH + Duration::from_secs(1)),
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
