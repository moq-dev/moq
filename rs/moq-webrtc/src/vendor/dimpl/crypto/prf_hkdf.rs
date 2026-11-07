//! PRF key derivation built on top of [`HmacProvider`].
//!
//! The TLS 1.2 PRF is a pure composition of HMAC calls. This module provides a
//! generic implementation so that crypto backends only need to implement
//! [`HmacProvider`] — no separate PRF provider.

use crate::vendor::dimpl::CryptoError;
use crate::vendor::dimpl::buffer::Buf;
use crate::vendor::dimpl::types::HashAlgorithm;

use super::HmacProvider;

/// Maximum HMAC output size we support (SHA-384 = 48 bytes).
const MAX_HASH_LEN: usize = 48;

// ============================================================================
// TLS 1.2 PRF (RFC 5246 Section 5)
// ============================================================================

/// TLS 1.2 PRF: `PRF(secret, label, seed)` writing `output_len` bytes to `out`.
///
/// Uses `scratch` for temporary concatenation of label+seed.
#[allow(clippy::too_many_arguments)]
pub fn prf_tls12(
	hmac: &dyn HmacProvider,
	secret: &[u8],
	label: &str,
	seed: &[u8],
	out: &mut Buf,
	output_len: usize,
	scratch: &mut Buf,
	hash: HashAlgorithm,
) -> Result<(), CryptoError> {
	let mut hmac_a = [0u8; MAX_HASH_LEN];

	// Build label + seed
	scratch.clear();
	scratch.extend_from_slice(label.as_bytes());
	scratch.extend_from_slice(seed);
	let label_seed = scratch.as_ref();

	// A(1) = HMAC(secret, label_seed)
	let hash_len = hmac.hmac(hash, secret, label_seed, &mut hmac_a)?;

	// Build payload = A(i) || label || seed
	scratch.clear();
	scratch.extend_from_slice(&hmac_a[..hash_len]);
	scratch.extend_from_slice(label.as_bytes());
	scratch.extend_from_slice(seed);
	let payload = scratch.as_mut();

	out.clear();
	while out.len() < output_len {
		// P(i) = HMAC(secret, A(i) || label || seed)
		let mut hmac_block = [0u8; MAX_HASH_LEN];
		let block_len = hmac.hmac(hash, secret, payload, &mut hmac_block)?;

		let remaining = output_len - out.len();
		let to_copy = remaining.min(block_len);
		out.extend_from_slice(&hmac_block[..to_copy]);

		if out.len() < output_len {
			// A(i+1) = HMAC(secret, A(i))
			hmac.hmac(hash, secret, &payload[..hash_len], &mut hmac_a)?;
			payload[..hash_len].copy_from_slice(&hmac_a[..hash_len]);
		}
	}

	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	fn hex_to_vec(hex: &str) -> Vec<u8> {
		let hex = hex.replace([' ', '\n'], "");
		let mut v = Vec::new();
		for i in 0..hex.len() / 2 {
			// unwrap: test-only hex parsing
			let byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap();
			v.push(byte);
		}
		v
	}

	fn slice_to_hex(data: &[u8]) -> String {
		let mut s = String::new();
		for byte in data.iter() {
			s.push_str(&format!("{:02x}", byte));
		}
		s
	}

	/// Convert an ASCII hex array into a byte array at compile time.
	macro_rules! hex_as_bytes {
		($input:expr) => {{
			const fn from_hex_char(c: u8) -> u8 {
				match c {
					b'0'..=b'9' => c - b'0',
					b'a'..=b'f' => c - b'a' + 10,
					b'A'..=b'F' => c - b'A' + 10,
					_ => panic!("Invalid hex character"),
				}
			}

			const INPUT: &[u8] = $input;
			const LEN: usize = INPUT.len();
			const OUTPUT_LEN: usize = LEN / 2;

			const fn convert() -> [u8; OUTPUT_LEN] {
				assert!(LEN % 2 == 0, "Hex string length must be even");

				let mut out = [0u8; OUTPUT_LEN];
				let mut i = 0;
				while i < LEN {
					out[i / 2] = (from_hex_char(INPUT[i]) << 4) | from_hex_char(INPUT[i + 1]);
					i += 2;
				}
				out
			}

			convert()
		}};
	}

	/// We need a concrete HmacProvider for tests. Use the default feature-gated one.
	fn hmac_provider() -> &'static dyn HmacProvider {
		#[cfg(all())]
		{
			&crate::vendor::dimpl::crypto::aws_lc_rs::hmac::HMAC_PROVIDER
		}
		#[cfg(all(not(all()), any()))]
		{
			&crate::vendor::dimpl::crypto::rust_crypto::hmac::HMAC_PROVIDER
		}
	}

	// ========================================================================
	// HMAC-SHA-256 Test Vectors from RFC 4231
	// ========================================================================

	#[test]
	fn hmac_sha256_test_case_1() {
		let key = hex_to_vec("0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b");
		let data = b"Hi There";
		let expected = "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7";

		let result = hmac_provider().hmac_sha256(&key, data).unwrap();
		assert_eq!(slice_to_hex(&result), expected);
	}

	#[test]
	fn hmac_sha256_test_case_2() {
		let key = b"Jefe";
		let data = b"what do ya want for nothing?";
		let expected = "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843";

		let result = hmac_provider().hmac_sha256(key, data).unwrap();
		assert_eq!(slice_to_hex(&result), expected);
	}

	#[test]
	fn hmac_sha256_test_case_3() {
		let key = hex_to_vec("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
		let data = hex_to_vec(
			"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd\
             dddddddddddddddddddddddddddddddddddd",
		);
		let expected = "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe";

		let result = hmac_provider().hmac_sha256(&key, &data).unwrap();
		assert_eq!(slice_to_hex(&result), expected);
	}

	#[test]
	fn hmac_sha256_test_case_4() {
		let key = hex_to_vec("0102030405060708090a0b0c0d0e0f10111213141516171819");
		let data = hex_to_vec(
			"cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd\
             cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd",
		);
		let expected = "82558a389a443c0ea4cc819899f2083a85f0faa3e578f8077a2e3ff46729665b";

		let result = hmac_provider().hmac_sha256(&key, &data).unwrap();
		assert_eq!(slice_to_hex(&result), expected);
	}

	#[test]
	fn hmac_sha256_test_case_6() {
		// Test with a key larger than block size (> 64 bytes)
		let key = vec![0xaa; 131];
		let data = b"Test Using Larger Than Block-Size Key - Hash Key First";
		let expected = "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54";

		let result = hmac_provider().hmac_sha256(&key, data).unwrap();
		assert_eq!(slice_to_hex(&result), expected);
	}

	#[test]
	fn hmac_sha256_test_case_7() {
		// Test with a key larger than block size and large data
		let key = vec![0xaa; 131];
		let data = b"This is a test using a larger than block-size key and a larger than block-size \
              data. The key needs to be hashed before being used by the HMAC algorithm.";
		let expected = "9b09ffa71b942fcb27635fbcd5b0e944bfdc63644f0713938a7f51535c3a35e2";

		let result = hmac_provider().hmac_sha256(&key, data).unwrap();
		assert_eq!(slice_to_hex(&result), expected);
	}

	// ========================================================================
	// TLS 1.2 PRF
	// ========================================================================

	#[test]
	fn prf_tls12_sha256() {
		// Test vector from https://github.com/xomexh/TLS-PRF
		let mut output = Buf::new();
		let mut scratch = Buf::new();
		prf_tls12(
			hmac_provider(),
			&hex_as_bytes!(b"9bbe436ba940f017b17652849a71db35"),
			"test label",
			&hex_as_bytes!(b"a0ba9f936cda311827a6f796ffd5198c"),
			&mut output,
			100,
			&mut scratch,
			HashAlgorithm::SHA256,
		)
		.unwrap();
		assert_eq!(
			output.as_ref(),
			&hex_as_bytes!(
				b"e3f229ba727be17b8d122620557cd453c2aab21d\
                  07c3d495329b52d4e61edb5a6b301791e90d35c9\
                  c9a46b4e14baf9af0fa022f7077def17abfd3797\
                  c0564bab4fbc91666e9def9b97fce34f796789ba\
                  a48082d122ee42c5a72e5a5110fff70187347b66"
			)
		);
	}
}
