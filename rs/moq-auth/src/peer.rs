//! Peer grants: the relay's short-lived statement of one session's scope, bound to the
//! session's presenter key, that another client verifies with the relay's public keys.
//!
//! A peer grant is not a [`Claims`] token. Each refuses the other's JSON: [`Claims`]
//! rejects the `cnf` claim, and a grant requires it, so presenting a grant as `?jwt=`
//! cannot widen a relay session. Only ES256 and EdDSA keys sign or verify one, so a
//! relay holding only HMAC keys issues none and never has to hand out a secret.

use crate::error::KeyError;
use crate::{Algorithm, Claims, Jwk, Key, KeyMaterial, KeyOperation, KeySet};
use jsonwebtoken::Header;
use moq_pattern::Patterns;
use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use std::sync::Arc;

/// The algorithms that sign and verify a peer grant.
const PEER_ALGORITHMS: [Algorithm; 2] = [Algorithm::ES256, Algorithm::EdDSA];

/// A session's publish and subscribe patterns, bound to its presenter key and signed by the relay.
///
/// The presenter key is the public half of a key the client minted and announced to the relay.
/// A grant names no routing identity: it authorizes paths and binds a key, nothing more. A
/// receiver checks the relay's signature with [`KeySet::verify_peer`], then that the grant
/// [`binds`](Self::binds) the key the peer's roster entry names and its session proved.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PeerGrant {
	/// The root for the publish/subscribe patterns below.
	pub root: String,

	/// Patterns the presenter may publish, relative to the root.
	pub publish: Patterns,

	/// Patterns the presenter may subscribe to, relative to the root.
	pub subscribe: Patterns,

	/// The presenter's public key (`cnf`), which a direct session must prove it holds.
	pub presenter: Key,

	/// When the relay issued the grant (`iat`).
	pub issued: kio::clock::Wall,

	/// When the grant lapses (`exp`).
	pub expires: kio::clock::Wall,
}

impl PeerGrant {
	/// A grant bound to `presenter`, valid from `issued` until `expires`, granting nothing yet.
	///
	/// Fails unless `presenter` is a public asymmetric key.
	pub fn new(presenter: Key, issued: kio::clock::Wall, expires: kio::clock::Wall) -> crate::Result<Self> {
		validate_presenter(&presenter)?;
		Ok(Self {
			root: String::new(),
			publish: Patterns::default(),
			subscribe: Patterns::default(),
			presenter,
			issued,
			expires,
		})
	}

	/// Set the root that the publish/subscribe patterns are relative to.
	pub fn with_root(mut self, root: impl Into<String>) -> Self {
		self.root = root.into();
		self
	}

	/// Set the patterns the presenter may publish.
	pub fn with_publish(mut self, patterns: impl IntoIterator<Item = moq_pattern::Pattern>) -> Self {
		self.publish = patterns.into_iter().collect();
		self
	}

	/// Set the patterns the presenter may subscribe to.
	pub fn with_subscribe(mut self, patterns: impl IntoIterator<Item = moq_pattern::Pattern>) -> Self {
		self.subscribe = patterns.into_iter().collect();
		self
	}

	/// Whether the grant is bound to `key`: the same algorithm and public key material.
	///
	/// `kid`, `key_ops`, and private material are ignored, so a roster entry's public JWK and
	/// the presenter's own key pair both match.
	pub fn binds(&self, key: &Jwk) -> bool {
		self.presenter.algorithm == key.algorithm && same_public(&self.presenter.material, &key.material)
	}

	/// The grant as [`Claims`], so a key's scope caps a peer grant as it caps a token.
	fn as_claims(&self) -> Claims {
		let mut claims = Claims::default().with_root(self.root.clone());
		claims.publish = self.publish.clone();
		claims.subscribe = self.subscribe.clone();
		claims.with_issued(self.issued).with_expires(self.expires)
	}

	fn validate(&self) -> crate::Result<()> {
		validate_presenter(&self.presenter)?;
		self.as_claims().validate()
	}
}

