//! The moq-lite-05 SETUP message and its unidirectional Setup stream.
//!
//! Each endpoint opens one Setup stream at the start of the session, sends a single
//! SETUP message advertising its optional capabilities, and closes it (FIN). Unknown
//! parameters are ignored so new ones stay backward compatible.

use crate::{Error, coding::*};

use super::{DataType, Message, Parameters, Version};

/// Setup parameter ID for the request Path (client-only, on URI-less transports).
const PARAM_PATH: u64 = 0x2;

/// The SETUP message, sent once per endpoint on a Setup stream (moq-lite-05+).
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct Setup {
	/// The request path, sent by a client on a transport binding that carries no
	/// request URI (native QUIC, qmux over TCP/TLS/UDS). When present it begins with
	/// `/`. A server never sends one, and it is absent on URI-carrying bindings
	/// (WebTransport), which already convey the path.
	pub path: Option<String>,
}

impl Message for Setup {
	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		if !version.has_setup_stream() {
			return Err(EncodeError::Version);
		}

		let mut params = Parameters::default();
		if let Some(path) = &self.path {
			// The path must be an absolute URI path; reject malformed values rather
			// than emitting something a peer must close the session over.
			if !path.starts_with('/') {
				return Err(EncodeError::InvalidState);
			}
			params.set(PARAM_PATH, path.clone().into_bytes());
		}

		params.encode(w, version)
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		if !version.has_setup_stream() {
			return Err(DecodeError::Version);
		}

		let params = Parameters::decode(r, version)?;

		let path = match params.get(PARAM_PATH) {
			Some(bytes) => {
				let s = std::str::from_utf8(bytes).map_err(|_| DecodeError::InvalidValue)?;
				// Must be an absolute URI path; URL-carrying transports can never
				// produce anything else, and the relay scopes auth from it.
				if !s.starts_with('/') {
					return Err(DecodeError::InvalidValue);
				}
				Some(s.to_string())
			}
			None => None,
		};

		Ok(Self { path })
	}
}

/// Open a Setup stream, send a single SETUP message, and close it (FIN).
///
/// Called once on connect; each endpoint opens exactly one Setup stream.
pub(crate) async fn send_setup<S: web_transport_trait::Session>(
	session: &S,
	version: Version,
	setup: Setup,
) -> Result<(), Error> {
	let stream = session.open_uni().await.map_err(Error::from_transport)?;

	let mut writer = Writer::new(stream, version);
	writer.encode(&DataType::Setup).await?;
	writer.encode(&setup).await?;
	writer.finish()?;

	Ok(())
}

/// Read the peer's SETUP off its Setup stream, returning once it arrives.
///
/// Used on the server to learn the client's request path before deciding what to
/// serve. Any data stream (e.g. a Group) that races ahead of the Setup stream is
/// reset and skipped; the peer sends exactly one SETUP, so this resolves quickly.
pub(crate) async fn accept_setup<S: web_transport_trait::Session>(
	session: &S,
	version: Version,
) -> Result<Setup, Error> {
	loop {
		let recv = session.accept_uni().await.map_err(Error::from_transport)?;
		let mut reader = Reader::new(recv, version);

		match reader.decode::<DataType>().await? {
			DataType::Setup => return reader.decode().await,
			DataType::Group => reader.abort(&Error::Cancel),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use bytes::BytesMut;

	fn roundtrip(setup: &Setup) -> Setup {
		let mut buf = BytesMut::new();
		setup.encode(&mut buf, Version::Lite05Wip).unwrap();
		let mut slice = &buf[..];
		let decoded = Setup::decode(&mut slice, Version::Lite05Wip).unwrap();
		assert!(bytes::Buf::remaining(&slice) == 0, "trailing bytes after decode");
		decoded
	}

	#[test]
	fn empty() {
		assert_eq!(roundtrip(&Setup::default()), Setup::default());
	}

	#[test]
	fn with_path() {
		let setup = Setup {
			path: Some("/live/room".to_string()),
		};
		assert_eq!(roundtrip(&setup), setup);
	}

	#[test]
	fn rejects_relative_path() {
		// Encoding a non-absolute path fails rather than emitting bad wire bytes.
		let mut buf = BytesMut::new();
		assert!(matches!(
			Setup {
				path: Some("foo".into())
			}
			.encode(&mut buf, Version::Lite05Wip),
			Err(EncodeError::InvalidState)
		));

		// Decoding a hand-rolled relative path is rejected at the wire boundary.
		let mut params = Parameters::default();
		params.set(PARAM_PATH, b"foo".to_vec());
		let mut body = BytesMut::new();
		params.encode(&mut body, Version::Lite05Wip).unwrap();
		let mut framed = BytesMut::new();
		(body.len() as u64).encode(&mut framed, Version::Lite05Wip).unwrap();
		framed.extend_from_slice(&body);
		assert!(matches!(
			Setup::decode(&mut framed, Version::Lite05Wip),
			Err(DecodeError::InvalidValue)
		));
	}

	#[test]
	fn rejects_before_lite05() {
		let mut buf = BytesMut::new();
		assert!(matches!(
			Setup::default().encode(&mut buf, Version::Lite04),
			Err(EncodeError::Version)
		));
	}

	#[test]
	fn ignores_unknown_parameters() {
		// Encode a SETUP carrying an unknown parameter ID alongside the path.
		let mut params = Parameters::default();
		params.set(PARAM_PATH, b"/foo".to_vec());
		params.set(0xbeef, b"whatever".to_vec());

		let mut body = BytesMut::new();
		params.encode(&mut body, Version::Lite05Wip).unwrap();

		// Wrap with the message size prefix the Message impl expects.
		let mut buf = BytesMut::new();
		(body.len() as u64).encode(&mut buf, Version::Lite05Wip).unwrap();
		buf.extend_from_slice(&body);

		let decoded = Setup::decode(&mut buf, Version::Lite05Wip).unwrap();
		assert_eq!(decoded.path.as_deref(), Some("/foo"));
	}
}
