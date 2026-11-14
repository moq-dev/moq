//! Test support, enabled by the `fuzz` feature: the `token` and `jwk` fuzz target bodies, plus
//! their seeds.
//!
//! These are the parsers `moq auth serve` runs on what any client sends: the JWT in its query,
//! and the key or key set the token selects. The bodies live here rather than in
//! `fuzz/fuzz_targets/` because they check through private parts (verifying without the clock),
//! and so the tests below replay them on stable.
//!
//! It tracks the crate's internals, so it can change in any release. See `fuzz/README.md` for
//! the workflow.

use std::str::FromStr;
use std::sync::{Arc, OnceLock};

use serde_json::Value;

use crate::{Claims, Key, KeyOperation, KeySet, Pattern, Rate};

/// One fuzz target body: it returns whether the input parsed.
pub type Target = fn(&[u8]) -> bool;

/// Every target, keyed by the name of its `fuzz_targets/<name>.rs` shim.
pub const TARGETS: &[(&str, Target)] = &[("token", token), ("jwk", jwk)];

/// The signing keys, fixed so a committed input replays: one per kind of material.
const KEYS: &[&str] = &[
	r#"{"alg":"HS256","key_ops":["sign","verify"],"kty":"oct","k":"Mayc-1UbXngAVLAyPJljKTrOA7rwVTS_Im_LRXYvDzE","kid":"fuzz-hs256"}"#,
	r#"{"alg":"EdDSA","key_ops":["verify","sign"],"kty":"OKP","crv":"Ed25519","x":"GDjNqZlC0ZyXp2Lb8c_eT-oDp4UhTUYc_6q8HMaWauE","d":"_cHIr6X1GEQ6qc-9GUjSDTtlcmTnsOctwUkvE28ZDMw","kid":"fuzz-eddsa"}"#,
	r#"{"alg":"ES256","key_ops":["verify","sign"],"kty":"EC","crv":"P-256","x":"MTm-tdpUeGdRtA4lS-SYQSs2VtinHFTfB8Wi2WnzSqw","y":"m0kSg8wde8t0aoeK5fiikAsyQtDUJsL30sZgE5Vcsd8","d":"lT-UkeuPDxJ8Cm35e6yfeehclkJptYGwvC3Iv-SmAZc","kid":"fuzz-es256"}"#,
	r#"{"alg":"RS256","key_ops":["sign","verify"],"kty":"RSA","n":"p2nsYkS2yzIvhdcez-PS9Sew7aN7fZlGwbyOJrzzfhYKj1lcKEnlc1UCnTvIuqVqAMZ2H2W0flT7dP6oYETN0mDxJMTrccAUcB9GvMIc0774y5mXSNMDD2cF7M6m7FDYo1xFk8GMUhIFJXewknY9g_SVy2OUIgEET6hqGJmhgfiw9o6WVKAKAgewHjUi2KPJcvKYwUMTLvMPnIpsGZmJBH3CU85RnkmYEg91ZJsDuJXlPv9WOrWMmeo9kkj_dFElwANxhA5-Mlzzmzz6up-skzonpMknYaCUsruYI3GheenRMWIu6Blzm-acgDgsJjwkwN_xW9qBSdfMAqW1mcmaKw","e":"AQAB","d":"C7NrCpk732rUCHo3FJebRHgwNQPCm04ttFe95bY6HHtk5Iz91HEJwOxVoPCvQGBUTtaQJfShT10YMB6mXQgiDIyq_gNnY01mAEonUwUMJ76GvBAxkyzwX1C3iMwOvaCO66gE0uVprcjFpE8_v5TVCtZ667q7fT7t8rVWKP7_20oN-AP5ei0MjdNAZlIcyMZsAbyzKkOxVUqkov33mNaERlUqgZr7Y6yADtEXq6T08K4JPRfuWFkl35DYZSPbmfGZIj630YTa7-u6FpABLvWSmqP58qXAVYicMut4v4p6wbjHqgg5ShjmxZm7yrSj8YUw5yjSXNx6jaVvJp_iOi1AgQ","p":"2NeGjrclLhNQXA464tFAaFu5hZ5HeNq9D5hg6hQXIyos_gl7xzKzO49rzdO09QomDGO0aLsshlXcZIOwB1DwTdFI3fHVjbOYoBd1uxq98QMJvDMbKYDeQyl-DekMr0Y5LeYwAibOQtpBO2d8-KB8XAl57_ATeegdDWaFxGT0Fqs","q":"xaVdLVwdvCqj3cwLLbtBd35MgG4Fmf6XFVu8C0vIlpi0e14p6N03TxtugTZ9rgN5Nowk7TQAZCXHJn3_kXP8tr5A2LFXyuwa0GkkWJS-2rWhXdsowM2ewpHzFhcA-MkbgRJ5jh-1KdqgzWm_DJ1a0x7AtloKoagYM9V-NRu2ioE","dp":"HDrS1bmaKYlyKMnheK3tSKjIWlDk1SMFAgbAdzjuWjGfceMiLii3lMVzL3SkVlJf6bUBQ7E6drKu4pcST6GF51ubtOlK-Autjwn-1cEBgWL33Xx5yMlyJiQ8uOriL99gik0xatZKfAGWBHnQlz2tcv341ffXUUOapKf4Oj06Wck","dq":"f5qr9cUrTXkaIvDBptzm_4cDTUe7Rhkg4iE_3usmrlQY8i1xUW7Bbsl_Jt1PdzG26IjCroKMXhwl36wpfHcuCa37n-ufitu01CmN1cLzFbF-ZWJBhXk7rtMb5n8jmXv9ze9nm2LNTCBjATvspPBS20tXkGPByW0FczG7Mf6adAE","qi":"utxsd7Bxcmjyp16suwnTtZWXTPtxwrL8dpA5lgFia3QjLWX3AC7JUEsoH10RN2a1-pW7HnqjYE__xw6P2AePWJCrF9gXTR_5GDXu-_2vceqlGqflt7UgyWkXWC4__34UVezRjkNBa0BexXDhLR3c3vC_9KHUx5IFqCBqeTUcMzI","kid":"fuzz-rs256"}"#
];