/// Whether two keys share their public half.
fn same_public(a: &KeyMaterial, b: &KeyMaterial) -> bool {
	match (a, b) {
		(
			KeyMaterial::EC {
				curve: ca,
				x: xa,
				y: ya,
				..
			},
			KeyMaterial::EC {
				curve: cb,
				x: xb,
				y: yb,
				..
			},
		) => ca == cb && xa == xb && ya == yb,
		(KeyMaterial::OKP { curve: ca, x: xa, .. }, KeyMaterial::OKP { curve: cb, x: xb, .. }) => ca == cb && xa == xb,
		(KeyMaterial::RSA { public: a, .. }, KeyMaterial::RSA { public: b, .. }) => a.n == b.n && a.e == b.e,
		_ => false,
	}
}

fn validate_presenter(key: &Key) -> crate::Result<()> {
	if matches!(key.material, KeyMaterial::OCT { .. }) {
		return Err(KeyError::NoPublicKey.into());
	}
	if key.is_private() {
		return Err(crate::Error::PrivatePresenter);
	}
	Ok(())
}

fn is_peer_key(key: &Key) -> bool {
	PEER_ALGORITHMS.contains(&key.algorithm) && !matches!(key.material, KeyMaterial::OCT { .. })
}

impl Key {
	/// Sign a peer grant with this key, which must be ES256 or EdDSA.
	pub fn sign_peer(&self, grant: &PeerGrant) -> crate::Result<String> {
		if !is_peer_key(self) {
			return Err(crate::Error::NoPeerKey);
		}
		if !self.operations.contains(&KeyOperation::Sign) {
			return Err(KeyError::SignUnsupported.into());
		}

		crate::path::validate(&grant.root)?;
		grant.validate()?;
		self.validate_scope(&grant.as_claims())?;

		let mut header = Header::new(self.algorithm.into());
		header.kid = self.kid.as_ref().map(|k| k.to_string());
		Ok(jsonwebtoken::encode(
			&header,
			&wire::PeerGrant::from(grant),
			self.to_encoding_key()?,
		)?)
	}

	/// Verify a peer grant's signature, claims, expiry, and this key's scope.
	///
	/// Refused unless this key is ES256 or EdDSA, so an HMAC-signed grant is never one.
	/// Binding the presenter key to a session is the caller's: see [`PeerGrant::binds`].
	pub fn verify_peer(&self, token: &str) -> crate::Result<PeerGrant> {
		if !is_peer_key(self) {
			return Err(crate::Error::NoPeerKey);
		}
		self.check_kid(token)?;

		let mut validation = jsonwebtoken::Validation::new(self.algorithm.into());
		validation.required_spec_claims = Default::default();
		validation.validate_exp = false;
		validation.validate_nbf = false;
		validation.validate_aud = false;
		let wire: wire::PeerGrant = jsonwebtoken::decode(token, self.to_decoding_key()?, &validation)?.claims;
		let grant = PeerGrant::try_from(wire)?;
		grant.validate()?;
		self.validate_scope(&grant.as_claims())?;

		if grant.expires <= kio::clock::wall() {
			return Err(crate::Error::TokenExpired);
		}
		Ok(grant)
	}
}

impl KeySet {
	/// The public keys a client verifies peer grants with: ES256 and EdDSA keys only.
	///
	/// HMAC keys are skipped, never exported, so the result is safe to hand to any client.
	pub fn to_peer_set(&self) -> crate::Result<KeySet> {
		Ok(KeySet {
			keys: self
				.keys
				.iter()
				.filter(|key| is_peer_key(key) && key.operations.contains(&KeyOperation::Verify))
				.map(|key| key.to_public().map(Arc::new))
				.collect::<Result<_, _>>()?,
		})
	}

	/// Sign a peer grant with the first ES256 or EdDSA key that can sign.
	///
	/// Fails with [`Error::NoPeerKey`](crate::Error::NoPeerKey) when the set holds none, such as
	/// a set of HMAC keys: such a relay issues no peer grants.
	pub fn sign_peer(&self, grant: &PeerGrant) -> crate::Result<String> {
		let key = self
			.keys
			.iter()
			.find(|key| is_peer_key(key) && key.operations.contains(&KeyOperation::Sign) && key.is_private())
			.ok_or(crate::Error::NoPeerKey)?;
		key.sign_peer(grant)
	}

	/// Verify a peer grant with the key matching its `kid` header.
	pub fn verify_peer(&self, token: &str) -> crate::Result<PeerGrant> {
		self.key(token)?.verify_peer(token)
	}
}

