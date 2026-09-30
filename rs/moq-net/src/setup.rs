//! The SETUP exchange, and the credential a peer may present in it.
//!
//! Only [`Token`] is public; the SETUP messages themselves are wire internals.

use bytes::Bytes;

use crate::{
	Version,
	coding::{self, Decode, DecodeError, Decoder, Encode, EncodeError, Encoder},
	ietf, lite,
};

const CLIENT_SETUP: u8 = 0x20;
const SERVER_SETUP: u8 = 0x21;

/// Draft-17 unified SETUP message type (varint 0x2F00)
pub(crate) const SETUP_V17: u64 = 0x2F00;

/// A credential a moq-transport peer presented in its SETUP's `AUTHORIZATION TOKEN` option.
///
/// The transport never reads the bytes; verifying them is the application's job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
	/// The wire Token Type, naming how [`value`](Self::value) is encoded.
	pub kind: u64,
	/// The token itself.
	pub value: Vec<u8>,
}

impl Token {
	/// Token Type 0: a format the endpoints agreed on out of band, such as a JWT.
	pub const OUT_OF_BAND: u64 = 0x0;
	/// Token Type 1: a Common Access Token (draft-ietf-moq-c4m).
	pub const CAT: u64 = 0x1;
}

/// Draft-17+ unified SETUP message, with the same encoding for both client and server.
#[derive(Debug, Clone)]
pub(crate) struct Setup {
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
	fn encode(&self, w: &mut Encoder<'_>, v: Version) -> Result<(), EncodeError> {
		Self::check_version(v);
		w.varint(SETUP_V17)?;
		let prefix = w.prefix_u16();
		w.slice(&self.parameters);
		w.fill(prefix)
	}
}

impl Decode<Version> for Setup {
	fn decode(r: &mut Decoder<'_>, v: Version) -> Result<Self, DecodeError> {
		Self::check_version(v);
		let kind = r.varint()?;
		if kind != SETUP_V17 {
			return Err(DecodeError::InvalidValue);
		}
		let size = r.u16()? as usize;
		let parameters = Bytes::copy_from_slice(r.slice(size)?);
		Ok(Self { parameters })
	}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SetupVersion {
	Draft14,
	Draft15Plus,
	/// Draft17+ uses ALPN-only negotiation with no legacy SETUP message.
	Modern,
	LiteLegacy,
	Unsupported,
}

impl SetupVersion {
	fn from_version(v: Version) -> Self {
		match v {
			Version::Ietf(ietf::Version::Draft14) => Self::Draft14,
			Version::Ietf(ietf::Version::Draft15) | Version::Ietf(ietf::Version::Draft16) => Self::Draft15Plus,
			Version::Ietf(ietf::Version::Draft17)
			| Version::Ietf(ietf::Version::Draft18)
			| Version::Ietf(ietf::Version::Draft19)
			| Version::Ietf(ietf::Version::Draft20)
			| Version::Ietf(ietf::Version::Draft21)
			| Version::Ietf(ietf::Version::Draft22) => Self::Modern,
			Version::Lite(lite::Version::Lite01) | Version::Lite(lite::Version::Lite02) => Self::LiteLegacy,
			Version::Lite(_) => Self::Unsupported,
		}
	}
}

/// A version-agnostic setup message sent by the client.
#[derive(Debug, Clone)]
pub(crate) struct Client {
	/// The list of supported versions in preferred order.
	pub versions: coding::Versions,