/// The keys behind [`KEYS`].
fn keys() -> &'static [Key] {
	static KEYS_: OnceLock<Vec<Key>> = OnceLock::new();
	KEYS_.get_or_init(|| KEYS.iter().map(|jwk| jwk.parse().expect("a fixed key parses")).collect())
}

/// What the keys signed, as the targets compare it: the claims, encoded.
fn claims() -> Vec<Claims> {
	let pattern = |text: &str| text.parse::<Pattern>().expect("a fixed pattern parses");
	let at = |secs| kio::clock::Wall::UNIX_EPOCH + std::time::Duration::from_secs(secs);
	vec![
		Claims::default().with_root("demo").with_publish([pattern("alice/**")]),
		Claims::default()
			.with_root("room/123")
			.with_publish([pattern("alice")])
			.with_subscribe([pattern("**")])
			.with_subscribe_rate(Rate::from_bps(1_000_000).expect("a fixed rate"))
			.with_expires(at(4_000_000_000))
			.with_issued(at(1_700_000_000)),
	]
}

/// A token a key in [`KEYS`] signed.
struct Issued {
	key: usize,
	token: String,
	claims: Value,
}

impl Issued {
	/// The part of the token its signature covers.
	fn signed(&self) -> &str {
		signed(&self.token).expect("an issued token has a signature")
	}
}

/// Everything [`KEYS`] signed: each key over each of [`claims`].
fn issued() -> &'static [Issued] {
	static ISSUED: OnceLock<Vec<Issued>> = OnceLock::new();
	ISSUED.get_or_init(|| {
		let mut issued = Vec::new();
		for (key, signer) in keys().iter().enumerate() {
			for claims in claims() {
				let token = signer.sign(&claims).expect("a fixed key signs");
				issued.push(Issued {
					key,
					token,
					claims: encode(&claims),
				});
			}
		}
		issued
	})
}

/// The verify-only halves of [`KEYS`] as one set, as `moq auth serve` loads a JWK Set.
fn set() -> &'static KeySet {
	static SET: OnceLock<KeySet> = OnceLock::new();
	SET.get_or_init(|| KeySet {
		keys: keys()
			.iter()
			.map(|key| Arc::new(key.to_public().unwrap_or_else(|_| key.clone())))
			.collect(),
	})
}

fn encode<T: serde::Serialize>(value: &T) -> Value {
	serde_json::to_value(value).expect("encodes")
}