impl TryFrom<wire::PeerGrant> for PeerGrant {
	type Error = crate::Error;

	fn try_from(wire: wire::PeerGrant) -> crate::Result<Self> {
		Ok(Self {
			root: wire.root,
			publish: wire.publish,
			subscribe: wire.subscribe,
			presenter: wire.cnf.jwk.import()?,
			issued: wire.iat,
			expires: wire.exp,
		})
	}
}

mod wire {
	use super::*;

	/// The JWT payload of a [`PeerGrant`](super::PeerGrant).
	///
	/// Every field outside this list is refused, as [`Claims`](crate::Claims) does, and `cnf`,
	/// `iat`, and `exp` are required, so no relay token parses as a peer grant.
	#[serde_as]
	#[derive(Serialize, Deserialize)]
	#[serde(deny_unknown_fields)]
	pub(super) struct PeerGrant {
		#[serde(default, skip_serializing_if = "String::is_empty")]
		pub root: String,
		#[serde(default, skip_serializing_if = "Patterns::is_empty")]
		pub publish: Patterns,
		#[serde(default, skip_serializing_if = "Patterns::is_empty")]
		pub subscribe: Patterns,
		/// RFC 7800 confirmation: the presenter's public key.
		pub cnf: Confirmation,
		#[serde_as(as = "kio::clock::serde::UnixSeconds")]
		pub iat: kio::clock::Wall,
		#[serde_as(as = "kio::clock::serde::UnixSeconds")]
		pub exp: kio::clock::Wall,
	}

	#[derive(Serialize, Deserialize)]
	#[serde(deny_unknown_fields)]
	pub(super) struct Confirmation {
		pub jwk: Jwk,
	}

