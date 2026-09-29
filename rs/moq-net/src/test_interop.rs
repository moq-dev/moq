//! Wire bytes cross the language boundary: each side decodes what the other encoded.

/// Run a `test/interop` Bun script on a JSON input and parse the JSON it prints.
fn bun<T: serde::de::DeserializeOwned>(script: &str, input: serde_json::Value) -> T {
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
	serde_json::from_slice(&output.stdout).expect("the JS side printed JSON")
}

pub(crate) fn fin(version: &str, started: bool, clean: bool, responses: Vec<u8>) -> Vec<u8> {
	let input = serde_json::json!({ "version": version, "started": started, "clean": clean, "responses": responses });
	bun("bare-fin.ts", input)
}

#[cfg(test)]
mod tests {
	use crate::coding::{Decode, Encode, VarInt};
	use crate::lite::{self, Version};

	/// Every leading-ones length boundary, plus the JS safe-integer edge and the 62-bit ceiling.
	fn values() -> Vec<u64> {
		let mut values = vec![0];
		for bits in [6, 7, 14, 21, 28, 30, 35, 42, 49, 53, 56] {
			values.extend([(1u64 << bits) - 1, 1 << bits]);
		}
		values.push(VarInt::MAX.into_inner());
		values
	}

	/// One GROUP header and its frames, as a publisher writes them: a zigzag timestamp delta,
	/// the payload size, then the payload.
	fn group(version: Version) -> Vec<u8> {
		let mut buf = Vec::new();
		let header = lite::Group {
			subscribe: 5,
			sequence: 1 << 20,
			frame_start: 200,
		};
		header.encode(&mut buf, version).unwrap();
		for (delta, size) in [(0i64, 10usize), (33_333, 300), (-1_000, 20_000), (1 << 40, 1)] {
			VarInt::from_zigzag(delta).unwrap().encode(&mut buf, version).unwrap();
			size.encode(&mut buf, version).unwrap();
			buf.extend(std::iter::repeat_n(0xAB, size));
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
	/// Past 2^62-1 the range is per version. JS writes lite-07's 64-bit values, which Rust
	/// must refuse with a decode error until its `VarInt` widens; on lite-06 JS refuses them.
	#[test]
	#[ignore = "requires Bun; run by just test lite-varint in interop CI"]
	fn lite_varint_interop() {
		let values = values();
		for version in [Version::Lite06, Version::Lite07] {
			let mut varints = Vec::new();
			for value in &values {
				value.encode(&mut varints, version).unwrap();
			}

			let setup = lite::Setup {
				hop: Some(crate::Hop::new(VarInt::MAX.into_inner()).unwrap()),
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

			let group = group(version);

			let input = serde_json::json!({
				"version": crate::Version::from(version).alpn(),
				"values": values.len(),
				"varints": varints,
				"setup": setup.to_vec(),
				"datagram": datagram.to_vec(),
				"group": group,
			});
			let echo: Echo = super::bun("lite-varint.ts", input);

			let decoded: Vec<u64> = echo.values.iter().map(|v| v.parse().unwrap()).collect();
			assert_eq!(decoded, values, "{version}: JS decoded different values");
			assert_eq!(echo.varints, varints, "{version}: JS encoded the values differently");
			assert_eq!(echo.setup, setup, "{version}: SETUP");
			assert_eq!(echo.datagram, datagram, "{version}: datagram");
			assert_eq!(echo.group, group, "{version}: group");

			match version {
				Version::Lite07 => {
					let mut expected = vec![0xFF, 0x40, 0, 0, 0, 0, 0, 0, 0];
					expected.extend([0xFF; 9]);
					assert_eq!(echo.beyond, expected, "{version}: JS's 64-bit encodings");
					for wire in echo.beyond.chunks(9) {
						let err = VarInt::decode(&mut &wire[..], version).unwrap_err();
						assert!(matches!(err, crate::coding::DecodeError::BoundsExceeded), "{err:?}");
					}
				}
				_ => assert!(echo.beyond.is_empty(), "{version}: JS wrote a value past 2^62-1"),
			}
		}
	}
}
