//! A 32-byte secret an operator configures as a file, such as `cluster.lan.secret`
//! or `listen.reset.secret`.

use std::fmt;
use std::path::{Path, PathBuf};

/// A secret file could not be read or decoded.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// The file does not hold exactly 64 hexadecimal characters.
	#[error("secret file {0} must hold exactly 64 hexadecimal characters (`openssl rand -hex 32`)")]
	Invalid(PathBuf),

	/// The file could not be read.
	#[error("failed to read secret file {0}")]
	Read(PathBuf, #[source] std::io::Error),
}

/// A random 32-byte secret.
///
/// Anyone watching what it keys can brute-force a weak one offline, so generate
/// it (`openssl rand -hex 32`) rather than choosing one. `Debug` never prints it.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret([u8; 32]);

impl Secret {
	/// Read a secret from a file holding 64 hexadecimal characters, optionally
	/// followed by one newline, as `openssl rand -hex 32` writes it.
	pub fn read(path: impl AsRef<Path>) -> Result<Self, Error> {
		let path = path.as_ref();
		let contents = std::fs::read(path).map_err(|err| Error::Read(path.to_path_buf(), err))?;
		let hex = contents.strip_suffix(b"\n").unwrap_or(&contents);
		let mut secret = [0; 32];
		hex::decode_to_slice(hex, &mut secret).map_err(|_| Error::Invalid(path.to_path_buf()))?;
		Ok(Self(secret))
	}

	/// The secret's 32 bytes.
	pub fn as_bytes(&self) -> &[u8; 32] {
		&self.0
	}
}

impl From<[u8; 32]> for Secret {
	fn from(secret: [u8; 32]) -> Self {
		Self(secret)
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
	fn debug_never_prints_the_bytes() {
		let secret = Secret::from([0xab; 32]);
		assert_eq!(format!("{secret:?}"), "Secret([redacted])");
	}

	/// The file `openssl rand -hex 32 > secret.key` writes decodes with or
	/// without its newline, and anything else is refused.
	#[test]
	fn read_requires_exactly_64_hex_characters() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("secret.key");

		for valid in [format!("{KEY}\n"), KEY.to_string()] {
			std::fs::write(&path, valid).unwrap();
			let secret = Secret::read(&path).unwrap();
			assert_eq!(secret.as_bytes()[31], 0x1f);
		}

		// Empty, short, raw bytes, non-hex, and a byte too few or too many.
		let invalid: [&[u8]; 6] = [b"", b"\n", &[0x5a; 32], &[b'x'; 64], &[b'0'; 62], &[b'0'; 66]];
		for contents in invalid {
			std::fs::write(&path, contents).unwrap();
			assert!(matches!(Secret::read(&path), Err(Error::Invalid(_))), "{contents:?}");
		}
		for contents in [format!("{KEY}\n\n"), format!(" {KEY}"), format!("{KEY}\r\n")] {
			std::fs::write(&path, &contents).unwrap();
			assert!(matches!(Secret::read(&path), Err(Error::Invalid(_))), "{contents:?}");
		}

		assert!(matches!(
			Secret::read(dir.path().join("missing.key")),
			Err(Error::Read(..))
		));
	}
}
