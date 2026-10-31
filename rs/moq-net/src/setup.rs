//! The SETUP exchange, the extensions a side offers in it, and the credential a peer may
//! present in it.
//!
//! Only [`Extensions`], [`Token`], and [`Role`] are public; the SETUP messages themselves
//! are wire internals.

use bytes::Bytes;

use crate::{
	Version,
	coding::{self, Decode, DecodeError, Decoder, Encode, EncodeError, Encoder},
	lite,
};

/// The session direction a client advertises in its SETUP (moq-lite-05+).
pub use crate::lite::Role;

// SETUP only carries negotiation parameters. Bound it independently of control messages.
pub(crate) const MAX_SETUP_SIZE: usize = u16::MAX as usize;

const CLIENT_SETUP: u8 = 0x20;
const SERVER_SETUP: u8 = 0x21;

/// The moq-transport SETUP message type (varint 0x2F00).
pub(crate) const SETUP: u64 = 0x2F00;

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
	/// The MoQ Auth extension: tokens presented and granted on their own stream.
	pub auth: bool,
	/// The MoQ Solicit extension: the peer answers our SUBSCRIBE_NAMESPACE rather than
	/// announcing unasked (moq-transport only).
	pub solicit: bool,
	/// The MoQ Epoch extension: announcements and requests carry the publisher's
	/// [`Epoch`](crate::Epoch), as moq-lite 07 does.
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

/// The unified SETUP message, with the same encoding for both client and server.
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

impl Encode<Version> for Setup {
	fn encode(&self, w: &mut Encoder<'_>, v: Version) -> Result<(), EncodeError> {
		w.control = Some(crate::stats::Message::Session);
		if v.is_lite() {
			return Err(EncodeError::Version);
		}
		w.varint(SETUP)?;
		let prefix = w.prefix_u16();
		w.slice(&self.parameters);
		w.fill(prefix)
	}
}

impl<'a> Decode<'a, Version> for Setup {
	fn decode(r: &mut Decoder<'a>, v: Version) -> Result<Self, DecodeError> {
		r.control = Some(crate::stats::Message::Session);
		if v.is_lite() {
			return Err(DecodeError::Version);
		}
		let kind = r.varint()?;
		if kind != SETUP {
			return Err(DecodeError::InvalidValue);
		}
		let size = r.u16()? as usize;
		let parameters = Bytes::copy_from_slice(r.slice(size)?);
		Ok(Self { parameters })
	}
}

/// Whether `v` encodes the `moql` bidi SETUP exchange: moq-lite 01/02, whose draft-14 framing
/// and QUIC varints it keeps whichever moq-lite version it negotiates.
fn framed(v: Version) -> bool {
	matches!(v, Version::Lite(lite::Version::Lite01 | lite::Version::Lite02))
}

/// The client half of the `moql` bidi SETUP exchange, offering moq-lite versions.
///
/// The framing is moq-transport draft-14's CLIENT_SETUP, which moq-lite borrowed: a
/// u16 size, the offered versions, then parameters.
#[derive(Clone)]
pub(crate) struct Client {
	/// The list of supported versions in preferred order.
	pub versions: coding::Versions,

	/// Parameters, unparsed: moq-lite defines none here, so neither side reads them.
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

impl<'a> Decode<'a, Version> for Client {
	fn decode(r: &mut Decoder<'a>, v: Version) -> Result<Self, DecodeError> {
		r.control = Some(crate::stats::Message::Session);
		if !framed(v) {
			return Err(DecodeError::Version);
		}
		let kind = r.u8()?;
		if kind != CLIENT_SETUP {
			return Err(DecodeError::InvalidValue);
		}

		let size = r.u16()? as usize;
		let mut msg = r.sub(size)?;
		let versions = coding::Versions::decode(&mut msg, v).map_err(DecodeError::complete)?;

		Ok(Self {
			versions,
			parameters: Bytes::copy_from_slice(msg.rest()),
		})
	}
}

impl Encode<Version> for Client {
	fn encode(&self, w: &mut Encoder<'_>, v: Version) -> Result<(), EncodeError> {
		w.control = Some(crate::stats::Message::Session);
		if !framed(v) {
			return Err(EncodeError::Version);
		}
		w.u8(CLIENT_SETUP);
		let prefix = w.prefix_u16();
		self.versions.encode(w, v)?;
		w.slice(&self.parameters);
		fill_body(w, prefix)
	}
}

/// Size a SETUP body, refusing one our own receiver would refuse.
fn fill_body(w: &mut Encoder<'_>, prefix: coding::Prefix) -> Result<(), EncodeError> {
	if w.since(&prefix) > MAX_SETUP_SIZE {
		w.discard(prefix);
		return Err(EncodeError::TooLarge);
	}
	w.fill(prefix)
}

/// The server half of the `moql` bidi SETUP exchange, selecting a moq-lite version.
///
/// The framing is moq-transport draft-14's SERVER_SETUP: a u16 size, the selected version,
/// then parameters.
#[derive(Clone)]
pub(crate) struct Server {
	/// The selected version.
	pub version: coding::Version,

