//! The SETUP exchange, the extensions a side offers in it, and the credential a peer may
//! present in it.
//!
//! Only [`Extensions`], [`Token`], and [`Role`] are public; the SETUP messages themselves
//! are wire internals.

use bytes::Bytes;

use crate::{
	Version,
	coding::{self, Decode, DecodeError, Decoder, Encode, EncodeError, Encoder},
	ietf, lite,
};

/// The session direction a client advertises in its SETUP (moq-lite-05+).
pub use crate::lite::Role;

// SETUP only carries negotiation parameters. Bound it independently of control messages.
pub(crate) const MAX_SETUP_SIZE: usize = u16::MAX as usize;

const CLIENT_SETUP: u8 = 0x20;
const SERVER_SETUP: u8 = 0x21;

/// Draft-17 unified SETUP message type (varint 0x2F00)
pub(crate) const SETUP_V17: u64 = 0x2F00;

/// Decode a SETUP Implementation value, refusing one that is not UTF-8.
pub(crate) fn decode_implementation(bytes: Option<&[u8]>) -> Result<Option<String>, crate::DecodeError> {
	bytes
		.map(|bytes| {
			std::str::from_utf8(bytes)
				.map(str::to_owned)
				.map_err(|_| crate::DecodeError::InvalidValue)
		})
		.transpose()
}

/// The implementation the peer advertised in SETUP, published once that SETUP is read.
///
/// `None` until then. The protocol holds a clone for the life of its driver, so a
/// dropped driver closes the channel; `Session::peer_implementation` also watches the
/// session's end, for a finished driver the caller keeps.
#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
#[derive(Clone, Default)]
pub(crate) struct PeerImplementation(kio::Producer<Option<Option<String>>>);

#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
impl PeerImplementation {
	/// A slot for a SETUP read before the session started, or a version that carries none.
	pub fn known(value: Option<String>) -> Self {
		Self(kio::Producer::new(Some(value)))
	}

	/// Record what the peer's SETUP advertised. The first SETUP wins.
	pub fn set(&self, value: Option<String>) {
		if let Ok(mut slot) = self.0.write()
			&& slot.is_none()
		{
			*slot = Some(value);
		}
	}

	pub(crate) fn consume(&self) -> kio::Consumer<Option<Option<String>>> {
		self.0.consume()
	}
}

#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
/// Our Implementation value to send, or `None` when the caller configured it empty.
pub(crate) fn implementation(value: &str) -> Option<String> {
	(!value.is_empty()).then(|| value.to_owned())
}

/// The SETUP extensions this side offers.
///
/// Turning one off connects as a peer that does not speak that extension. moq-lite carries
/// auth, solicit, and epoch in its core, so those only change moq-transport sessions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct Extensions {
	/// The MoQ Auth extension: tokens presented and granted on their own stream
	/// (moq-transport draft-17+ only).
	pub auth: bool,
	/// The MoQ Solicit extension: the peer answers our SUBSCRIBE_NAMESPACE rather than
	/// announcing unasked (moq-transport only).
	pub solicit: bool,
	/// The MoQ Epoch extension: announcements and requests carry the publisher's
	/// [`Epoch`](crate::Epoch), as moq-lite 07 does (moq-transport draft-17+ only).
	pub epoch: bool,
	/// The MoQ IETF extension, off by default: a moq-lite 07 hop between relays carries the
	/// moq-transport object state a relay preserves, such as Object IDs and properties.
	pub ietf: bool,
}

impl Default for Extensions {
	fn default() -> Self {
		Self {
			auth: true,
			solicit: true,
			epoch: true,
			ietf: false,
		}
	}
}

/// A connection credential presented in SETUP: moq-transport's `AUTHORIZATION TOKEN`
/// option, or moq-lite-07's Token parameter.
///
/// The transport never reads the bytes; verifying them is the application's job.
#[derive(Clone, PartialEq, Eq)]
pub struct Token {
	/// The wire Token Type, naming how [`value`](Self::value) is encoded.
	pub kind: u64,
	/// The token itself.
	pub value: Vec<u8>,
}

