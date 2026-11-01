//! The SETUP exchange, and the credential a peer may present in it.
//!
//! Only [`Token`] is public; the SETUP messages themselves are wire internals.

use bytes::Bytes;

use crate::{
	Version,
	coding::{self, Decode, DecodeError, Encode, EncodeError, Sizer},
	ietf, lite,
};

// SETUP only carries negotiation parameters. Bound it independently of control messages.
pub(crate) const MAX_SETUP_SIZE: usize = 64 * 1024;

const CLIENT_SETUP: u8 = 0x20;
const SERVER_SETUP: u8 = 0x21;

/// Draft-17 unified SETUP message type (varint 0x2F00)
pub(crate) const SETUP_V17: u64 = 0x2F00;

/// A credential a moq-transport peer presented in its SETUP's `AUTHORIZATION TOKEN` option.
///
/// The transport never reads the bytes; verifying them is the application's job.
#[derive(Clone, PartialEq, Eq)]
pub struct Token {
	/// The wire Token Type, naming how [`value`](Self::value) is encoded.
	pub kind: u64,
	/// The token itself.
	pub value: Vec<u8>,
}

impl std::fmt::Debug for Token {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "Token(kind={}, <{} bytes>)", self.kind, self.value.len())
	}
}

impl Token {
	/// Token Type 0: a format the endpoints agreed on out of band, such as a JWT.
	pub const OUT_OF_BAND: u64 = 0x0;
	/// Token Type 1: a Common Access Token (draft-ietf-moq-c4m).
	pub const CAT: u64 = 0x1;
}

/// Draft-17+ unified SETUP message, with the same encoding for both client and server.
#[derive(Clone)]
pub(crate) struct Setup {
	pub parameters: Bytes,
}

impl std::fmt::Debug for Setup {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		// Opaque parameters can contain credentials, even before we recognize an option.
		f.debug_struct("Setup")
			.field("parameters_len", &self.parameters.len())
			.finish()
	}
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
	fn control(&self, _: Version) -> Option<crate::stats::Control> {
		Some(crate::stats::Control::Session)
	}

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
	fn control(&self, _: Version) -> Option<crate::stats::Control> {
		Some(crate::stats::Control::Session)
	}

	fn decode<R: bytes::Buf>(r: &mut R, v: Version) -> Result<Self, DecodeError> {
		Self::check_version(v);
		let kind = u64::decode(r, v)?;
		if kind != SETUP_V17 {
			return Err(DecodeError::InvalidValue);
		}
		// The fixed u16 length is already below MAX_SETUP_SIZE.
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
#[derive(Clone)]
pub(crate) struct Client {
	/// The list of supported versions in preferred order.
	pub versions: coding::Versions,

	/// Parameters, unparsed because the IETF draft changed the encoding.
	pub parameters: Bytes,
}

impl std::fmt::Debug for Client {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Client")
			.field("versions", &self.versions)
			.field("parameters_len", &self.parameters.len())
			.finish()
	}
}

impl Client {
	fn encode_inner<W: bytes::BufMut>(&self, w: &mut W, v: Version) -> Result<(), EncodeError> {
		match SetupVersion::from_version(v) {
			SetupVersion::Draft15Plus => {
				// Draft15+: no versions list, parameters only.
			}
			SetupVersion::Draft14 | SetupVersion::LiteLegacy => self.versions.encode(w, v)?,
			SetupVersion::Modern | SetupVersion::Unsupported => return Err(EncodeError::Version),
		};
		if w.remaining_mut() < self.parameters.len() {
			return Err(EncodeError::Short);
		}
		w.put_slice(&self.parameters);
		Ok(())
	}
}

impl Decode<Version> for Client {
	fn control(&self, _: Version) -> Option<crate::stats::Control> {
		Some(crate::stats::Control::Session)
	}

	/// Decode a client setup message (draft-14 through draft-16 only).
	fn decode<R: bytes::Buf>(r: &mut R, v: Version) -> Result<Self, DecodeError> {
		let kind = u8::decode(r, v)?;
		if kind != CLIENT_SETUP {
			return Err(DecodeError::InvalidValue);
		}

		let size = match SetupVersion::from_version(v) {
			SetupVersion::Draft14 | SetupVersion::Draft15Plus => u16::decode(r, v)? as usize,
			SetupVersion::LiteLegacy => usize::decode(r, v)?,
			SetupVersion::Modern | SetupVersion::Unsupported => return Err(DecodeError::Version),
		};

		if size > MAX_SETUP_SIZE {
			return Err(DecodeError::MessageTooLarge {
				size,
				max: MAX_SETUP_SIZE,
			});
		}

		if r.remaining() < size {
			return Err(DecodeError::Short);
		}

		let mut msg = r.copy_to_bytes(size);

		let versions = match SetupVersion::from_version(v) {
			SetupVersion::Draft15Plus => {
				// Draft15+: no versions list, parameters only.
				coding::Versions::from([v.into()])
			}
			SetupVersion::Draft14 | SetupVersion::LiteLegacy => {
				coding::Versions::decode(&mut msg, v).map_err(DecodeError::complete)?
			}
			SetupVersion::Modern | SetupVersion::Unsupported => return Err(DecodeError::Version),
		};

		Ok(Self {
			versions,
			parameters: msg,
		})
	}
}

