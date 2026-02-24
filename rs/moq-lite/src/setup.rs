use bytes::Bytes;

use crate::{
	coding::{self, Decode, DecodeError, Encode, Sizer},
	ietf, lite,
	version::Version,
};

const CLIENT_SETUP: u8 = 0x20;
const SERVER_SETUP: u8 = 0x21;

/// A version-agnostic setup message sent by the client.
#[derive(Debug, Clone)]
pub struct Client {
	/// The list of supported versions in preferred order.
	pub versions: coding::Versions,

	/// Parameters, unparsed because the IETF draft changed the encoding.
	pub parameters: Bytes,
}

impl Client {
	fn encode_inner<W: bytes::BufMut>(&self, w: &mut W, v: Version) {
		match v {
			Version::Ietf(ietf::Version::Draft15 | ietf::Version::Draft16) => {
				// Draft15+: no versions list, parameters only.
			}
			Version::Ietf(ietf::Version::Draft14)
			| Version::Lite(lite::Version::Draft02)
			| Version::Lite(lite::Version::Draft01) => self.versions.encode(w, v),
			Version::Lite(lite::Version::Draft03) => unreachable!("draft-03 uses no setup message"),
		};
		w.put_slice(&self.parameters);
	}
}

impl Decode<Version> for Client {
	/// Decode a client setup message.
	fn decode<R: bytes::Buf>(r: &mut R, v: Version) -> Result<Self, DecodeError> {
		let kind = u8::decode(r, v)?;
		if kind != CLIENT_SETUP {
			return Err(DecodeError::InvalidValue);
		}

		let size = match v {
			Version::Ietf(ietf::Version::Draft14 | ietf::Version::Draft15 | ietf::Version::Draft16) => {
				u16::decode(r, v)? as usize
			}
			Version::Lite(lite::Version::Draft02 | lite::Version::Draft01) => u64::decode(r, v)? as usize,
			Version::Lite(lite::Version::Draft03) => unreachable!("draft-03 uses no setup message"),
		};

		if r.remaining() < size {
			return Err(DecodeError::Short);
		}

		let mut msg = r.copy_to_bytes(size);

		let versions = match v {
			Version::Ietf(ietf::Version::Draft15 | ietf::Version::Draft16) => {
				// Draft15+: no versions list, parameters only.
				coding::Versions::from([v.into()])
			}
			Version::Ietf(ietf::Version::Draft14)
			| Version::Lite(lite::Version::Draft02)
			| Version::Lite(lite::Version::Draft01) => coding::Versions::decode(&mut msg, v)?,
			Version::Lite(lite::Version::Draft03) => unreachable!("draft-03 uses no setup message"),
		};

		Ok(Self {
			versions,
			parameters: msg,
		})
	}
}

impl Encode<Version> for Client {
	/// Encode a client setup message.
	fn encode<W: bytes::BufMut>(&self, w: &mut W, v: Version) {
		CLIENT_SETUP.encode(w, v);

		let mut sizer = Sizer::default();
		self.encode_inner(&mut sizer, v);
		let size = sizer.size;

		match v {
			Version::Ietf(ietf::Version::Draft14 | ietf::Version::Draft15 | ietf::Version::Draft16) => {
				u16::try_from(size).expect("message too large for u16").encode(w, v)
			}
			Version::Lite(lite::Version::Draft02 | lite::Version::Draft01) => (size as u64).encode(w, v),
			Version::Lite(lite::Version::Draft03) => unreachable!("draft-03 uses no setup message"),
		}
		self.encode_inner(w, v);
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
	fn encode_inner<W: bytes::BufMut>(&self, w: &mut W, v: Version) {
		match v {
			Version::Ietf(ietf::Version::Draft15 | ietf::Version::Draft16) => {
				// Draft15+: No version field, parameters only.
			}
			Version::Ietf(ietf::Version::Draft14)
			| Version::Lite(lite::Version::Draft02)
			| Version::Lite(lite::Version::Draft01) => self.version.encode(w, v),
			Version::Lite(lite::Version::Draft03) => unreachable!("draft-03 uses no setup message"),
		};
		w.put_slice(&self.parameters);
	}
}

impl Encode<Version> for Server {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, v: Version) {
		SERVER_SETUP.encode(w, v);

		let mut sizer = Sizer::default();
		self.encode_inner(&mut sizer, v);
		let size = sizer.size;

		match v {
			Version::Ietf(ietf::Version::Draft14 | ietf::Version::Draft15 | ietf::Version::Draft16) => {
				u16::try_from(size).expect("message too large for u16").encode(w, v)
			}
			Version::Lite(lite::Version::Draft02 | lite::Version::Draft01) => (size as u64).encode(w, v),
			Version::Lite(lite::Version::Draft03) => unreachable!("draft-03 uses no setup message"),
		}

		self.encode_inner(w, v);
	}
}

impl Decode<Version> for Server {
	fn decode<R: bytes::Buf>(r: &mut R, v: Version) -> Result<Self, DecodeError> {
		let kind = u8::decode(r, v)?;
		if kind != SERVER_SETUP {
			return Err(DecodeError::InvalidValue);
		}

		let size = match v {
			Version::Ietf(ietf::Version::Draft14 | ietf::Version::Draft15 | ietf::Version::Draft16) => {
				u16::decode(r, v)? as usize
			}
			Version::Lite(lite::Version::Draft02 | lite::Version::Draft01) => u64::decode(r, v)? as usize,
			Version::Lite(lite::Version::Draft03) => unreachable!("draft-03 uses no setup message"),
		};

		if r.remaining() < size {
			return Err(DecodeError::Short);
		}

		let mut msg = r.copy_to_bytes(size);
		let version = match v {
			Version::Ietf(ietf::Version::Draft15 | ietf::Version::Draft16) => v.into(),
			Version::Ietf(ietf::Version::Draft14)
			| Version::Lite(lite::Version::Draft02)
			| Version::Lite(lite::Version::Draft01) => coding::Version::decode(&mut msg, v)?,
			Version::Lite(lite::Version::Draft03) => unreachable!("draft-03 uses no setup message"),
		};

		Ok(Self {
			version,
			parameters: msg,
		})
	}
}
