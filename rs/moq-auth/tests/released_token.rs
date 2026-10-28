//! Tokens we sign against the verifier moq-relay 0.14 shipped (`moq-token` 0.7).
//!
//! A grant of subtrees is written as legacy `put`/`get` prefixes, which 0.14 reads
//! with the same scope. Anything a prefix can't say is written as
//! `publish`/`subscribe`, which 0.14 reads as granting nothing and refuses, rather
//! than misreading an exact grant as a prefix.

use base64::Engine;
use moq_auth::{Algorithm, Claims, Key};

fn patterns(texts: &[&str]) -> moq_auth::Patterns {
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
fn patterns_keep_the_published_auth_wire_shape() {
	let key = Key::generate(Algorithm::HS256, None).unwrap();
	let released = moq_auth_published::Key::from_str(&key.to_str().unwrap()).unwrap();
	for publish in [["alice/**", "bob/**"], ["alice", "*/chat"]] {
		let claims = Claims::default().with_root("room").with_publish(patterns(&publish));
		let ours = key.sign(&claims).unwrap();
		let verified = released.verify(&ours).unwrap();
		assert_eq!(
			serde_json::to_value(&verified).unwrap(),
			serde_json::to_value(&claims).unwrap()
		);
		let theirs = released.sign(&verified).unwrap();
		assert_eq!(
			serde_json::to_value(key.verify(&theirs).unwrap()).unwrap(),
			serde_json::to_value(&claims).unwrap()
		);
		let grant = moq_auth::Grant::new(claims.publish, claims.subscribe);
		let wire = serde_json::to_value(&grant).unwrap();
		let published: moq_auth_published::Grant = serde_json::from_value(wire.clone()).unwrap();
		assert_eq!(serde_json::to_value(published).unwrap(), wire);
	}
}

#[test]
fn capped_objects_are_refused_in_full_by_current_and_published_auth() {
	let key = Key::generate(Algorithm::HS256, None).unwrap();
	let published = moq_auth_published::Key::from_str(&key.to_str().unwrap()).unwrap();
	let legacy = moq_token::Key::from_str(&key.to_str().unwrap()).unwrap();
	let jwk: serde_json::Value = serde_json::to_value(&key).unwrap();
	let secret = base64::engine::general_purpose::URL_SAFE_NO_PAD
		.decode(jwk["k"].as_str().unwrap())
		.unwrap();
	let signing = jsonwebtoken::EncodingKey::from_secret(&secret);
	for role in ["publish", "subscribe"] {
		for path in [serde_json::json!("alice/**"), serde_json::json!(["alice/**"])] {
			let mut payload = serde_json::json!({"publish":["**"],"subscribe":["**"]});
			payload[role] = serde_json::json!({"path":path,"rate":8_000_000});
			let token = jsonwebtoken::encode(
				&jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
				&payload,
				&signing,
			)
			.unwrap();
			assert!(
				key.verify(&token).is_err(),
				"must not discard the restriction: {payload}"
			);
			assert!(published.verify(&token).is_err());
			assert!(legacy.verify(&token).is_err());
			assert!(serde_json::from_value::<moq_auth::Grant>(payload.clone()).is_err());
			assert!(serde_json::from_value::<moq_auth_published::Grant>(payload).is_err());
		}
	}
}
