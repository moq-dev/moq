use std::time::Duration;

use bytes::Bytes;

use crate::coding::*;
use crate::{Pattern, Patterns};

use super::{Message, Version};

/// The first message on an Auth Stream: the token the opener presents. Lite06+.
///
/// An empty token means the credential the connection already presented (the
/// URL, a client certificate), or nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Auth {
	pub token: Bytes,
}

impl Message for Auth {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		if !version.has_auth() {
			return Err(DecodeError::Version);
		}
		Ok(Self {
			token: Bytes::decode(r, version)?,
		})
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		if !version.has_auth() {
			return Err(EncodeError::Version);
		}
		self.token.encode(w, version)
	}
}

/// The grant a token earns, as the acceptor writes it on the Auth Stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthOk {
	/// What the opener may publish to the acceptor.
	pub publish: Patterns,
	/// What the opener may subscribe to from the acceptor.
	pub subscribe: Patterns,
	/// How long until the grant lapses, or `None` for never.
	pub expires: Option<Duration>,
}

/// Largest millisecond count every implementation carries losslessly.
const MAX_EXPIRES_MS: u64 = (1 << 53) - 1;

/// Encode a grant's patterns as their canonical text.
fn encode_patterns<W: bytes::BufMut>(patterns: &Patterns, w: &mut W, version: Version) -> Result<(), EncodeError> {
	patterns.len().encode(w, version)?;
	for pattern in patterns {
		pattern.as_str().encode(w, version)?;
	}
	Ok(())
}

fn decode_patterns<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Patterns, DecodeError> {
	let count = usize::decode(r, version)?;
	let mut patterns = Patterns::new();
	// No preallocation: the count is peer-controlled, and the message size limit
	// is what bounds how many patterns actually fit.
	for _ in 0..count {
		let text = String::decode(r, version)?;
		let pattern = Pattern::try_from(text.as_str()).map_err(|_| DecodeError::InvalidValue)?;
		// Only the canonical spelling is valid, so each pattern has one encoding.
		if pattern.as_str() != text {
			return Err(DecodeError::InvalidValue);
		}
		patterns.insert(pattern);
	}
	Ok(patterns)
}

impl Message for AuthOk {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		if !version.has_auth() {
			return Err(DecodeError::Version);
		}
		let publish = decode_patterns(r, version)?;
		let subscribe = decode_patterns(r, version)?;
		let expires = match u64::decode(r, version)? {
			0 => None,
			ms => Some(Duration::from_millis(ms)),
		};
		Ok(Self {
			publish,
			subscribe,
			expires,
		})
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		if !version.has_auth() {
			return Err(EncodeError::Version);
		}
		encode_patterns(&self.publish, w, version)?;
		encode_patterns(&self.subscribe, w, version)?;
		// 0 means never, so a grant that has already lapsed rounds up to the
		// smallest value that still reads as an expiry.
		let expires = match self.expires {
			None => 0,
			Some(expires) => (expires.as_nanos().div_ceil(1_000_000).min(MAX_EXPIRES_MS as u128) as u64).max(1),
		};
		expires.encode(w, version)
	}
}

/// The acceptor refusing a token (as its first reply) or revoking it (after an
/// AUTH_OK). The acceptor closes the stream afterward.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthError {
	/// A code from the session error registry.
	pub code: u64,
	pub reason: String,
}

/// Longest AUTH_ERROR reason, in bytes. Matches the GOAWAY URI cap: generous for a
/// human-readable reason, and rejected from the length prefix alone.
const MAX_REASON: usize = 8192;

impl Message for AuthError {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		if !version.has_auth() {
			return Err(DecodeError::Version);
		}
		let code = u64::decode(r, version)?;
		let len = usize::decode(r, version)?;
		if len > MAX_REASON {
			return Err(DecodeError::InvalidValue);
		}
		if r.remaining() < len {
			return Err(DecodeError::Short);
		}
		let reason = String::from_utf8(r.copy_to_bytes(len).to_vec())?;
		Ok(Self { code, reason })
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		if !version.has_auth() {
			return Err(EncodeError::Version);
		}
		if self.reason.len() > MAX_REASON {
			return Err(EncodeError::TooLarge);
		}
		self.code.encode(w, version)?;
		self.reason.as_str().encode(w, version)
	}
}

/// A message the acceptor writes on the Auth Stream, prefixed with its type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthReply {
	Ok(AuthOk),
	Error(AuthError),
}

const AUTH_OK: u64 = 0;
const AUTH_ERROR: u64 = 1;

/// Write a `type` varint followed by the size-prefixed message body.
fn encode_typed<W: bytes::BufMut, M: Message>(
	w: &mut W,
	typ: u64,
	msg: &M,
	version: Version,
) -> Result<(), EncodeError> {
	typ.encode(w, version)?;
	msg.encode(w, version)
}

impl Encode<Version> for AuthReply {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		match self {
			Self::Ok(ok) => encode_typed(w, AUTH_OK, ok, version),
			Self::Error(err) => encode_typed(w, AUTH_ERROR, err, version),
		}
	}
}