impl Encode<Version> for Client {
	fn control(&self, _: Version) -> Option<crate::stats::Control> {
		Some(crate::stats::Control::Session)
	}

	/// Encode a client setup message (draft-14 through draft-16 only).
	fn encode<W: bytes::BufMut>(&self, w: &mut W, v: Version) -> Result<(), EncodeError> {
		CLIENT_SETUP.encode(w, v)?;

		let mut sizer = Sizer::default();
		self.encode_inner(&mut sizer, v)?;
		let size = sizer.size;
		if size > MAX_SETUP_SIZE {
			return Err(EncodeError::TooLarge);
		}

		match SetupVersion::from_version(v) {
			SetupVersion::Draft14 | SetupVersion::Draft15Plus => {
				u16::try_from(size).map_err(|_| EncodeError::TooLarge)?.encode(w, v)?;
			}
			SetupVersion::LiteLegacy => (size as u64).encode(w, v)?,
			SetupVersion::Modern | SetupVersion::Unsupported => return Err(EncodeError::Version),
		}
		self.encode_inner(w, v)
	}
}

/// Sent by the server in response to a client setup.
#[derive(Clone)]
pub(crate) struct Server {
	/// The list of supported versions in preferred order.
	pub version: coding::Version,

	/// Supported extensions.
	pub parameters: Bytes,
}

impl std::fmt::Debug for Server {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Server")
			.field("version", &self.version)
			.field("parameters_len", &self.parameters.len())
			.finish()
	}
}

impl Server {
	fn encode_inner<W: bytes::BufMut>(&self, w: &mut W, v: Version) -> Result<(), EncodeError> {
		match SetupVersion::from_version(v) {
			SetupVersion::Draft15Plus => {
				// Draft15+: No version field, parameters only.
			}
			SetupVersion::Draft14 | SetupVersion::LiteLegacy => self.version.encode(w, v)?,
			SetupVersion::Modern | SetupVersion::Unsupported => return Err(EncodeError::Version),
		};
		if w.remaining_mut() < self.parameters.len() {
			return Err(EncodeError::Short);
		}
		w.put_slice(&self.parameters);
		Ok(())
	}
}

impl Encode<Version> for Server {
	fn control(&self, _: Version) -> Option<crate::stats::Control> {
		Some(crate::stats::Control::Session)
	}

	/// Encode a server setup message (draft-14 through draft-16 only).
	fn encode<W: bytes::BufMut>(&self, w: &mut W, v: Version) -> Result<(), EncodeError> {
		SERVER_SETUP.encode(w, v)?;

		let mut sizer = Sizer::default();
		self.encode_inner(&mut sizer, v)?;
		let size = sizer.size;
		if size > MAX_SETUP_SIZE {
			return Err(EncodeError::TooLarge);
		}

		match SetupVersion::from_version(v) {
			SetupVersion::Draft14 | SetupVersion::Draft15Plus => {
				u16::try_from(size).map_err(|_| EncodeError::TooLarge)?.encode(w, v)?;
			}
			SetupVersion::LiteLegacy => (size as u64).encode(w, v)?,
			SetupVersion::Modern | SetupVersion::Unsupported => return Err(EncodeError::Version),
		}

		self.encode_inner(w, v)
	}
}

impl Decode<Version> for Server {
	fn control(&self, _: Version) -> Option<crate::stats::Control> {
		Some(crate::stats::Control::Session)
	}

