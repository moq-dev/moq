//! Replays `fixtures/lite.json`, the moq-lite corpus `js/net` replays too
//! (`src/lite/corpus.test.ts`), so a decoder that drifts from the other language fails a test
//! on both sides.
//!
//! Each case is one complete wire input at a negotiated `version` (its handshake name), with
//! the outcome both must reach: `decoded`, where our own encoding of the decoded value must
//! equal the input, so a case also pins the value, `refused`, where the input is rejected
//! without asking for more bytes, or `withdrawn`, where a lite-07 route message decodes but its
//! forwarded content is refused, so the receiver withdraws the route or claim and keeps the
//! session. A fuzz crash on a single moq-lite message belongs here
//! rather than in `fuzz/regressions/`, so JS replays it too.

use crate::coding::{Decode, DecodeError, Decoder, Encode, Form};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
	name: String,
	version: crate::Version,
	message: Message,
	hex: String,
	/// `announce_init`: the announce slots left, unbounded when absent.
	budget: Option<usize>,
	/// `client_setup`: the one version the server accepts.
	server: Option<crate::Version>,
	expect: Expect,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Message {
	/// One announcement on the announce stream, type and length prefix included.
	Announce,
	/// One lite-07 reply on the announce stream, subscriber to publisher, type and length
	/// prefix included.
	AnnounceReply,
	/// A lite-01/02 ANNOUNCE_INIT, length prefix included.
	AnnounceInit,
	/// A group stream header, length prefix included.
	Group,
	/// A `moql` CLIENT_SETUP.
	ClientSetup,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Expect {
	Decoded,
	Refused,
	Withdrawn,
}

/// What a decode came to: our own encoding of what it decoded, as hex, or why it did not.
fn outcome<T: Encode<V>, V: Copy + Into<Form>>(
	data: &[u8],
	version: V,
	decoded: Result<(T, usize), DecodeError>,
) -> String {
	match decoded {
		Ok((_, used)) if used != data.len() => format!("left {} bytes", data.len() - used),
		Ok((value, _)) => match value.encode_bytes(version) {
			Ok(bytes) => hex(&bytes),
			Err(err) => format!("could not re-encode: {err}"),
		},
		// Asking for more bytes is neither outcome: every input is complete.
		Err(DecodeError::Short) => "short".to_string(),
		Err(_) => "refused".to_string(),
	}
}

/// Decode `data` the way a session does, with what it refuses checked before what it decodes.
fn run(case: &Case, data: &[u8]) -> String {
	let crate::Version::Lite(version) = case.version else {
		return format!("{} is not a moq-lite version", case.version);
	};
	let decoder = || Decoder::new(data, version.into());
	match case.message {
		Message::Announce => match super::AnnounceBroadcast::decode_slice(data, version) {
			Ok((super::AnnounceBroadcast::Refused { .. }, used)) if used == data.len() => "withdrawn".to_string(),
			decoded => outcome(data, version, decoded),
		},
		Message::AnnounceReply => outcome(data, version, super::AnnounceReply::decode_slice(data, version)),
		Message::AnnounceInit => {
			let mut r = decoder();
			let decoded = super::AnnounceInit::decode_within(&mut r, version, case.budget.unwrap_or(usize::MAX));
			let used = data.len() - r.remaining();
			outcome(data, version, decoded.map(|init| (init, used)))
		}
		Message::Group => outcome(data, version, super::Group::decode_slice(data, version)),
		Message::ClientSetup => {
			let decoded = crate::setup::Client::decode_slice(data, case.version);
			if let (Ok((client, _)), Some(server)) = (&decoded, case.server)
				&& client.select(&server.into()).is_err()
			{
				return "refused".to_string();
			}
			outcome(data, case.version, decoded)
		}
	}
}

fn hex(bytes: &[u8]) -> String {
	bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
	assert!(text.len().is_multiple_of(2), "odd hex length");
	(0..text.len())
		.step_by(2)
		.map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("invalid hex"))
		.collect()
}

#[test]
fn corpus() {
	let cases: Vec<Case> = serde_json::from_str(include_str!("../../fixtures/lite.json")).expect("a malformed corpus");
	assert!(!cases.is_empty(), "the corpus is empty");
	let mut failures = Vec::new();
	for case in &cases {
		let want = match case.expect {
			Expect::Decoded => case.hex.clone(),
			Expect::Refused => "refused".to_string(),
			Expect::Withdrawn => "withdrawn".to_string(),
		};
		let got = run(case, &unhex(&case.hex));
		if got != want {
			failures.push(format!("{}: expected {want}, got {got}", case.name));
		}
	}
	assert!(failures.is_empty(), "{}", failures.join("\n"));
}