impl Decode<Version> for AuthReply {
	fn decode<B: bytes::Buf>(buf: &mut B, version: Version) -> Result<Self, DecodeError> {
		match u64::decode(buf, version)? {
			AUTH_OK => Ok(Self::Ok(AuthOk::decode(buf, version)?)),
			AUTH_ERROR => Ok(Self::Error(AuthError::decode(buf, version)?)),
			typ => Err(DecodeError::InvalidMessage(typ)),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn patterns(texts: &[&str]) -> Patterns {
		texts.iter().map(|text| Pattern::try_from(*text).unwrap()).collect()
	}

	fn round_trip<T: Encode<Version> + Decode<Version>>(msg: &T) -> T {
		let mut buf = bytes::BytesMut::new();
		msg.encode(&mut buf, Version::Lite06).unwrap();
		let mut slice = &buf[..];
		let got = T::decode(&mut slice, Version::Lite06).unwrap();
		assert!(slice.is_empty(), "trailing bytes after decode");
		got
	}

	#[test]
	fn auth_round_trips() {
		for token in [Bytes::new(), Bytes::from_static(b"eyJhbGciOi.jwt")] {
			let msg = Auth { token };
			assert_eq!(round_trip(&msg), msg);
		}
	}

	/// `**` grants everything, the empty pattern only the root, and the empty list
	/// nothing; literals and wildcards travel exactly, never widened to a prefix.
	#[test]
	fn auth_ok_round_trips() {
		for (publish, subscribe, expires) in [
			(patterns(&["**"]), patterns(&[]), None),
			(patterns(&[""]), patterns(&["room/**"]), None),
			(
				patterns(&["room/alice", "room/*/cam", "**/demo.hang"]),
				patterns(&["room/cam-*.hang", "lobby/**"]),
				Some(Duration::from_secs(60)),
			),
		] {
			let msg = AuthReply::Ok(AuthOk {
				publish,
				subscribe,
				expires,
			});
			assert_eq!(round_trip(&msg), msg);
		}
	}

	/// The exact bytes, shared with `js/net/src/lite/auth.test.ts` so both encoders agree.
	#[test]
	fn auth_ok_golden() {
		let msg = AuthReply::Ok(AuthOk {
			publish: patterns(&["room/*/cam", "**/b.hang"]),
			subscribe: patterns(&[""]),
			expires: Some(Duration::from_millis(1000)),
		});
		let mut buf = bytes::BytesMut::new();
		msg.encode(&mut buf, Version::Lite06).unwrap();
		#[rustfmt::skip]
		let want: &[u8] = &[
			0x00, // AUTH_OK
			0x1a, // length
			0x02, // publish count, in canonical order
			0x09, b'*', b'*', b'/', b'b', b'.', b'h', b'a', b'n', b'g',
			0x0a, b'r', b'o', b'o', b'm', b'/', b'*', b'/', b'c', b'a', b'm',
			0x01, // subscribe count
			0x00, // the empty pattern: the root alone
			0x43, 0xe8, // expires: 1000ms
		];
		assert_eq!(&buf[..], want);
	}

	/// Only valid, canonical text decodes: each pattern has exactly one encoding.
	#[test]
	fn invalid_patterns_are_refused() {
		for text in ["*/**", "/room", "room/", "room//a", "a*b*c", "**/**", "a**"] {
			let mut buf = bytes::BytesMut::new();
			AUTH_OK.encode(&mut buf, Version::Lite06).unwrap();
			let mut body = bytes::BytesMut::new();
			1usize.encode(&mut body, Version::Lite06).unwrap();
			text.encode(&mut body, Version::Lite06).unwrap();
			0usize.encode(&mut body, Version::Lite06).unwrap();
			0u64.encode(&mut body, Version::Lite06).unwrap();
			body.len().encode(&mut buf, Version::Lite06).unwrap();
			buf.extend_from_slice(&body);
			assert!(
				matches!(
					AuthReply::decode(&mut &buf[..], Version::Lite06),
					Err(DecodeError::InvalidValue)
				),
				"{text} decoded"
			);
		}
	}

	#[test]
	fn auth_error_round_trips() {
		let msg = AuthReply::Error(AuthError {
			code: 0x2,
			reason: "expired".to_string(),
		});
		assert_eq!(round_trip(&msg), msg);
	}

	/// A lapsed grant still reads as an expiry, never as "never".
	#[test]
	fn zero_expiry_rounds_up() {
		let msg = AuthReply::Ok(AuthOk {
			publish: Patterns::new(),
			subscribe: Patterns::new(),
			expires: Some(Duration::ZERO),
		});
		let AuthReply::Ok(got) = round_trip(&msg) else {
			panic!("expected AUTH_OK");
		};
		assert_eq!(got.expires, Some(Duration::from_millis(1)));
	}

	#[test]
	fn older_versions_have_no_auth() {
		for version in [
			Version::Lite01,
			Version::Lite02,
			Version::Lite03,
			Version::Lite04,
			Version::Lite05,
		] {
			let mut buf = bytes::BytesMut::new();
			assert!(matches!(
				Auth { token: Bytes::new() }.encode(&mut buf, version),
				Err(EncodeError::Version)
			));
			let mut slice: &[u8] = &[0];
			assert!(matches!(
				Auth::decode_msg(&mut slice, version),
				Err(DecodeError::Version)
			));
		}
	}
}
