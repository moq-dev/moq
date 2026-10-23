//! Unactivated identity proposal. Only the adversarial tests compile this module.
//! Transport binding and coverage of authority-changing controls remain design decisions.
use std::sync::OnceLock;

use p256::ecdsa::{
	Signature, SigningKey, VerifyingKey,
	signature::{Signer, Verifier},
};
use p256::elliptic_curve::Generate;
use sha2::{Digest, Sha256};

const ID_DOMAIN: &[u8] = b"moq-routing-identity-v1\0";
const PROOF_DOMAIN: &[u8] = b"moq-lite-07-wip identity-proof-v1\0";
const KEY_LEN: usize = 65;
const NONCE_LEN: usize = 32;
const SIG_LEN: usize = 64;
const CNF: u8 = 2;
const PROOF: u8 = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Identity([u8; 32]);

struct Key(SigningKey);

impl Key {
	fn generate() -> Self {
		Self(SigningKey::generate())
	}
	fn process() -> &'static Self {
		static KEY: OnceLock<Key> = OnceLock::new();
		KEY.get_or_init(Self::generate)
	}
	fn public(&self) -> [u8; KEY_LEN] {
		self.0.verifying_key().to_sec1_point(false).as_ref().try_into().unwrap()
	}
	fn sign(&self, message: &[u8]) -> [u8; SIG_LEN] {
		let signature: Signature = self.0.sign(message);
		signature.to_bytes().into()
	}
}

fn identity(public: &[u8; KEY_LEN]) -> Result<Identity, &'static str> {
	// Uncompressed SEC1 is the one public-key encoding in this proposal.
	if public[0] != 4 {
		return Err("noncanonical public key");
	}
	VerifyingKey::from_sec1_bytes(public).map_err(|_| "invalid public key")?;
	let mut hash = Sha256::new();
	hash.update(ID_DOMAIN);
	hash.update(public);
	Ok(Identity(hash.finalize().into()))
}

#[derive(Clone)]
struct Cnf {
	public: [u8; KEY_LEN],
	nonce: [u8; NONCE_LEN],
}

impl Cnf {
	fn encode(&self) -> Vec<u8> {
		// Both values fit the current lite-07 one-byte leading-ones varint.
		let mut bytes = vec![CNF, (KEY_LEN + NONCE_LEN) as u8];
		bytes.extend(self.public);
		bytes.extend(self.nonce);
		bytes
	}
	fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
		if bytes.len() != 2 + KEY_LEN + NONCE_LEN || bytes[..2] != [CNF, (KEY_LEN + NONCE_LEN) as u8] {
			return Err("fixed AUTH_CNF length or type");
		}
		let public = bytes[2..2 + KEY_LEN].try_into().unwrap();
		identity(&public)?;
		Ok(Self {
			public,
			nonce: bytes[2 + KEY_LEN..].try_into().unwrap(),
		})
	}
}

fn encode_proof(signature: [u8; SIG_LEN]) -> Vec<u8> {
	let mut bytes = vec![PROOF, SIG_LEN as u8];
	bytes.extend(signature);
	bytes
}

fn decode_proof(bytes: &[u8]) -> Result<[u8; SIG_LEN], &'static str> {
	if bytes.len() != 2 + SIG_LEN || bytes[..2] != [PROOF, SIG_LEN as u8] {
		return Err("fixed AUTH_PROOF length or type");
	}
	let signature: [u8; SIG_LEN] = bytes[2..].try_into().unwrap();
	Signature::from_slice(&signature).map_err(|_| "invalid signature scalars")?;
	Ok(signature)
}

fn transcript(client: &Cnf, server: &Cnf, role: u8, binding: Option<[u8; 32]>) -> Vec<u8> {
	let mut bytes = PROOF_DOMAIN.to_vec();
	bytes.push(role);
	bytes.push(u8::from(binding.is_some()));
	bytes.extend(client.public);
	bytes.extend(client.nonce);
	bytes.extend(server.public);
	bytes.extend(server.nonce);
	if let Some(binding) = binding {
		bytes.extend(binding);
	}
	bytes
}

fn verify(public: &[u8; KEY_LEN], message: &[u8], signature: [u8; SIG_LEN]) -> Result<(), &'static str> {
	let key = VerifyingKey::from_sec1_bytes(public).map_err(|_| "invalid public key")?;
	let signature = Signature::from_slice(&signature).map_err(|_| "invalid signature")?;
	key.verify(message, &signature).map_err(|_| "invalid proof")
}

struct Slot {
	local: Cnf,
	role: u8,
	binding: Option<[u8; 32]>,
	peer: Option<Cnf>,
	checking: bool,
	ended: bool,
	verified: Option<Identity>,
	attempts: usize,
}

