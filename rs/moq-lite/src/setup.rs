use bytes::Bytes;

use crate::{
	Version,
	coding::{self, Decode, DecodeError, Encode, EncodeError, Sizer},
	ietf, lite,
};

const CLIENT_SETUP: u8 = 0x20;
const SERVER_SETUP: u8 = 0x21;

/// Draft-17 unified SETUP message type (varint 0x2F00)
pub(crate) const SETUP_V17: u64 = 0x2F00;

/// Draft-17+ unified SETUP message — same encoding for both client and server.
#[derive(Debug, Clone)]
pub struct Setup {
	pub parameters: Bytes,
}

impl Setup {
	fn check_version(v: Version) {
		match v {
			Version::Ietf(ietf::Version::Draft14 | ietf::Version::Draft15 | ietf::Version::Draft16)
			| Version::Lite(_) => unreachable!("Setup is draft-17+ only"),
			_ => {}
		}
	}
}

impl Encode<Version> for Setup {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, v: Version) -> Result<(), EncodeError> {
		Self::check_version(v);
		SETUP_V17.encode(w, v)?;
		u16::try_from(self.parameters.len())
			.map_err(|_| EncodeError::TooLarge)?
			.encode(w, v)?;
		if w.remaining_mut() < self.parameters.len() {
			return Err(EncodeError::Short);
		}
		w.put_slice(&self.parameters);
		Ok(())
	}
}

impl Decode<Version> for Setup {
	fn decode<R: bytes::Buf>(r: &mut R, v: Version) -> Result<Self, DecodeError> {
		Self::check_version(v);
		let kind = u64::decode(r, v)?;
		if kind != SETUP_V17 {
			return Err(DecodeError::InvalidValue);
		}
		let size = u16::decode(r, v)? as usize;
		if r.remaining() < size {
			return Err(DecodeError::Short);
		}
		let msg = r.copy_to_bytes(size);
		Ok(Self { parameters: msg })
	}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SetupVersion {
	Draft14,
	Draft15Plus,
	Draft17,
	LiteLegacy,
	Unsupported,
}

impl SetupVersion {
	fn from_version(v: Version) -> Self {
		match v {
			Version::Ietf(ietf::Version::Draft14) => Self::Draft14,
			Version::Ietf(ietf::Version::Draft15) | Version::Ietf(ietf::Version::Draft16) => Self::Draft15Plus,
			Version::Ietf(ietf::Version::Draft17) => Self::Draft17,
			Version::Lite(lite::Version::Lite01) | Version::Lite(lite::Version::Lite02) => Self::LiteLegacy,
			Version::Lite(lite::Version::Lite03 | lite::Version::Lite04) => Self::Unsupported,
		}
	}
}

/// A version-agnostic setup message sent by the client.
#[derive(Debug, Clone)]
pub struct Client {
	/// The list of supported versions in preferred order.
	pub versions: coding::Versions,

