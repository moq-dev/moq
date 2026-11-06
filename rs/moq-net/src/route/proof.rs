//! Route key proofs: an endpoint shows its peer that it holds a route key's secret.
//!
//! A proof is an Ed25519 signature over a secret both ends export from the session's
//! handshake and the prover's side of the session, so it neither replays on another
//! session nor reflects back to the peer that sent it.

use std::{fmt, sync::Arc};

use rind::signature::{Ed25519KeyPair, PublicKey, verify::ED25519};
use zeroize::Zeroizing;

/// The label every transport binding exports the session secret under.
pub(crate) const EXPORTER_LABEL: &[u8] = b"EXPORTER-moq-lite-route-key";

/// Bytes of exported secret a proof signs.
const SECRET_LEN: usize = 32;

/// Separates a route proof from anything else the same key signs.
const DOMAIN: &[u8] = b"moq-lite route key";

const KEY_LEN: usize = 32;
const SIGNATURE_LEN: usize = 64;

/// The PKCS#8 v1 (RFC 8410) encoding of an Ed25519 key, up to its 32-byte seed. rind loads a
/// seed alone only this way.
const PKCS8_SEED_PREFIX: [u8; 16] = [
	0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
];

/// The Ed25519 key an origin proves to its peers, so they file its routes under it.
///
/// Relays sharing one key present an origin under the same identity whichever of them
/// carries it, so a scoped peer reached through either fails over as one. Cheap to clone;
/// its `Debug` shows only the public key.
#[derive(Clone)]
pub struct RouteKey(Arc<Ed25519KeyPair>);

impl RouteKey {
	/// Generate a fresh key from the system's secure random source.
	pub fn generate() -> Self {
		Self(Arc::new(Ed25519KeyPair::generate().expect("system randomness")))
	}

	/// The key with this 32-byte Ed25519 seed.
	pub fn from_seed(seed: &[u8; KEY_LEN]) -> Self {
		let mut der = Zeroizing::new([0u8; PKCS8_SEED_PREFIX.len() + KEY_LEN]);
		der[..PKCS8_SEED_PREFIX.len()].copy_from_slice(&PKCS8_SEED_PREFIX);
		der[PKCS8_SEED_PREFIX.len()..].copy_from_slice(seed);
		Self(Arc::new(
			Ed25519KeyPair::from_pkcs8(der.as_ref()).expect("a PKCS#8 v1 Ed25519 key"),
		))
	}

	fn public(&self) -> [u8; KEY_LEN] {
		self.0
			.public_key()
			.as_ref()
			.try_into()
			.expect("a 32-byte Ed25519 public key")
	}
}

impl fmt::Debug for RouteKey {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_tuple("RouteKey").field(&Hex(&self.public())).finish()
	}
}

/// A proof that the sender holds a route key, as it travels in SETUP: the public key, then
/// its signature.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Proof {
	pub key: [u8; KEY_LEN],
	pub signature: [u8; SIGNATURE_LEN],
}

impl Proof {
	/// Its encoded size; any other is malformed.
	#[cfg(test)]
	pub const LEN: usize = KEY_LEN + SIGNATURE_LEN;

	pub fn to_bytes(self) -> Vec<u8> {
		[self.key.as_slice(), self.signature.as_slice()].concat()
	}

	/// Decode a proof, or `None` when `bytes` is not exactly [`Self::LEN`] long.
	pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
		let (key, signature) = bytes.split_first_chunk::<KEY_LEN>()?;
		Some(Self {
			key: *key,
			signature: signature.try_into().ok()?,
		})
	}
}

impl fmt::Debug for Proof {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Proof")
			.field("key", &Hex(&self.key))
			.finish_non_exhaustive()
	}
}

/// What a proof on one session signs: the secret both ends export from its handshake.
pub(crate) struct Transcript(Zeroizing<[u8; SECRET_LEN]>);

impl Transcript {
	pub fn new(secret: [u8; SECRET_LEN]) -> Self {
		Self(Zeroizing::new(secret))
	}

	/// The signed message: the domain, the prover's side (0 dialed, 1 accepted), and the secret.
	fn message(&self, client: bool) -> Zeroizing<Vec<u8>> {
		Zeroizing::new([DOMAIN, &[u8::from(!client)], self.0.as_slice()].concat())
	}

	/// Prove `key` as the side of this session `client` names.
	pub fn prove(&self, key: &RouteKey, client: bool) -> Proof {
		Proof {
			key: key.public(),
			signature: key
				.0
				.sign(&self.message(client))
				.as_ref()
				.try_into()
				.expect("a 64-byte Ed25519 signature"),
		}
	}

	/// Whether `proof` shows its key held by the side of this session `client` names.
	pub fn verify(&self, proof: &Proof, client: bool) -> bool {
		PublicKey::new(&ED25519, &proof.key)
			.is_ok_and(|key| key.verify(&self.message(client), &proof.signature).is_ok())
	}
}

struct Hex<'a>(&'a [u8]);

impl fmt::Debug for Hex<'_> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Shared with `js/net/src/lite/route_key.test.ts`, so both sides sign the same transcript.
	const CLIENT_PROOF: &str = concat!(
		"8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c",
		"e7ea4e34a65b8501185b805d09b58a61b5b8495654fcaac0949d4f98aa35002e",
		"6fca096329973e91e1105030dd88fa0eb543c68c00e18394f8300cb0574cf901",
	);

	#[test]
	fn a_proof_matches_the_js_transcript() {
		let proof = Transcript::new([2; 32]).prove(&RouteKey::from_seed(&[1; 32]), true);
		assert_eq!(format!("{:?}", Hex(&proof.to_bytes())), CLIENT_PROOF);
	}

	#[test]
	fn a_proof_holds_only_for_its_side_of_its_session() {
		let transcript = Transcript::new([2; 32]);
		let proof = transcript.prove(&RouteKey::generate(), true);
		assert!(transcript.verify(&proof, true));
		assert!(!transcript.verify(&proof, false), "reflected to its sender");
		assert!(
			!Transcript::new([3; 32]).verify(&proof, true),
			"replayed on another session"
		);

		let other = RouteKey::generate().public();
		assert!(
			!transcript.verify(&Proof { key: other, ..proof }, true),
			"claimed for another key"
		);
	}

	#[test]
	fn a_proof_decodes_only_at_its_size() {
		let proof = Transcript::new([2; 32]).prove(&RouteKey::generate(), false);
		assert_eq!(Proof::from_bytes(&proof.to_bytes()), Some(proof));
		assert_eq!(Proof::from_bytes(&[0; Proof::LEN - 1]), None);
		assert_eq!(Proof::from_bytes(&[0; Proof::LEN + 1]), None);
	}
}