	/// Decode a server setup message (draft-14 through draft-16 only).
	fn decode<R: bytes::Buf>(r: &mut R, v: Version) -> Result<Self, DecodeError> {
		let kind = u8::decode(r, v)?;
		if kind != SERVER_SETUP {
			return Err(DecodeError::InvalidValue);
		}

		let size = match SetupVersion::from_version(v) {
			SetupVersion::Draft14 | SetupVersion::Draft15Plus => u16::decode(r, v)? as usize,
			SetupVersion::LiteLegacy => usize::decode(r, v)?,
			SetupVersion::Modern | SetupVersion::Unsupported => return Err(DecodeError::Version),
		};

		if size > MAX_SETUP_SIZE {
			return Err(DecodeError::MessageTooLarge {
				size,
				max: MAX_SETUP_SIZE,
			});
		}

		if r.remaining() < size {
			return Err(DecodeError::Short);
		}

		let mut msg = r.copy_to_bytes(size);
		let version = match SetupVersion::from_version(v) {
			SetupVersion::Draft15Plus => v.into(),
			SetupVersion::Draft14 | SetupVersion::LiteLegacy => {
				coding::Version::decode(&mut msg, v).map_err(DecodeError::complete)?
			}
			SetupVersion::Modern | SetupVersion::Unsupported => return Err(DecodeError::Version),
		};

		Ok(Self {
			version,
			parameters: msg,
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	#[tracing_test::traced_test]
	fn credentials_stay_out_of_traces() {
		let secret = "credential-that-must-not-be-logged";
		let token = Token {
			kind: Token::OUT_OF_BAND,
			value: secret.as_bytes().to_vec(),
		};
		let setup = lite::Setup {
			path: Some(format!("/room?jwt={secret}")),
			..Default::default()
		};
		let wire = setup.encode_bytes(lite::Version::Lite06).unwrap();
		let decoded = lite::Setup::decode(&mut wire.as_ref(), lite::Version::Lite06).unwrap();
		assert_eq!(decoded, setup);

		let version = ietf::Version::Draft22;
		let mut params = ietf::Parameters::default();
		ietf::token::into_setup(&mut params, &token, version).unwrap();
		params.set_bytes(ietf::ParameterBytes::Path, format!("/room?jwt={secret}").into_bytes());
		let setup = Setup {
			parameters: params.encode_bytes(version).unwrap(),
		};
		let wire = setup.encode_bytes(Version::Ietf(version)).unwrap();
		let decoded = Setup::decode(&mut wire.as_ref(), Version::Ietf(version)).unwrap();
		let params = ietf::Parameters::decode(&mut decoded.parameters.as_ref(), version).unwrap();
		assert_eq!(ietf::token::from_setup(&params, version).unwrap(), Some(token.clone()));
		let client = Client {
			versions: coding::Versions::from([Version::Ietf(version).into()]),
			parameters: setup.parameters.clone(),
		};
		let server = Server {
			version: Version::Ietf(version).into(),
			parameters: setup.parameters.clone(),
		};
		let mut lite_params = lite::Parameters::default();
		lite_params.set_bytes(999, secret.as_bytes().to_vec());
		tracing::trace!(
			?token,
			?setup,
			?decoded,
			?params,
			?client,
			?server,
			?lite_params,
			"SETUP diagnostics"
		);

		let uri = format!("https://relay.example/room?jwt={secret}");
		let goaway = lite::Goaway {
			uri: uri.as_str().into(),
		};
		let wire = goaway.encode_bytes(lite::Version::Lite06).unwrap();
		assert_eq!(
			lite::Goaway::decode(&mut wire.as_ref(), lite::Version::Lite06)
				.unwrap()
				.uri,
			uri
		);
		let goaway = ietf::GoAway {
			new_session_uri: uri.as_str().into(),
			timeout: 0,
		};
		let wire = goaway.encode_bytes(version).unwrap();
		assert_eq!(
			ietf::GoAway::decode(&mut wire.as_ref(), version)
				.unwrap()
				.new_session_uri,
			uri
		);
		tracing::trace!(goaway = ?crate::goaway::Goaway::redirect(uri), "GOAWAY diagnostics");

		// A malformed message can still contain a valid credential before the error.
		let mut body = Vec::new();
		(secret.len() + 1).encode(&mut body, lite::Version::Lite06).unwrap();
		body.extend_from_slice(secret.as_bytes());
		let mut malformed = Vec::new();
		body.len().encode(&mut malformed, lite::Version::Lite06).unwrap();
		malformed.extend_from_slice(&body);
		assert!(lite::Goaway::decode(&mut malformed.as_slice(), lite::Version::Lite06).is_err());
		let mut body = Vec::new();
		// The required timeout is absent.
		format!("https://relay.example/?jwt={secret}")
			.encode(&mut body, version)
			.unwrap();
		let mut malformed = Vec::new();
		(body.len() as u16).encode(&mut malformed, version).unwrap();
		malformed.extend_from_slice(&body);
		assert!(ietf::GoAway::decode(&mut malformed.as_slice(), version).is_err());

		assert!(logs_contain("encoding"));
		assert!(logs_contain("decoded"));
		assert!(logs_contain("SETUP diagnostics"));
		assert!(logs_contain("GOAWAY diagnostics"));
		assert!(!logs_contain(secret));
		let decimal = format!("{:?}", secret.as_bytes());
		assert!(!logs_contain(&decimal[1..decimal.len() - 1]));
	}

	/// Never emit a legacy SETUP our own receiver would refuse.
	#[test]
	fn encode_enforces_the_setup_limit() {
		let v = Version::Lite(lite::Version::Lite01);
		let parameters = Bytes::from(vec![0; MAX_SETUP_SIZE]);

		let client = Client {
			versions: coding::Versions::from([v.into()]),
			parameters: parameters.clone(),
		};
		let mut buf = Vec::new();
		assert!(matches!(client.encode(&mut buf, v), Err(EncodeError::TooLarge)));

		let server = Server {
			version: v.into(),
			parameters,
		};
		let mut buf = Vec::new();
		assert!(matches!(server.encode(&mut buf, v), Err(EncodeError::TooLarge)));
	}
}