	impl From<&super::PeerGrant> for PeerGrant {
		fn from(grant: &super::PeerGrant) -> Self {
			Self {
				root: grant.root.clone(),
				publish: grant.publish.clone(),
				subscribe: grant.subscribe.clone(),
				cnf: Confirmation {
					jwk: grant.presenter.export(),
				},
				iat: grant.issued,
				exp: grant.expires,
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::time::Duration;

	fn presenter() -> Key {
		Key::generate(Algorithm::EdDSA, None).unwrap()
	}

	fn grant(presenter: &Key) -> PeerGrant {
		let now = kio::clock::wall();
		PeerGrant::new(presenter.to_public().unwrap(), now, now + Duration::from_secs(60))
			.unwrap()
			.with_root("room")
			.with_publish(["alice/**".parse::<moq_pattern::Pattern>().unwrap()])
			.with_subscribe(["**".parse::<moq_pattern::Pattern>().unwrap()])
	}

	fn relay(algorithm: Algorithm) -> KeySet {
		let kid = crate::KeyId::decode("relay").unwrap();
		KeySet {
			keys: vec![Arc::new(Key::generate(algorithm, Some(kid)).unwrap())],
		}
	}

	#[test]
	fn round_trip() {
		for algorithm in PEER_ALGORITHMS {
			let relay = relay(algorithm);
			let presenter = presenter();
			let token = relay.sign_peer(&grant(&presenter)).unwrap();

			let peers = relay.to_peer_set().unwrap();
			let verified = peers.verify_peer(&token).unwrap();
			assert_eq!(verified.root, "room");
			assert_eq!(verified.publish, grant(&presenter).publish);
			assert_eq!(verified.subscribe, grant(&presenter).subscribe);
			assert!(verified.binds(&presenter.export()));
			assert!(verified.binds(&presenter.to_public().unwrap().export()));
		}
	}

	#[test]
	fn hmac_relay_issues_nothing() {
		let relay = relay(Algorithm::HS256);
		assert!(matches!(
			relay.sign_peer(&grant(&presenter())),
			Err(crate::Error::NoPeerKey)
		));
		assert!(relay.to_peer_set().unwrap().keys.is_empty());

		let key = &relay.keys[0];
		assert!(matches!(
			key.sign_peer(&grant(&presenter())),
			Err(crate::Error::NoPeerKey)
		));
	}

	#[test]
	fn peer_set_drops_hmac_and_private_material() {
		let mut relay = relay(Algorithm::ES256);
		relay
			.keys
			.push(Arc::new(Key::generate(Algorithm::HS256, None).unwrap()));
		relay
			.keys
			.push(Arc::new(Key::generate(Algorithm::RS256, None).unwrap()));

		let peers = relay.to_peer_set().unwrap();
		assert_eq!(peers.keys.len(), 1);
		assert_eq!(peers.keys[0].algorithm, Algorithm::ES256);
		assert!(!peers.keys[0].is_private());
		assert!(!peers.to_str().unwrap().contains("\"k\""));
	}

	#[test]
	fn hmac_signed_grant_is_refused() {
		// Forge a grant with an HMAC key that shares the asymmetric key's kid.
		let relay = relay(Algorithm::ES256);
		let wire = wire::PeerGrant::from(&grant(&presenter()));
		let mut header = Header::new(jsonwebtoken::Algorithm::HS256);
		header.kid = Some("relay".into());
		let secret = [7u8; 32];
		let token = jsonwebtoken::encode(&header, &wire, &jsonwebtoken::EncodingKey::from_secret(&secret)).unwrap();

		assert!(relay.to_peer_set().unwrap().verify_peer(&token).is_err());

		// Even a receiver holding that HMAC key refuses it.
		let hmac = Jwk::new(
			Algorithm::HS256,
			KeyMaterial::OCT {
				secret: secret.to_vec(),
			},
		);
		let mut hmac = hmac;
		hmac.kid = Some(crate::KeyId::decode("relay").unwrap());
		assert!(matches!(
			hmac.import().unwrap().verify_peer(&token),
			Err(crate::Error::NoPeerKey)
		));
	}

	#[test]
	fn grant_is_not_claims() {
		let relay = relay(Algorithm::ES256);
		let token = relay.sign_peer(&grant(&presenter())).unwrap();
		assert!(relay.verify(&token).is_err());
		assert!(relay.to_public_set().unwrap().verify(&token).is_err());

		// Nor does a relay token pass as a peer grant.
		let claims = Claims::default().with_subscribe(["**".parse().unwrap()]);
		let token = relay.sign(&claims).unwrap();
		assert!(relay.verify_peer(&token).is_err());
	}

	#[test]
	fn expired_grant_is_refused() {
		let relay = relay(Algorithm::EdDSA);
		let presenter = presenter();
		let now = kio::clock::wall();
		let mut expired = grant(&presenter);
		expired.issued = now.checked_sub(Duration::from_secs(120)).unwrap();
		expired.expires = now.checked_sub(Duration::from_secs(60)).unwrap();
		let token = relay.sign_peer(&expired).unwrap();
		assert!(matches!(relay.verify_peer(&token), Err(crate::Error::TokenExpired)));
	}

	#[test]
	fn other_key_is_not_bound() {
		let relay = relay(Algorithm::ES256);
		let token = relay.sign_peer(&grant(&presenter())).unwrap();
		let verified = relay.verify_peer(&token).unwrap();

		assert!(!verified.binds(&presenter().export()));
		assert!(!verified.binds(&Key::generate(Algorithm::ES256, None).unwrap().export()));
	}

	#[test]
	fn presenter_must_be_public_and_asymmetric() {
		let now = kio::clock::wall();
		let later = now + Duration::from_secs(60);
		assert!(PeerGrant::new(presenter(), now, later).is_err());
		assert!(PeerGrant::new(Key::generate(Algorithm::HS256, None).unwrap(), now, later).is_err());
	}

	#[test]
	fn scope_caps_a_grant() {
		let scope = crate::Scope {
			root: "room".into(),
			publish: Patterns::default(),
			subscribe: ["**".parse::<moq_pattern::Pattern>().unwrap()].into_iter().collect(),
		};
		let key = Key::generate(Algorithm::ES256, None)
			.unwrap()
			.with_scope(scope)
			.unwrap();
		assert!(matches!(
			key.sign_peer(&grant(&presenter())),
			Err(crate::Error::ScopeExceeded)
		));
	}

	#[test]
	fn useless_grant_is_refused() {
		let relay = relay(Algorithm::ES256);
		let now = kio::clock::wall();
		let empty = PeerGrant::new(presenter().to_public().unwrap(), now, now + Duration::from_secs(60)).unwrap();
		assert!(relay.sign_peer(&empty).is_err());
	}
}
