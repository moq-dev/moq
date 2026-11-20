//! The token and grant encodings released issuers and verifiers depend on.
//!
//! Tokens are written in the documented shape, `root`, `publish`, and `subscribe`, each a
//! list of prefixes. A release that read those lists as patterns reads a prefix `p` as the
//! one path `p`, so it grants less than the token, never more. Tokens minted by
//! `moq-token`, the format before patterns, are still read: each `put`/`get` entry is the
//! same prefix.
//! Published moq-auth 0.2.1 refuses a role written as an object, so a capped role is a
//! shape it refuses in full rather than misread.

use moq_auth::{Algorithm, Claims, Key};
use serde_json::{Value, json};

fn prefixes(texts: &[&str]) -> moq_auth::Prefixes {
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
fn grants_are_written_as_prefixes() {
	for (claims, expected) in [
		(
			Claims::default()
				.with_root("room")
				.with_publish(prefixes(&["alice"]))
				.with_subscribe(prefixes(&[""])),
			json!({"root": "room", "publish": ["alice"], "subscribe": [""]}),
		),
		(
			Claims::default().with_root("room").with_publish(prefixes(&["alice"])),
			json!({"root": "room", "publish": ["alice"]}),
		),
		(
			Claims::default().with_subscribe(prefixes(&["room/chat", "lobby/chat"])),
			json!({"subscribe": ["lobby/chat", "room/chat"]}),
		),
		(
			// A member beneath another adds nothing, so it is never written.
			Claims::default()
				.with_root("room")
				.with_publish(prefixes(&["bob", "alice/cam", "alice"])),
			json!({"root": "room", "publish": ["alice", "bob"]}),
		),
	] {
		assert_eq!(wire(&claims), expected, "{claims:?}");
		let mut grant_json = expected;
		grant_json.as_object_mut().unwrap().remove("root");
		assert_eq!(grant(&claims), grant_json, "{claims:?}");
	}
}

/// Claims minted with patterns, between moq-dev/moq#3746 and prefix grants, are refused
/// loudly rather than read as a narrower or wider grant. `tests/corpus.json` holds a signed one.
#[test]
fn wildcard_claims_are_refused() {
	for payload in [
		json!({"publish": ["alice/**"]}),
		json!({"subscribe": ["**"]}),
		json!({"subscribe": ["*/chat"]}),
	] {
		let err = serde_json::from_value::<Claims>(payload.clone()).unwrap_err();
		assert!(err.to_string().contains("wildcard"), "{payload}: {err}");
	}
}

/// A token `@moq/auth` minted in the `moq-token` prefix form, before tokens were patterns.
#[test]
fn legacy_prefixes_verify_as_prefixes() {
	let key: Key = r#"{"kty":"oct","alg":"HS256","k":"xm6xsSkfFqzPU3KfcbAcF2_h0OkStxQ_nNqVPYl0ync","kid":"js-test-key","key_ops":["sign","verify"]}"#
		.parse()
		.unwrap();
	// {"root":"live","put":["camera1"],"get":["camera1","camera2"],"iat":1775176754}
	let token = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCIsImtpZCI6ImpzLXRlc3Qta2V5In0.eyJyb290IjoibGl2ZSIsInB1dCI6WyJjYW1lcmExIl0sImdldCI6WyJjYW1lcmExIiwiY2FtZXJhMiJdLCJpYXQiOjE3NzUxNzY3NTR9.tHNQtHh_HCIKxXOexDCM7AkjqWzbULLZzjEckfOGRfY";

	let claims = key.verify(token).unwrap();
	assert_eq!(claims.publish.path, prefixes(&["camera1"]));
	assert_eq!(claims.subscribe.path, prefixes(&["camera1", "camera2"]));
	// Signed again, it is written in the documented shape.
	assert_eq!(
		wire(&claims),
		json!({"root": "live", "publish": ["camera1"], "subscribe": ["camera1", "camera2"], "iat": 1775176754}),
	);
}

/// A capped role is an object every published verifier refuses in full, so an old
/// relay never admits the paths while dropping the cap.
#[test]
fn capped_roles_are_written_as_objects() {
	let rate = moq_auth::Rate::from_bps(8_000_000).unwrap();
	let claims = Claims::default()
		.with_publish(prefixes(&["alice"]))
		.with_subscribe(prefixes(&[""]));
	for (claims, expected) in [
		(
			claims.clone().with_publish_rate(rate),
			json!({"publish": {"path": ["alice"], "rate": 8_000_000}, "subscribe": [""]}),
		),
		(
			claims.with_subscribe_rate(rate),
			json!({"publish": ["alice"], "subscribe": {"path": [""], "rate": 8_000_000}}),
		),
	] {
		assert_eq!(wire(&claims), expected, "{claims:?}");
		assert_eq!(grant(&claims), expected, "{claims:?}");
	}
}