/// The header and payload of `token`, which its signature covers.
fn signed(token: &str) -> Option<&str> {
	token.rsplit_once('.').map(|(signed, _)| signed)
}

/// A JWT, or with a leading odd byte, a JWT payload.
///
/// - A token verifies only under a key that signed its header and payload, to the claims it
///   signed, whether checked against each key or picked from a set by its `kid` the way
///   `moq auth serve` does.
/// - Claims that decode encode to claims that decode back the same, and a token signed over
///   them verifies to the same claims again.
pub fn token(data: &[u8]) -> bool {
	let Some((&selector, rest)) = data.split_first() else {
		return false;
	};
	let Ok(text) = std::str::from_utf8(rest) else {
		return false;
	};
	match selector & 1 {
		0 => verify(text),
		_ => payload(text),
	}
}

fn verify(token: &str) -> bool {
	let check = |key: usize, claims: &Claims| {
		let issued = issued()
			.iter()
			.find(|issued| issued.key == key && Some(issued.signed()) == signed(token))
			.unwrap_or_else(|| panic!("key {key} verified a token it never signed: {token}"));
		assert_eq!(encode(claims), issued.claims, "{token} verified to other claims");
	};
	let mut verified = false;
	for (index, key) in keys().iter().enumerate() {
		if let Ok(claims) = key.verify_claims(token) {
			check(index, &claims);
			verified = true;
		}
	}
	if let Ok(key) = set().key(token)
		&& let Ok(claims) = key.verify_claims(token)
	{
		let index = set().keys.iter().position(|k| Arc::ptr_eq(k, &key)).expect("from the set");
		check(index, &claims);
		assert!(verified, "the set verified {token}, which its key alone refused");
	}
	verified
}

fn payload(json: &str) -> bool {
	let Ok(claims) = serde_json::from_str::<Claims>(json) else {
		return false;
	};
	let encoded = serde_json::to_string(&claims).expect("decoded claims encode");
	let again: Claims = serde_json::from_str(&encoded).unwrap_or_else(|err| panic!("{encoded} does not decode: {err}"));
	assert_eq!(encode(&again), encode(&claims), "{json} encoded as {encoded}");

	// Signing is stricter than verifying (a root with an empty segment, say), so only what it
	// takes goes through a token.
	let signer = &keys()[0];
	if let Ok(token) = signer.sign(&claims) {
		let verified = signer
			.verify_claims(&token)
			.unwrap_or_else(|err| panic!("{token} for {json} does not verify: {err}"));
		assert_eq!(encode(&verified), encode(&claims), "{json} came back from {token}");
	}
	true
}

/// A JWK, or with a leading odd byte, a JWK Set.
///
/// - A key that parses encodes to a key that parses back the same.
/// - It verifies a token [`KEYS`] signed only when it holds the signer's key material.
/// - What it signs, its public half verifies, to the same claims.
pub fn jwk(data: &[u8]) -> bool {
	let Some((&selector, rest)) = data.split_first() else {
		return false;
	};
	let Ok(text) = std::str::from_utf8(rest) else {
		return false;
	};
	match selector & 1 {
		0 => match Key::from_str(text) {
			Ok(key) => {
				check_key(&key);
				true
			}
			Err(_) => false,
		},
		_ => match KeySet::from_str(text) {
			Ok(set) => {
				for key in &set.keys {
					check_key(key);
				}
				for issued in issued() {
					if let Ok(key) = set.key(&issued.token)
						&& key.verify_claims(&issued.token).is_ok()
					{
						assert_eq!(material(&key), material(&keys()[issued.key]), "{text} verified {}", issued.token);
					}
				}
				true
			}
			Err(_) => false,
		},
	}
}

/// The key material that decides what verifies: the public half, or an `oct` key's secret.
fn material(key: &Key) -> Value {
	let jwk = encode(key);
	let fields = ["kty", "crv", "x", "y", "n", "e", "k"];
	fields.iter().map(|&field| jwk.get(field).cloned().unwrap_or(Value::Null)).collect()
}

/// A key as JSON, with `key_ops` sorted: it is a set, so its order varies.
fn encode_key(key: &Key) -> Value {
	let mut jwk = encode(key);
	if let Some(Value::Array(operations)) = jwk.get_mut("key_ops") {
		operations.sort_by_key(|operation| operation.to_string());
	}
	jwk
}