impl Slot {
	fn new(local: Cnf, role: u8, binding: Option<[u8; 32]>) -> Self {
		Self {
			local,
			role,
			binding,
			peer: None,
			checking: false,
			ended: false,
			verified: None,
			attempts: 0,
		}
	}
	fn begin(&mut self, cnf: &[u8]) -> Result<(), &'static str> {
		if self.checking || self.ended {
			return Err("duplicate session proof");
		}
		self.checking = true; // Reserve before decoding or importing the peer's curve point.
		self.attempts += 1;
		match Cnf::decode(cnf) {
			Ok(peer) => {
				self.peer = Some(peer);
				Ok(())
			}
			Err(err) => {
				self.ended = true;
				Err(err)
			}
		}
	}
	fn finish(&mut self, claimed: &Identity, signature: [u8; SIG_LEN]) -> Result<(), &'static str> {
		if !self.checking || self.ended {
			return Err("proof out of order");
		}
		self.ended = true; // Token renewal cannot reopen a successful or failed slot.
		let peer = self.peer.as_ref().ok_or("no peer CNF")?;
		let derived = identity(&peer.public)?;
		if &derived != claimed {
			return Err("claimed identity differs from public key");
		}
		let message = if self.role == 0 {
			transcript(peer, &self.local, 0, self.binding)
		} else {
			transcript(&self.local, peer, 1, self.binding)
		};
		verify(&peer.public, &message, signature)?;
		self.verified = Some(derived);
		Ok(())
	}
	fn owns(&self, claimed: &Identity) -> bool {
		self.verified.as_ref() == Some(claimed)
	}
}

