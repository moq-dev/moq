//! Tokens we sign against the verifier moq-relay 0.14 shipped (`moq-token` 0.7).
//!
//! A grant of subtrees is written as legacy `put`/`get` prefixes, which 0.14 reads
//! with the same scope. Anything a prefix can't say is written as
//! `publish`/`subscribe`, which 0.14 reads as granting nothing and refuses, rather
//! than misreading an exact grant as a prefix.

use moq_auth::{Algorithm, Claims, Key};

fn patterns(texts: &[&str]) -> moq_auth::Access {
	texts.iter().map(|text| text.parse().unwrap()).collect()
}

/// A key pair both crates hold: ours signs, the released one verifies.
fn keys() -> (Key, moq_token::Key) {
	let key = Key::generate(Algorithm::HS256, None).unwrap();
	let released = moq_token::Key::from_str(&key.to_str().unwrap()).unwrap();
	(key, released)
}

#[test]
fn subtree_grants_verify_on_0_14_with_the_same_scope() {
	let (key, released) = keys();
	let claims = Claims::default()
		.with_root("room")
		.with_publish(patterns(&["alice/**"]))
		.with_subscribe(patterns(&["**"]));
	let verified = released.verify(&key.sign(&claims).unwrap()).unwrap();
	assert_eq!(verified.root, "room");
	assert_eq!(verified.publish, ["alice"]);
	assert_eq!(verified.subscribe, [""]);
}

#[test]
fn exact_grants_are_refused_by_0_14() {
	let (key, released) = keys();
	for claims in [
		Claims::default().with_root("room").with_publish(patterns(&["alice"])),
		Claims::default().with_subscribe(patterns(&["*/chat"])),
		// One exact grant moves the whole document to patterns.
		Claims::default()
			.with_publish(patterns(&["alice/**"]))
			.with_subscribe(patterns(&["bob"])),
	] {
		assert!(released.verify(&key.sign(&claims).unwrap()).is_err(), "{claims:?}");
	}
}

#[test]
fn capped_tokens_are_refused_by_published_auth() {
	let key = Key::generate(Algorithm::HS256, None).unwrap();
	let released = moq_auth_published::Key::from_str(&key.to_str().unwrap()).unwrap();
	let mut claims = Claims::default().with_publish(patterns(&["alice/**"]));
	claims.publish.rate = Some(moq_auth::Rate::from_bps(8_000_000).unwrap());
	let token = key.sign(&claims).unwrap();
	assert!(released.verify(&token).is_err());
	let legacy = moq_token::Key::from_str(&key.to_str().unwrap()).unwrap();
	assert!(legacy.verify(&token).is_err());
	let grant = moq_auth::Grant::new(claims.publish, claims.subscribe);
	assert!(serde_json::from_value::<moq_auth_published::Grant>(serde_json::to_value(grant).unwrap()).is_err());
}