	/// Parameters, unparsed because the IETF draft changed the encoding.
	pub parameters: Bytes,
}

impl Client {
	fn encode_inner<W: bytes::BufMut>(&self, w: &mut W, v: Version) -> Result<(), EncodeError> {
		match SetupVersion::from_version(v) {
			SetupVersion::Draft15Plus => {
				// Draft15+: no versions list, parameters only.
			}
			SetupVersion::Draft14 | SetupVersion::LiteLegacy => self.versions.encode(w, v)?,
			SetupVersion::Draft17 | SetupVersion::Unsupported => return Err(EncodeError::Version),
		};
		if w.remaining_mut() < self.parameters.len() {
			return Err(EncodeError::Short);
		}
		w.put_slice(&self.parameters);
		Ok(())
	}
}

impl Decode<Version> for Client {
	/// Decode a client setup message (draft-14 through draft-16 only).
	fn decode<R: bytes::Buf>(r: &mut R, v: Version) -> Result<Self, DecodeError> {
		let kind = u8::decode(r, v)?;
		if kind != CLIENT_SETUP {
			return Err(DecodeError::InvalidValue);
		}

		let size = match SetupVersion::from_version(v) {
			SetupVersion::Draft14 | SetupVersion::Draft15Plus => u16::decode(r, v)? as usize,
			SetupVersion::LiteLegacy => u64::decode(r, v)? as usize,
			SetupVersion::Draft17 | SetupVersion::Unsupported => return Err(DecodeError::Version),
		};

		if r.remaining() < size {
			return Err(DecodeError::Short);
		}

		let mut msg = r.copy_to_bytes(size);

		let versions = match SetupVersion::from_version(v) {
			SetupVersion::Draft15Plus => {
				// Draft15+: no versions list, parameters only.
				coding::Versions::from([v.into()])
			}
			SetupVersion::Draft14 | SetupVersion::LiteLegacy => coding::Versions::decode(&mut msg, v)?,
			SetupVersion::Draft17 | SetupVersion::Unsupported => return Err(DecodeError::Version),
		};

		Ok(Self {
			versions,
			parameters: msg,
		})
	}
}

impl Encode<Version> for Client {
	/// Encode a client setup message (draft-14 through draft-16 only).
	fn encode<W: bytes::BufMut>(&self, w: &mut W, v: Version) -> Result<(), EncodeError> {
		CLIENT_SETUP.encode(w, v)?;

		let mut sizer = Sizer::default();
		self.encode_inner(&mut sizer, v)?;
		let size = sizer.size;

		match SetupVersion::from_version(v) {
			SetupVersion::Draft14 | SetupVersion::Draft15Plus => {
				u16::try_from(size).map_err(|_| EncodeError::TooLarge)?.encode(w, v)?;
			}
			SetupVersion::LiteLegacy => (size as u64).encode(w, v)?,
			SetupVersion::Draft17 | SetupVersion::Unsupported => return Err(EncodeError::Version),
		}
		self.encode_inner(w, v)
	}
}

/// Sent by the server in response to a client setup.
#[derive(Debug, Clone)]
pub struct Server {
	/// The list of supported versions in preferred order.
	pub version: coding::Version,

	/// Supported extensions.
	pub parameters: Bytes,
}

impl Server {
	fn encode_inner<W: bytes::BufMut>(&self, w: &mut W, v: Version) -> Result<(), EncodeError> {
		match SetupVersion::from_version(v) {
			SetupVersion::Draft15Plus => {
				// Draft15+: No version field, parameters only.
			}
			SetupVersion::Draft14 | SetupVersion::LiteLegacy => self.version.encode(w, v)?,
			SetupVersion::Draft17 | SetupVersion::Unsupported => return Err(EncodeError::Version),
		};
		if w.remaining_mut() < self.parameters.len() {
			return Err(EncodeError::Short);
		}
		w.put_slice(&self.parameters);
		Ok(())
	}
}

impl Encode<Version> for Server {
	/// Encode a server setup message (draft-14 through draft-16 only).
	fn encode<W: bytes::BufMut>(&self, w: &mut W, v: Version) -> Result<(), EncodeError> {
		SERVER_SETUP.encode(w, v)?;

		let mut sizer = Sizer::default();
		self.encode_inner(&mut sizer, v)?;
		let size = sizer.size;

		match SetupVersion::from_version(v) {
			SetupVersion::Draft14 | SetupVersion::Draft15Plus => {
				u16::try_from(size).map_err(|_| EncodeError::TooLarge)?.encode(w, v)?;
			}
			SetupVersion::LiteLegacy => (size as u64).encode(w, v)?,
			SetupVersion::Draft17 | SetupVersion::Unsupported => return Err(EncodeError::Version),
		}

		self.encode_inner(w, v)
	}
}

impl Decode<Version> for Server {
	/// Decode a server setup message (draft-14 through draft-16 only).
	fn decode<R: bytes::Buf>(r: &mut R, v: Version) -> Result<Self, DecodeError> {
		let kind = u8::decode(r, v)?;
		if kind != SERVER_SETUP {
			return Err(DecodeError::InvalidValue);
		}

		let size = match SetupVersion::from_version(v) {
			SetupVersion::Draft14 | SetupVersion::Draft15Plus => u16::decode(r, v)? as usize,
			SetupVersion::LiteLegacy => u64::decode(r, v)? as usize,
			SetupVersion::Draft17 | SetupVersion::Unsupported => return Err(DecodeError::Version),
		};

		if r.remaining() < size {
			return Err(DecodeError::Short);
		}

		let mut msg = r.copy_to_bytes(size);
		let version = match SetupVersion::from_version(v) {
			SetupVersion::Draft15Plus => v.into(),
			SetupVersion::Draft14 | SetupVersion::LiteLegacy => coding::Version::decode(&mut msg, v)?,
			SetupVersion::Draft17 | SetupVersion::Unsupported => return Err(DecodeError::Version),
		};

		Ok(Self {
			version,
			parameters: msg,
		})
	}
}