fn hex(bytes: &[u8]) -> String {
	bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn unhex(value: &str) -> Vec<u8> {
	value
		.as_bytes()
		.as_chunks::<2>()
		.0
		.iter()
		.map(|part| u8::from_str_radix(std::str::from_utf8(part).unwrap(), 16).unwrap())
		.collect()
}
fn vectors() -> serde_json::Value {
	serde_json::from_str(include_str!("../../../../test/interop/identity/vectors.json")).unwrap()
}
fn fixture_key(number: usize) -> Key {
	let value = vectors();
	Key(SigningKey::from_slice(&unhex(value["keys"][number]["private"].as_str().unwrap())).unwrap())
}
fn hello(key: &Key, nonce: u8) -> Cnf {
	Cnf {
		public: key.public(),
		nonce: [nonce; NONCE_LEN],
	}
}

#[test]
fn process_key_is_shared_and_restart_gets_a_new_full_width_identity() {
	let a = identity(&Key::process().public()).unwrap();
	assert_eq!(a, identity(&Key::process().public()).unwrap());
	assert_ne!(a, identity(&Key::generate().public()).unwrap());
	assert_eq!(a.0.len(), 32); // It is never truncated to the legacy 53/62-bit Hop.
}

#[test]
fn webcrypto_and_rust_share_key_identity_message_and_signature_encodings() {
	let value = vectors();
	let client = hello(&fixture_key(0), 1);
	let server = hello(&fixture_key(1), 2);
	assert_eq!(hex(&client.public), value["keys"][0]["public"].as_str().unwrap());
	assert_eq!(
		hex(&identity(&client.public).unwrap().0),
		value["identity"].as_str().unwrap()
	);
	assert_eq!(hex(&client.encode()), value["cnf"].as_str().unwrap());
	let message = transcript(&client, &server, 0, Some([3; 32]));
	assert_eq!(hex(&message), value["message"].as_str().unwrap());
	let browser_signature = unhex(value["webcrypto_signature"].as_str().unwrap())
		.try_into()
		.unwrap();
	verify(&client.public, &message, browser_signature).unwrap();
	let rust_signature = fixture_key(0).sign(&message);
	// RustCrypto uses deterministic ECDSA; WebCrypto verifies this shared vector too.
	assert_eq!(hex(&rust_signature), value["rust_signature"].as_str().unwrap());
	assert_eq!(decode_proof(&encode_proof(rust_signature)).unwrap(), rust_signature);
}

#[test]
fn replay_reflection_binding_and_key_swaps_are_refused_including_shared_static_keys() {
	let a = fixture_key(0);
	for b in [fixture_key(0), fixture_key(1)] {
		let client = hello(&a, 1);
		let server = hello(&b, 2);
		let message = transcript(&client, &server, 0, Some([3; 32]));
		let signature = a.sign(&message);
		verify(&client.public, &message, signature).unwrap();
		assert!(
			verify(
				&client.public,
				&transcript(&hello(&a, 4), &server, 0, Some([3; 32])),
				signature
			)
			.is_err()
		);
		assert!(
			verify(
				&client.public,
				&transcript(&client, &server, 1, Some([3; 32])),
				signature
			)
			.is_err()
		);
		assert!(
			verify(
				&client.public,
				&transcript(&client, &server, 0, Some([4; 32])),
				signature
			)
			.is_err()
		);
		assert!(verify(&fixture_key(1).public(), &message, signature).is_err());
	}
}

#[test]
fn ownership_requires_one_verified_proof_and_cannot_be_changed_by_a_later_auth_stream() {
	let key = fixture_key(0);
	let other = identity(&fixture_key(1).public()).unwrap();
	let owned = identity(&key.public()).unwrap();
	let message = transcript(&hello(&key, 1), &hello(&key, 2), 0, Some([3; 32]));
	let mut slot = Slot::new(hello(&key, 2), 0, Some([3; 32]));
	assert!(!slot.owns(&owned));
	slot.begin(&hello(&key, 1).encode()).unwrap();
	assert!(slot.begin(&hello(&key, 1).encode()).is_err()); // Reserved before an asynchronous verifier could yield.
	slot.finish(&owned, key.sign(&message)).unwrap();
	assert!(slot.owns(&owned));
	assert!(!slot.owns(&other));
	assert!(slot.begin(&hello(&key, 1).encode()).is_err());
	assert_eq!(slot.attempts, 1);
	let mut forger = Slot::new(hello(&key, 2), 0, Some([3; 32]));
	forger.begin(&hello(&key, 1).encode()).unwrap();
	assert!(forger.finish(&other, key.sign(&message)).is_err());
	assert!(forger.begin(&hello(&key, 1).encode()).is_err());
	assert!(!forger.owns(&other));
}

#[test]
fn malformed_fixed_fields_are_refused_and_a_failed_slot_cannot_restart() {
	let good = hello(&fixture_key(0), 1).encode();
	Cnf::decode(&good).unwrap();
	for size in 0..good.len() {
		assert!(Cnf::decode(&good[..size]).is_err());
	}
	let mut extra = good.clone();
	extra.push(0);
	assert!(Cnf::decode(&extra).is_err());
	for (offset, byte) in [(0, 3), (1, 255), (2, 2)] {
		let mut bad = good.clone();
		bad[offset] = byte;
		assert!(Cnf::decode(&bad).is_err());
	}
	let mut point = good;
	point[3..67].fill(0);
	assert!(Cnf::decode(&point).is_err());
	assert!(decode_proof(&encode_proof([0; SIG_LEN])).is_err());
	assert!(decode_proof(&[PROOF, 255]).is_err());
	let mut slot = Slot::new(hello(&fixture_key(0), 2), 0, None);
	assert!(
		slot.finish(&identity(&fixture_key(0).public()).unwrap(), [0; SIG_LEN])
			.is_err()
	);
	assert!(slot.begin(&[CNF, 255]).is_err());
	assert!(slot.begin(&hello(&fixture_key(0), 1).encode()).is_err());
	assert_eq!(slot.attempts, 1);
}

#[test]
fn a_keyless_live_relay_defeats_nonce_only_ownership_but_not_transport_binding() {
	let a = fixture_key(0);
	let b = fixture_key(1);
	let client = hello(&a, 1);
	let server = hello(&b, 2);
	// M forwards both CNFs and both role-specific signatures unchanged between A-M and M-B.
	// Fresh nonces and both peer keys therefore agree at A and B despite different TLS sessions.
	let unsigned_channel = transcript(&client, &server, 0, None);
	let relayed_signature = a.sign(&unsigned_channel);
	let mut b_slot = Slot::new(server.clone(), 0, None);
	b_slot.begin(&client.encode()).unwrap();
	b_slot
		.finish(&identity(&client.public).unwrap(), relayed_signature)
		.unwrap();
	let mut a_slot = Slot::new(client.clone(), 1, None);
	a_slot.begin(&server.encode()).unwrap();
	a_slot
		.finish(
			&identity(&server.public).unwrap(),
			b.sign(&transcript(&client, &server, 1, None)),
		)
		.unwrap();
	assert!(b_slot.owns(&identity(&a.public()).unwrap()));
	assert!(a_slot.owns(&identity(&b.public()).unwrap()));
	let for_a_m = transcript(&client, &server, 0, Some([10; 32]));
	let mut bound = Slot::new(server.clone(), 0, Some([11; 32]));
	bound.begin(&client.encode()).unwrap();
	assert!(
		bound
			.finish(&identity(&client.public).unwrap(), a.sign(&for_a_m))
			.is_err()
	);
	// Even a correctly signed transcript from another local challenge is not caller-substitutable.
	let mut replay = Slot::new(hello(&b, 9), 0, Some([10; 32]));
	replay.begin(&client.encode()).unwrap();
	assert!(
		replay
			.finish(&identity(&client.public).unwrap(), a.sign(&for_a_m))
			.is_err()
	);
}
