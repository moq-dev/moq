//! Bytes cross the language boundary to a Bun script under `test/interop`: subscription
//! responses for subscribers on a mock transport, and varint encodings.

use crate::coding::{Decoder, Encoder, Form, varint};
use crate::ietf;

// Runs a script with a JSON argument, returning its stdout.
fn bun(script: &str, input: serde_json::Value) -> Vec<u8> {
	let output = std::process::Command::new("bun")
		.arg(format!("{}/../../test/interop/{script}", env!("CARGO_MANIFEST_DIR")))
		.arg(input.to_string())
		.output()
		.expect("Bun must be installed for the interop tests");
	assert!(
		output.status.success(),
		"{script} failed: {}",
		String::from_utf8_lossy(&output.stderr)
	);
	output.stdout
}

pub(crate) fn fin(version: &str, started: bool, clean: bool, responses: Vec<u8>) -> Vec<u8> {
	let input = serde_json::json!({ "version": version, "started": started, "clean": clean, "responses": responses });
	serde_json::from_slice(&bun("bare-fin.ts", input)).expect("JS publisher returned response bytes")
}

/// Every varint size boundary in both formats, plus the 2^53 edge of a JS `number`. js/net's own
/// tests cover leading-ones values past 2^62 - 1, where the QUIC form stops.
#[test]
#[ignore = "requires Bun; run by just test interop"]
fn varint_interop() {
	let mut values = vec![0u64];
	for bits in [6, 7, 14, 21, 28, 30, 32, 35, 42, 49, 53, 56] {
		values.extend([(1 << bits) - 1, 1 << bits]);
	}
	values.push(varint::MAX_QUIC);

	let quic = Form::Quic;
	let leading_ones = Form::from(ietf::Version::Draft17);
	let encode = |v: u64, form: Form| {
		let mut buf = Vec::new();
		Encoder::new(&mut buf, form).varint(v).unwrap();
		buf
	};
	let decode = |buf: &[u8], form: Form| {
		let mut decoder = Decoder::new(buf, form);
		let v = decoder.varint().unwrap();
		assert!(decoder.is_empty());
		v
	};

	let input = serde_json::json!({
		"values": values.iter().map(ToString::to_string).collect::<Vec<_>>(),
		"quic": values.iter().map(|&v| encode(v, quic)).collect::<Vec<_>>(),
		"leadingOnes": values.iter().map(|&v| encode(v, leading_ones)).collect::<Vec<_>>(),
	});
	let output: serde_json::Value =
		serde_json::from_slice(&bun("varint.ts", input)).expect("JS returned its encodings");
	let js = |format: &str| -> Vec<Vec<u8>> { serde_json::from_value(output[format].clone()).unwrap() };

	for ((&value, js_quic), js_leading) in values.iter().zip(js("quic")).zip(js("leadingOnes")) {
		assert_eq!(js_quic, encode(value, quic), "QUIC encoding of {value}");
		assert_eq!(
			js_leading,
			encode(value, leading_ones),
			"leading-ones encoding of {value}"
		);
		assert_eq!(decode(&js_quic, quic), value);
		assert_eq!(decode(&js_leading, leading_ones), value);
	}
}