impl Token {
	/// Token Type 0: a format the endpoints agreed on out of band, such as a JWT.
	pub const OUT_OF_BAND: u64 = 0x0;
}

// SETUP and handshakes are logged with `Debug`, so the credential shows only its type and length.
impl std::fmt::Debug for Token {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "Token(kind={}, <{} bytes>)", self.kind, self.value.len())
	}
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
		w.control = Some(crate::stats::Control::Session);
		Self::check_version(v);
		w.varint(SETUP_V17)?;
		let prefix = w.prefix_u16();
		w.slice(&self.parameters);
		w.fill(prefix)
	}
}

impl Decode<Version> for Setup {
	fn decode(r: &mut Decoder<'_>, v: Version) -> Result<Self, DecodeError> {
		r.control = Some(crate::stats::Control::Session);
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
		r.control = Some(crate::stats::Control::Session);
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
			SetupVersion::Draft14 | SetupVersion::LiteLegacy => {
				coding::Versions::decode(&mut msg, v).map_err(DecodeError::complete)?
			}
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
		w.control = Some(crate::stats::Control::Session);
		w.u8(CLIENT_SETUP);
		let prefix = prefix_body(w, v)?;
		self.encode_inner(w, v)?;
		fill_body(w, prefix)
	}
}

/// Read a pre-draft-17 SETUP body: its size, then that many bytes.
fn decode_body<'a>(r: &mut Decoder<'a>, v: Version) -> Result<Decoder<'a>, DecodeError> {
	let size = match SetupVersion::from_version(v) {
		SetupVersion::Draft14 | SetupVersion::Draft15Plus => r.u16()? as usize,
		SetupVersion::LiteLegacy => usize::try_from(r.varint()?).map_err(|_| DecodeError::BoundsExceeded)?,
		SetupVersion::Modern | SetupVersion::Unsupported => return Err(DecodeError::Version),
	};
	if size > MAX_SETUP_SIZE {
		return Err(DecodeError::MessageTooLarge {
			size,
			max: MAX_SETUP_SIZE,
		});
	}
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

/// Size a pre-draft-17 SETUP body, refusing one our own receiver would refuse.
fn fill_body(w: &mut Encoder<'_>, prefix: coding::Prefix) -> Result<(), EncodeError> {
	if w.since(&prefix) > MAX_SETUP_SIZE {
		w.discard(prefix);
		return Err(EncodeError::TooLarge);
	}
	w.fill(prefix)
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
		w.control = Some(crate::stats::Control::Session);
		w.u8(SERVER_SETUP);
		let prefix = prefix_body(w, v)?;
		self.encode_inner(w, v)?;
		fill_body(w, prefix)
	}
}

impl Decode<Version> for Server {
	/// Decode a server setup message (draft-14 through draft-16 only).
	fn decode(r: &mut Decoder<'_>, v: Version) -> Result<Self, DecodeError> {
		r.control = Some(crate::stats::Control::Session);
		let kind = r.u8()?;
		if kind != SERVER_SETUP {
			return Err(DecodeError::InvalidValue);
		}

		let mut msg = decode_body(r, v)?;
		let version = match SetupVersion::from_version(v) {
			SetupVersion::Draft15Plus => v.into(),
			SetupVersion::Draft14 | SetupVersion::LiteLegacy => {
				coding::Version::decode(&mut msg, v).map_err(DecodeError::complete)?
			}
			SetupVersion::Modern | SetupVersion::Unsupported => return Err(DecodeError::Version),
		};

		Ok(Self {
			version,
			parameters: Bytes::copy_from_slice(msg.rest()),
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;

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
		assert!(matches!(
			client.encode(&mut Encoder::new(&mut buf, v.into()), v),
			Err(EncodeError::TooLarge)
		));

		let server = Server {
			version: v.into(),
			parameters,
		};
		let mut buf = Vec::new();
		assert!(matches!(
			server.encode(&mut Encoder::new(&mut buf, v.into()), v),
			Err(EncodeError::TooLarge)
		));
	}
}
