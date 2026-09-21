use std::fmt;
use std::str::FromStr;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use crate::error::{Error, Result};
use crate::limits::{NAME_LEN, NAME_TEXT_LEN};

/// An opaque physical track name: 22 unpadded base64url characters derived from a semantic name.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Name([u8; NAME_TEXT_LEN]);

impl Name {
	/// The 22 ASCII characters, a valid moq-lite track name.
	pub fn as_str(&self) -> &str {
		std::str::from_utf8(&self.0).expect("base64url is ASCII")
	}
}

impl FromStr for Name {
	type Err = Error;

	/// Accept a name learned from a track or a decrypted catalog.
	fn from_str(name: &str) -> Result<Self> {
		if name.len() != NAME_TEXT_LEN {
			return Err(Error::Identity);
		}
		let mut material = [0u8; NAME_LEN];
		let n = URL_SAFE_NO_PAD
			.decode_slice(name.as_bytes(), &mut material)
			.map_err(|_| Error::Identity)?;
		if n != NAME_LEN {
			return Err(Error::Identity);
		}
		let mut out = [0u8; NAME_TEXT_LEN];
		out.copy_from_slice(name.as_bytes());
		Ok(Self(out))
	}
}

impl fmt::Debug for Name {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_tuple("Name").field(&self.as_str()).finish()
	}
}

impl fmt::Display for Name {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(self.as_str())
	}
}

impl AsRef<str> for Name {
	fn as_ref(&self) -> &str {
		self.as_str()
	}
}

/// Encode 16 bytes of HKDF output as a name.
pub(crate) fn encode(material: [u8; NAME_LEN]) -> Name {
	let mut out = [0u8; NAME_TEXT_LEN];
	let n = URL_SAFE_NO_PAD
		.encode_slice(material, &mut out)
		.expect("16 bytes encode to 22 base64url characters");
	debug_assert_eq!(n, NAME_TEXT_LEN);
	Name(out)
}
