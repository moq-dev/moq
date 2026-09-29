//! Bytes cross the language boundary to a Bun script under `test/interop`: subscription
//! responses for subscribers on a mock transport, and varint encodings.

use crate::coding::{Decode, Encode, VarInt};
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

/// Every varint size boundary in both formats, plus the 2^53 edge of a JS `number`.
#[test]
#[ignore = "requires Bun; run by just test interop"]
fn varint_interop() {
	let mut values = vec![0u64];
	for bits in [6, 7, 14, 21, 28, 30, 32, 35, 42, 49, 53, 56] {
		values.extend([(1 << bits) - 1, 1 << bits]);
	}
	values.push(VarInt::MAX.into_inner());
	let values: Vec<VarInt> = values.into_iter().map(|v| VarInt::from_u64(v).unwrap()).collect();

	let quic = |v: &VarInt| {
		let mut buf = Vec::new();
		v.encode_quic(&mut buf).unwrap();
		buf
	};
	let leading_ones = |v: &VarInt| {
		let mut buf = Vec::new();
		v.encode(&mut buf, ietf::Version::Draft17).unwrap();
		buf
	};

	let input = serde_json::json!({
		"values": values.iter().map(ToString::to_string).collect::<Vec<_>>(),
		"quic": values.iter().map(quic).collect::<Vec<_>>(),
		"leadingOnes": values.iter().map(leading_ones).collect::<Vec<_>>(),
	});
	let output: serde_json::Value = serde_json::from_slice(&bun("varint.ts", input)).expect("JS returned its encodings");
	let js = |format: &str| -> Vec<Vec<u8>> { serde_json::from_value(output[format].clone()).unwrap() };

	for ((value, js_quic), js_leading) in values.iter().zip(js("quic")).zip(js("leadingOnes")) {
		assert_eq!(js_quic, quic(value), "QUIC encoding of {value}");
		assert_eq!(js_leading, leading_ones(value), "leading-ones encoding of {value}");

		let mut buf = js_quic.as_slice();
		assert_eq!(VarInt::decode_quic(&mut buf).unwrap(), *value);
		assert!(buf.is_empty());
		let mut buf = js_leading.as_slice();
		assert_eq!(VarInt::decode(&mut buf, ietf::Version::Draft17).unwrap(), *value);
		assert!(buf.is_empty());
	}
}
