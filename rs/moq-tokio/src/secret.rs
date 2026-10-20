//! A 32-byte secret an operator configures, such as `cluster.lan.secret` or
//! `listen.reset_secret`.

use std::fmt;
use std::str::FromStr;

/// A secret could not be decoded or read.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// The value is not exactly 32 bytes encoded as 64 hexadecimal characters.
	#[error(
		"secret must be exactly 32 bytes encoded as 64 hexadecimal characters, or a path to a file containing them"
	)]
	Invalid,

	/// The value is not a secret, and not a file that could be read either.
	#[error("secret must be 64 hexadecimal characters or a path to a file containing them, and {0} is neither")]
	File(String, #[source] std::io::Error),
}

/// A random 32-byte secret, given as 64 hexadecimal characters.
///
/// Anyone watching what it keys can brute-force a weak one offline, so generate
/// it (`openssl rand -hex 32`) rather than choosing one.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret([u8; 32]);

impl Secret {
	/// Decode a secret from exactly 64 hexadecimal characters.
	pub fn new(secret: impl AsRef<str>) -> Result<Self, Error> {
		let secret = secret.as_ref();
		let mut key = [0; 32];
		if secret.len() != key.len() * 2 || hex::decode_to_slice(secret, &mut key).is_err() {
			return Err(Error::Invalid);
		}
		Ok(Self(key))
	}

	/// Decode a secret given inline, or read it from the file `value` names.
	///
	/// What a `--*-secret <HEX_OR_PATH>` flag accepts. A path that exists but
	/// holds something other than a secret is an error rather than a silent
	/// fallback, and a path that doesn't exist is never created.
	pub fn load(value: impl AsRef<str>) -> Result<Self, Error> {
		let value = value.as_ref();
		if let Ok(secret) = Self::new(value) {
			return Ok(secret);
		}
		// Not a secret, so it can only have been meant as a path. A read failure
		// here is the useful error: it names the file the operator actually typed.
		let contents = std::fs::read_to_string(value).map_err(|err| Error::File(value.to_string(), err))?;
		Self::new(contents.trim())
	}

	/// The secret's 32 bytes.
	pub fn as_bytes(&self) -> &[u8; 32] {
		&self.0
	}
}

impl FromStr for Secret {
	type Err = Error;

	fn from_str(secret: &str) -> Result<Self, Error> {
		Self::new(secret)
	}
}

impl fmt::Debug for Secret {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		formatter.write_str("Secret([redacted])")
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	const KEY: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

	#[test]
	fn new_requires_exactly_32_hex_bytes_and_redacts_debug() {
		for invalid in ["", "00", &"x".repeat(64), &"00".repeat(33)] {
			assert!(Secret::new(invalid).is_err(), "{invalid:?} must not parse");
		}
		assert_eq!(format!("{:?}", Secret::new(KEY).expect("valid")), "Secret([redacted])");
	}

	/// A file holding the hex, as `openssl rand -hex 32` writes it, decodes to the
	/// same secret as the hex given inline.
	#[test]
	fn load_reads_inline_hex_or_a_file() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("secret.key");
		std::fs::write(&path, format!("{KEY}\n")).unwrap();

		let inline = Secret::load(KEY).unwrap();
		assert_eq!(Secret::load(path.to_str().unwrap()).unwrap(), inline);
		assert_eq!(inline.as_bytes()[31], 0x1f);

		// Raw bytes are refused rather than taken as a secret.
		std::fs::write(&path, [0x5a; 32]).unwrap();
		assert!(matches!(Secret::load(path.to_str().unwrap()), Err(Error::Invalid)));
		let missing = dir.path().join("missing.key");
		assert!(matches!(Secret::load(missing.to_str().unwrap()), Err(Error::File(..))));
	}
}