	/// Parameters, unparsed because the IETF draft changed the encoding.
	pub parameters: Bytes,
}

impl Client {
	fn encode_inner(&self, w: &mut Encoder<'_>, v: Version) -> Result<(), EncodeError> {
		match SetupVersion::from_version(v) {
			SetupVersion::Draft15Plus => {
				// Draft15+: no versions list, parameters only.
			}
			SetupVersion::Draft14 | SetupVersion::LiteLegacy => self.versions.encode(w, v)?,
			SetupVersion::Modern | SetupVersion::Unsupported => return Err(EncodeError::Version),
		};
		w.slice(&self.parameters);
		Ok(())
	}
}

impl Decode<Version> for Client {
	/// Decode a client setup message (draft-14 through draft-16 only).
	fn decode(r: &mut Decoder<'_>, v: Version) -> Result<Self, DecodeError> {
		let kind = r.u8()?;
		if kind != CLIENT_SETUP {
			return Err(DecodeError::InvalidValue);
		}

		let mut msg = decode_body(r, v)?;

		let versions = match SetupVersion::from_version(v) {
			SetupVersion::Draft15Plus => {
				// Draft15+: no versions list, parameters only.
				coding::Versions::from([v.into()])
			}
			SetupVersion::Draft14 | SetupVersion::LiteLegacy => coding::Versions::decode(&mut msg, v)?,
			SetupVersion::Modern | SetupVersion::Unsupported => return Err(DecodeError::Version),
		};

		Ok(Self {
			versions,
			parameters: Bytes::copy_from_slice(msg.rest()),
		})
	}
}

impl Encode<Version> for Client {
	/// Encode a client setup message (draft-14 through draft-16 only).
	fn encode(&self, w: &mut Encoder<'_>, v: Version) -> Result<(), EncodeError> {
		w.u8(CLIENT_SETUP);
		let prefix = prefix_body(w, v)?;
		self.encode_inner(w, v)?;
		w.fill(prefix)
	}
}

/// Read a pre-draft-17 SETUP body: its size, then that many bytes.
fn decode_body<'a>(r: &mut Decoder<'a>, v: Version) -> Result<Decoder<'a>, DecodeError> {
	let size = match SetupVersion::from_version(v) {
		SetupVersion::Draft14 | SetupVersion::Draft15Plus => r.u16()? as usize,
		SetupVersion::LiteLegacy => usize::try_from(r.varint()?).map_err(|_| DecodeError::BoundsExceeded)?,
		SetupVersion::Modern | SetupVersion::Unsupported => return Err(DecodeError::Version),
	};
	r.sub(size)
}

/// Reserve the size prefix of a pre-draft-17 SETUP body.
fn prefix_body(w: &mut Encoder<'_>, v: Version) -> Result<coding::Prefix, EncodeError> {
	match SetupVersion::from_version(v) {
		SetupVersion::Draft14 | SetupVersion::Draft15Plus => Ok(w.prefix_u16()),
		SetupVersion::LiteLegacy => Ok(w.prefix_varint()),
		SetupVersion::Modern | SetupVersion::Unsupported => Err(EncodeError::Version),
	}
}

/// Sent by the server in response to a client setup.
#[derive(Debug, Clone)]
pub(crate) struct Server {
	/// The list of supported versions in preferred order.
	pub version: coding::Version,

	/// Supported extensions.
	pub parameters: Bytes,
}

impl Server {
	fn encode_inner(&self, w: &mut Encoder<'_>, v: Version) -> Result<(), EncodeError> {
		match SetupVersion::from_version(v) {
			SetupVersion::Draft15Plus => {
				// Draft15+: No version field, parameters only.
			}
			SetupVersion::Draft14 | SetupVersion::LiteLegacy => self.version.encode(w, v)?,
			SetupVersion::Modern | SetupVersion::Unsupported => return Err(EncodeError::Version),
		};
		w.slice(&self.parameters);
		Ok(())
	}
}

impl Encode<Version> for Server {
	/// Encode a server setup message (draft-14 through draft-16 only).
	fn encode(&self, w: &mut Encoder<'_>, v: Version) -> Result<(), EncodeError> {
		w.u8(SERVER_SETUP);
		let prefix = prefix_body(w, v)?;
		self.encode_inner(w, v)?;
		w.fill(prefix)
	}
}

impl Decode<Version> for Server {
	/// Decode a server setup message (draft-14 through draft-16 only).
	fn decode(r: &mut Decoder<'_>, v: Version) -> Result<Self, DecodeError> {
		let kind = r.u8()?;
		if kind != SERVER_SETUP {
			return Err(DecodeError::InvalidValue);
		}

		let mut msg = decode_body(r, v)?;
		let version = match SetupVersion::from_version(v) {
			SetupVersion::Draft15Plus => v.into(),
			SetupVersion::Draft14 | SetupVersion::LiteLegacy => coding::Version::decode(&mut msg, v)?,
			SetupVersion::Modern | SetupVersion::Unsupported => return Err(DecodeError::Version),
		};

		Ok(Self {
			version,
			parameters: Bytes::copy_from_slice(msg.rest()),
		})
	}
}
