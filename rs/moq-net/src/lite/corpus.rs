//! Replays the moq-lite corpus in `corpus/lite/`, which `js/net` replays too, so a decoder
//! that drifts from the other language fails a test on both sides. See `corpus/README.md`.

use crate::coding::{Decode, DecodeError, Decoder, Encode, Form};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
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
	Announce,
	AnnounceInit,
	Group,
	ClientSetup,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Expect {
	Decoded,
	Refused,
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
		Message::Announce => outcome(data, version, super::AnnounceBroadcast::decode_slice(data, version)),
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
	let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus/lite");
	let mut failures = Vec::new();
	let mut count = 0;
	for entry in std::fs::read_dir(&dir).expect("could not read the corpus") {
		let path = entry.expect("could not read the corpus").path();
		if path.extension().is_none_or(|ext| ext != "json") {
			continue;
		}
		let text = std::fs::read_to_string(&path).expect("could not read a corpus case");
		let case: Case = serde_json::from_str(&text).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
		let want = match case.expect {
			Expect::Decoded => case.hex.clone(),
			Expect::Refused => "refused".to_string(),
		};
		let got = run(&case, &unhex(&case.hex));
		if got != want {
			failures.push(format!("{}: expected {want}, got {got}", path.display()));
		}
		count += 1;
	}
	assert!(count > 0, "the corpus is empty");
	assert!(failures.is_empty(), "{}", failures.join("\n"));
}
