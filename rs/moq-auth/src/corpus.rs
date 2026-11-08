//! Replays `tests/corpus.json`, which `js/auth/src/corpus.test.ts` replays too, so the
//! two parsers cannot disagree on a token, key, key set, or grant without both failing.

use crate::{Claims, Grant, Key, KeySet};
use kio::clock::Wall;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::str::FromStr;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Corpus {
	keys: BTreeMap<String, Value>,
	tokens: Vec<TokenCase>,
	jwk: Vec<Case>,
	sets: Vec<Case>,
	grants: Vec<Case>,
}

/// A token, given signed or as a payload the case's key signs here, and the claims it
/// verifies to, or `null` when it is refused.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenCase {
	name: String,
	key: String,
	token: Option<String>,
	payload: Option<Value>,
	claims: Option<Value>,
}

/// A document under `jwk`, `jwks`, or `grant`, and whether it is accepted.
#[derive(Deserialize)]
struct Case {
	name: String,
	#[serde(alias = "jwk", alias = "jwks", alias = "grant")]
	input: Value,
	valid: bool,
}

fn corpus() -> Corpus {
	serde_json::from_str(include_str!("../tests/corpus.json")).unwrap()
}

/// The claims as the corpus writes them: patterns reduced, absent roles empty, and
/// rates and times only when present.
fn canonical(claims: &Claims) -> Value {
	let seconds = |wall: Wall| wall.checked_duration_since(Wall::UNIX_EPOCH).unwrap().as_secs();
	let mut out = json!({
		"root": claims.root,
		"publish": claims.publish,
		"subscribe": claims.subscribe,
	});
	let fields = [
		("publish_rate", claims.publish_rate.map(u64::from)),
		("subscribe_rate", claims.subscribe_rate.map(u64::from)),
		("exp", claims.expires.map(seconds)),
		("iat", claims.issued.map(seconds)),
		("nbf", claims.not_before.map(seconds)),
	];
	for (field, value) in fields {
		if let Some(value) = value {
			out[field] = value.into();
		}
	}
	out
}

#[test]
fn tokens() {
	let corpus = corpus();
	let keys: BTreeMap<_, _> = corpus
		.keys
		.into_iter()
		.map(|(name, jwk)| (name, serde_json::from_value::<Key>(jwk).unwrap()))
		.collect();

	for case in corpus.tokens {
		let key = &keys[&case.key];
		let token = match (case.token, case.payload) {
			(Some(token), None) => token,
			(None, Some(payload)) => {
				let mut header = jsonwebtoken::Header::new(key.algorithm.into());
				header.kid = key.kid.as_ref().map(ToString::to_string);
				jsonwebtoken::encode(&header, &payload, key.to_encoding_key().unwrap()).unwrap()
			}
			_ => panic!("{}: give exactly one of token or payload", case.name),
		};
		let verified = key.verify(&token);
		match (&case.claims, verified) {
			(Some(expected), Ok(claims)) => assert_eq!(&canonical(&claims), expected, "{}", case.name),
			(None, Err(_)) => {}
			(Some(_), Err(err)) => panic!("{}: refused: {err}", case.name),
			(None, Ok(claims)) => panic!("{}: accepted as {}", case.name, canonical(&claims)),
		}
	}
}

#[test]
fn documents() {
	let corpus = corpus();
	type Parse = fn(Value) -> Result<(), String>;
	let parsers: [(&str, Vec<Case>, Parse); 3] = [
		("jwk", corpus.jwk, |jwk| {
			Key::from_str(&jwk.to_string()).map(drop).map_err(|err| err.to_string())
		}),
		("sets", corpus.sets, |jwks| {
			KeySet::from_str(&jwks.to_string())
				.map(drop)
				.map_err(|err| err.to_string())
		}),
		("grants", corpus.grants, |grant| {
			let grant: Grant = serde_json::from_value(grant).map_err(|err| err.to_string())?;
			grant.validate().map_err(|err| err.to_string())
		}),
	];
	for (section, cases, parse) in parsers {
		for case in cases {
			match (case.valid, parse(case.input)) {
				(true, Err(err)) => panic!("{section}/{}: refused: {err}", case.name),
				(false, Ok(())) => panic!("{section}/{}: accepted", case.name),
				_ => {}
			}
		}
	}
}
