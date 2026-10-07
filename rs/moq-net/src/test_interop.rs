//! Bytes cross the language boundary to a Bun script under `test/interop`: subscription
//! responses for subscribers on a mock transport, varint encodings, and moq-lite messages.

use crate::coding::{Decoder, Encode, Encoder, Form, varint};
use crate::{ietf, lite};

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

/// Every leading-ones length boundary, plus the JS safe-integer edge and the 62-bit ceiling.
fn lite_values() -> Vec<u64> {
	let mut values = vec![0];
	for bits in [6, 7, 14, 21, 28, 30, 35, 42, 49, 53, 56] {
		values.extend([(1u64 << bits) - 1, 1 << bits]);
	}
	values.push(varint::MAX_QUIC);
	values
}

/// One GROUP header and its frames, as a publisher writes them: a zigzag timestamp delta,
/// the payload size, then the payload.
fn lite_group(version: lite::Version) -> Vec<u8> {
	let mut buf = Vec::new();
	let w = &mut Encoder::new(&mut buf, version.into());
	let header = lite::Group {
		subscribe: 5,
		sequence: 1 << 20,
		frame_start: 200,
	};
	header.encode(w, version).unwrap();
	for (delta, size) in [(0i64, 10usize), (33_333, 300), (-1_000, 20_000), (1 << 40, 1)] {
		w.varint(varint::zigzag(delta)).unwrap();
		w.varint(size as u64).unwrap();
		w.slice(&vec![0xAB; size]);
	}
	buf
}

#[derive(serde::Deserialize)]
struct Echo {
	values: Vec<String>,
	varints: Vec<u8>,
	setup: Vec<u8>,
	datagram: Vec<u8>,
	group: Vec<u8>,
	beyond: Vec<u8>,
}

/// JS decodes what Rust encodes back to the same values, and its own encoding of those
/// values is byte for byte Rust's, on lite-06 (QUIC) and lite-07 (leading-ones).
///
/// Past 2^62-1 the range is per version: JS writes lite-07's 64-bit values, which Rust
/// reads back, while on lite-06 JS refuses them.
#[test]
#[ignore = "requires Bun; run by just test interop"]
fn lite_varint_interop() {
	let values = lite_values();
	for version in [lite::Version::Lite06, lite::Version::Lite07] {
		let mut varints = Vec::new();
		let w = &mut Encoder::new(&mut varints, version.into());
		for value in &values {
			w.varint(*value).unwrap();
		}

		let setup = lite::Setup {
			hop: Some(crate::Hop::new(varint::MAX_QUIC).unwrap()),
			..Default::default()
		}
		.encode_bytes(version)
		.unwrap();

		let datagram = lite::Datagram {
			subscribe: (1 << 40) + 1,
			sequence: 300,
			timestamp: (1 << 50) + 5,
			payload: bytes::Bytes::from_static(b"x"),
		}
		.encode_bytes(version)
		.unwrap();

		let group = lite_group(version);

		let input = serde_json::json!({
			"version": crate::Version::from(version).alpn(),
			"values": values.len(),
			"varints": varints,
			"setup": setup.to_vec(),
			"datagram": datagram.to_vec(),
			"group": group,
		});
		let echo: Echo = serde_json::from_slice(&bun("lite-varint.ts", input)).expect("JS returned its encodings");

		let decoded: Vec<u64> = echo.values.iter().map(|v| v.parse().unwrap()).collect();
		assert_eq!(decoded, values, "{version}: JS decoded different values");
		assert_eq!(echo.varints, varints, "{version}: JS encoded the values differently");
		assert_eq!(echo.setup, setup, "{version}: SETUP");
		assert_eq!(echo.datagram, datagram, "{version}: datagram");
		assert_eq!(echo.group, group, "{version}: group");

		match version {
			lite::Version::Lite07 => {
				let mut expected = vec![0xFF, 0x40, 0, 0, 0, 0, 0, 0, 0];
				expected.extend([0xFF; 9]);
				assert_eq!(echo.beyond, expected, "{version}: JS's 64-bit encodings");
				let mut r = Decoder::new(&echo.beyond, Form::from(version));
				assert_eq!(r.varint().unwrap(), 1 << 62);
				assert_eq!(r.varint().unwrap(), u64::MAX);
				assert!(r.is_empty());
			}
			_ => assert!(echo.beyond.is_empty(), "{version}: JS wrote a value past 2^62-1"),
		}
	}
}