fn check_key(key: &Key) {
	let text = key.to_str().expect("a parsed key encodes");
	let again: Key = text.parse().unwrap_or_else(|err| panic!("{text} does not parse: {err}"));
	assert_eq!(encode_key(&again), encode_key(key));

	for issued in issued() {
		if key.verify_claims(&issued.token).is_ok() {
			assert_eq!(material(key), material(&keys()[issued.key]), "{key:?} verified {}", issued.token);
		}
	}

	if !key.operations.contains(&KeyOperation::Sign) {
		return;
	}
	let verifier = key.to_public().unwrap_or_else(|_| key.clone());
	for claims in claims() {
		let Ok(token) = key.sign(&claims) else { continue };
		let verified = verifier
			.verify_claims(&token)
			.unwrap_or_else(|err| panic!("{key:?} signed {token}, which its public half refuses: {err}"));
		assert_eq!(encode(&verified), encode(&claims));
	}
}

/// One input to start a target from.
pub struct Seed {
	/// The target it feeds, a key of [`TARGETS`].
	pub target: &'static str,
	/// The input, selector byte included.
	pub data: Vec<u8>,
}

/// The starting corpus: every token [`KEYS`] signed and a few altered ones, every payload,
/// key, and set in the corpus `js/auth` shares, and [`KEYS`] themselves alone and as sets.
pub fn seeds() -> Vec<Seed> {
	let mut seeds = Vec::new();
	let mut push = |target, selector: u8, text: &str| {
		let mut data = vec![selector];
		data.extend_from_slice(text.as_bytes());
		seeds.push(Seed { target, data });
	};

	for issued in issued() {
		push("token", 0, &issued.token);
		// The same token, unsigned and with `alg: none`.
		let (header, rest) = issued.token.split_once('.').unwrap();
		let (payload, _) = rest.split_once('.').unwrap();
		push("token", 0, &format!("{header}.{payload}."));
		push("token", 0, &format!("eyJhbGciOiJub25lIn0.{payload}."));
		push("token", 1, &issued.claims.to_string());
	}

	let corpus: Value = serde_json::from_str(include_str!("../tests/corpus.json")).expect("the corpus parses");
	for case in corpus["tokens"].as_array().expect("tokens") {
		if let Some(token) = case["token"].as_str() {
			push("token", 0, token);
		}
		if !case["payload"].is_null() {
			push("token", 1, &case["payload"].to_string());
		}
	}
	for jwk in corpus["keys"].as_object().expect("keys").values() {
		push("jwk", 0, &jwk.to_string());
	}
	for case in corpus["jwk"].as_array().expect("jwk cases") {
		push("jwk", 0, &case["jwk"].to_string());
	}
	for case in corpus["sets"].as_array().expect("set cases") {
		push("jwk", 1, &case["jwks"].to_string());
	}
	for key in KEYS {
		push("jwk", 0, key);
	}
	push("jwk", 1, &format!("{{\"keys\":[{}]}}", KEYS.join(",")));
	seeds
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The fixed keys and issued tokens pass their own checks, so the targets start sound.
	#[test]
	fn seeds_pass() {
		for seed in seeds() {
			let (_, target) = TARGETS.iter().find(|(name, _)| *name == seed.target).unwrap();
			target(&seed.data);
		}
		for issued in issued() {
			assert!(token(&[&[0], issued.token.as_bytes()].concat()), "{} refused", issued.token);
		}
		for key in KEYS {
			assert!(jwk(&[&[0], key.as_bytes()].concat()));
		}
	}

	/// Replay the inputs committed under `fuzz/regressions/`, so a bug the fuzzer found stays
	/// fixed on the stable toolchain, in the suite CI already runs.
	#[test]
	fn regressions() {
		let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fuzz/regressions");

		for (name, target) in TARGETS {
			// Nothing has failed yet for this target; `fuzz/README.md` says what to drop in here
			// when something does.
			let Ok(entries) = std::fs::read_dir(root.join(name)) else {
				continue;
			};
			for entry in entries {
				let path = entry.expect("could not read the regression directory").path();
				let data = std::fs::read(&path).expect("could not read a regression input");
				target(&data);
			}
		}
	}
}
