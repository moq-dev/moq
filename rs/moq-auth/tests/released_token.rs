//! The token and grant encodings released verifiers depend on.
//!
//! moq-relay 0.14 verifies with `moq-token` 0.7, which reads `put`/`get` prefixes and
//! refuses `publish`/`subscribe`. Published moq-auth 0.2.1 reads pattern arrays and
//! refuses a role written as an object. Both are frozen, so each test pins the exact
//! JSON we write: a subtree grant one they read with the same scope, and anything else
//! a shape they refuse in full rather than misread. The shapes were checked against
//! both releases when the crates were dropped as dev-dependencies, since each pulls in
//! a C crypto library.

use moq_auth::{Algorithm, Claims, Key};
use serde_json::{Value, json};

fn patterns(texts: &[&str]) -> moq_auth::Patterns {
	texts.iter().map(|text| text.parse().unwrap()).collect()
}

/// Sign and verify `claims` with our own key, returning the JSON written on the wire.
fn wire(claims: &Claims) -> Value {
	let key = Key::generate(Algorithm::HS256, None).unwrap();
	let verified = key.verify(&key.sign(claims).unwrap()).unwrap();
	let wire = serde_json::to_value(claims).unwrap();
	assert_eq!(serde_json::to_value(verified).unwrap(), wire);
	wire
}

/// The grant a relay hands an auth callback for `claims`, as JSON.
fn grant(claims: &Claims) -> Value {
	let mut grant = moq_auth::Grant::new(claims.publish.clone(), claims.subscribe.clone());
	(grant.publish_rate, grant.subscribe_rate) = (claims.publish_rate, claims.subscribe_rate);
	let wire = serde_json::to_value(&grant).unwrap();
	assert_eq!(serde_json::from_value::<moq_auth::Grant>(wire.clone()).unwrap(), grant);
	wire
}

#[test]
fn subtree_grants_are_written_as_legacy_prefixes() {
	let claims = Claims::default()
		.with_root("room")
		.with_publish(patterns(&["alice/**"]))
		.with_subscribe(patterns(&["**"]));
	assert_eq!(wire(&claims), json!({"root": "room", "put": ["alice"], "get": [""]}));

	let claims = Claims::default()
		.with_root("room")
		.with_publish(patterns(&["alice/**", "bob/**"]));
	assert_eq!(wire(&claims), json!({"root": "room", "put": ["alice", "bob"]}));
	assert_eq!(grant(&claims), json!({"publish": ["alice/**", "bob/**"]}));
}

#[test]
fn exact_grants_are_written_as_patterns() {
	for (claims, expected) in [
		(
			Claims::default().with_root("room").with_publish(patterns(&["alice"])),
			json!({"root": "room", "publish": ["alice"]}),
		),
		(
			Claims::default().with_subscribe(patterns(&["*/chat"])),
			json!({"subscribe": ["*/chat"]}),
		),
		// One exact grant moves the whole document to patterns.
		(
			Claims::default()
				.with_publish(patterns(&["alice/**"]))
				.with_subscribe(patterns(&["bob"])),
			json!({"publish": ["alice/**"], "subscribe": ["bob"]}),
		),
		(
			Claims::default()
				.with_root("room")
				.with_publish(patterns(&["alice", "*/chat"])),
			json!({"root": "room", "publish": ["*/chat", "alice"]}),
		),
	] {
		assert_eq!(wire(&claims), expected, "{claims:?}");
		let mut grant_json = expected;
		grant_json.as_object_mut().unwrap().remove("root");
		assert_eq!(grant(&claims), grant_json, "{claims:?}");
	}
}

/// A capped role is an object every published verifier refuses in full, so an old
/// relay never admits the paths while dropping the cap.
#[test]
fn capped_roles_are_written_as_objects() {
	let rate = moq_auth::Rate::from_bps(8_000_000).unwrap();
	// Subtrees alone would be written as legacy prefixes, which a cap must not be.
	let claims = Claims::default()
		.with_publish(patterns(&["alice/**"]))
		.with_subscribe(patterns(&["**"]));
	for (claims, expected) in [
		(
			claims.clone().with_publish_rate(rate),
			json!({"publish": {"path": ["alice/**"], "rate": 8_000_000}, "subscribe": ["**"]}),
		),
		(
			claims.with_subscribe_rate(rate),
			json!({"publish": ["alice/**"], "subscribe": {"path": ["**"], "rate": 8_000_000}}),
		),
	] {
		assert_eq!(wire(&claims), expected, "{claims:?}");
		assert_eq!(grant(&claims), expected, "{claims:?}");
	}
}