	/// Parameters, unparsed: moq-lite defines none here, so neither side reads them.
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

impl Encode<Version> for Server {
	fn encode(&self, w: &mut Encoder<'_>, v: Version) -> Result<(), EncodeError> {
		w.control = Some(crate::stats::Message::Session);
		if !framed(v) {
			return Err(EncodeError::Version);
		}
		w.u8(SERVER_SETUP);
		let prefix = w.prefix_u16();
		self.version.encode(w, v)?;
		w.slice(&self.parameters);
		fill_body(w, prefix)
	}
}

impl<'a> Decode<'a, Version> for Server {
	fn decode(r: &mut Decoder<'a>, v: Version) -> Result<Self, DecodeError> {
		r.control = Some(crate::stats::Message::Session);
		if !framed(v) {
			return Err(DecodeError::Version);
		}
		let kind = r.u8()?;
		if kind != SERVER_SETUP {
			return Err(DecodeError::InvalidValue);
		}

		let size = r.u16()? as usize;
		let mut msg = r.sub(size)?;
		let version = coding::Version::decode(&mut msg, v).map_err(DecodeError::complete)?;

		Ok(Self {
			version,
			parameters: Bytes::copy_from_slice(msg.rest()),
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
			token: Some(token.clone()),
			..Default::default()
		};
		let wire = setup.encode_bytes(lite::Version::Lite07).unwrap();
		assert_eq!(
			lite::Setup::decode_slice(&wire, lite::Version::Lite07).unwrap().0,
			setup
		);
		let auth = lite::Auth {
			token: Bytes::copy_from_slice(secret.as_bytes()),
		};
		let wire = auth.encode_bytes(lite::Version::Lite07).unwrap();
		assert_eq!(lite::Auth::decode_slice(&wire, lite::Version::Lite07).unwrap().0, auth);

		let version = crate::ietf::Version::Draft22;
		let auth = crate::ietf::auth::Auth {
			request_id: crate::ietf::RequestId(0),
			token: Bytes::copy_from_slice(secret.as_bytes()),
		};
		let wire = auth.encode_bytes(version).unwrap();
		assert_eq!(crate::ietf::auth::Auth::decode_slice(&wire, version).unwrap().0, auth);
		let mut params = crate::ietf::Parameters::default();
		crate::ietf::token::into_setup(&mut params, &token, version).unwrap();
		params.set_bytes(
			crate::ietf::ParameterBytes::Path,
			format!("/room?jwt={secret}").into_bytes(),
		);
		let setup = Setup {
			parameters: params.encode_bytes(version).unwrap(),
		};
		let wire = setup.encode_bytes(crate::Version::Ietf(version)).unwrap();
		let decoded = Setup::decode_slice(&wire, crate::Version::Ietf(version)).unwrap().0;
		let (params, _) = crate::ietf::Parameters::decode_slice(&decoded.parameters, version).unwrap();
		assert_eq!(crate::ietf::token::from_setup(&params, version).unwrap(), Some(token));
		tracing::trace!(?setup, ?decoded, ?params, "SETUP diagnostics");

		let uri = format!("https://relay.example/room?jwt={secret}");
		let goaway = lite::Goaway {
			uri: uri.as_str().into(),
		};
		let wire = goaway.encode_bytes(lite::Version::Lite07).unwrap();
		lite::Goaway::decode_slice(&wire, lite::Version::Lite07).unwrap();
		let goaway = crate::ietf::GoAway {
			new_session_uri: uri.as_str().into(),
			timeout: 0,
			request_id: Some(crate::ietf::RequestId(0)),
		};
		let wire = goaway.encode_bytes(version).unwrap();
		crate::ietf::GoAway::decode_slice(&wire, version).unwrap();
		tracing::trace!(goaway = ?crate::goaway::Goaway::redirect(uri), "GOAWAY diagnostics");

		assert!(logs_contain("encoding"));
		assert!(logs_contain("decoded"));
		assert!(logs_contain("SETUP diagnostics"));
		assert!(logs_contain("GOAWAY diagnostics"));
		assert!(!logs_contain(secret));
		let decimal = format!("{:?}", secret.as_bytes());
		assert!(!logs_contain(&decimal[1..decimal.len() - 1]));
	}

	/// Deployed lite-01/02 peers frame both halves with draft-14's u16 size, not a varint.
	#[test]
	fn the_moql_setup_has_a_u16_size() {
		for v in [lite::Version::Lite01, lite::Version::Lite02] {
			let v = Version::Lite(v);
			let client = Client {
				versions: coding::Versions::from([v.into()]),
				parameters: Bytes::from_static(&[0]),
			};
			let buf = client.encode_bytes(v).unwrap();
			assert_eq!(buf[..3], [CLIENT_SETUP, 0, (buf.len() - 3) as u8], "{v}");
			assert_eq!(Client::decode_slice(&buf, v).unwrap().0.versions, client.versions);

			let server = Server {
				version: v.into(),
				parameters: Bytes::from_static(&[0]),
			};
			let buf = server.encode_bytes(v).unwrap();
			assert_eq!(buf[..3], [SERVER_SETUP, 0, (buf.len() - 3) as u8], "{v}");
			assert_eq!(Server::decode_slice(&buf, v).unwrap().0.version, server.version);
		}
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
