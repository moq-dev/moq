//! The token and grant encodings released issuers and verifiers depend on.
//!
//! Tokens are written in the documented shape, `root`, `publish`, and `subscribe`, which
//! every moq-auth release reads. Tokens minted by `moq-token`, the format before
//! patterns, are still read: each `put`/`get` prefix `p` grants the subtree `p/**`.
//! Published moq-auth 0.2.1 refuses a role written as an object, so a capped role is a
//! shape it refuses in full rather than misread.

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
	let mut grant = moq_auth::Grant::default();
	(grant.publish, grant.subscribe) = (claims.publish.clone(), claims.subscribe.clone());
	let wire = serde_json::to_value(&grant).unwrap();
	assert_eq!(serde_json::from_value::<moq_auth::Grant>(wire.clone()).unwrap(), grant);
	wire
}

#[test]
fn grants_are_written_as_patterns() {
	for (claims, expected) in [
		(
			Claims::default()
				.with_root("room")
				.with_publish(patterns(&["alice/**"]))
				.with_subscribe(patterns(&["**"])),
			json!({"root": "room", "publish": ["alice/**"], "subscribe": ["**"]}),
		),
		(
			Claims::default().with_root("room").with_publish(patterns(&["alice"])),
			json!({"root": "room", "publish": ["alice"]}),
		),
		(
			Claims::default().with_subscribe(patterns(&["*/chat"])),
			json!({"subscribe": ["*/chat"]}),
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

/// A token `@moq/auth` minted in the `moq-token` prefix form, before tokens were patterns.
#[test]
fn legacy_prefixes_verify_as_subtrees() {
	let key: Key = r#"{"kty":"oct","alg":"HS256","k":"xm6xsSkfFqzPU3KfcbAcF2_h0OkStxQ_nNqVPYl0ync","kid":"js-test-key","key_ops":["sign","verify"]}"#
		.parse()
		.unwrap();
	// {"root":"live","put":["camera1"],"get":["camera1","camera2"],"iat":1775176754}
	let token = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCIsImtpZCI6ImpzLXRlc3Qta2V5In0.eyJyb290IjoibGl2ZSIsInB1dCI6WyJjYW1lcmExIl0sImdldCI6WyJjYW1lcmExIiwiY2FtZXJhMiJdLCJpYXQiOjE3NzUxNzY3NTR9.tHNQtHh_HCIKxXOexDCM7AkjqWzbULLZzjEckfOGRfY";

	let claims = key.verify(token).unwrap();
	assert_eq!(claims.publish.path, patterns(&["camera1/**"]));
	assert_eq!(claims.subscribe.path, patterns(&["camera1/**", "camera2/**"]));
	// Signed again, it is written in the documented shape.
	assert_eq!(
		wire(&claims),
		json!({"root": "live", "publish": ["camera1/**"], "subscribe": ["camera1/**", "camera2/**"], "iat": 1775176754}),
	);
}

/// A capped role is an object every published verifier refuses in full, so an old
/// relay never admits the paths while dropping the cap.
#[test]
fn capped_roles_are_written_as_objects() {
	let rate = moq_auth::Rate::from_bps(8_000_000).unwrap();
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
