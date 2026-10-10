use std::time::Duration;

use bytes::Bytes;

use crate::coding::*;
use crate::{Pattern, Patterns};

use super::{Message, Version};

/// The first message on an Auth Stream: the token the opener presents. Lite07+.
///
/// An empty token means the credential the connection already presented (the
/// URL, a client certificate), or nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Auth {
	pub token: Bytes,
}

impl Message for Auth {
	fn decode_msg(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		if !version.has_auth() {
			return Err(DecodeError::Version);
		}
		Ok(Self {
			token: Bytes::copy_from_slice(r.bytes()?),
		})
	}

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		if !version.has_auth() {
			return Err(EncodeError::Version);
		}
		w.bytes(&self.token)
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
fn encode_patterns(patterns: &Patterns, w: &mut Encoder<'_>) -> Result<(), EncodeError> {
	w.varint(patterns.len() as u64)?;
	for pattern in patterns {
		w.string(pattern.as_str())?;
	}
	Ok(())
}

fn decode_patterns(r: &mut Decoder<'_>) -> Result<Patterns, DecodeError> {
	let count = r.varint()?;
	let mut patterns = Patterns::new();
	// No preallocation: the count is peer-controlled, and the message size limit
	// is what bounds how many patterns actually fit.
	for _ in 0..count {
		let text = r.string()?;
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
	fn decode_msg(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		if !version.has_auth() {
			return Err(DecodeError::Version);
		}
		let publish = decode_patterns(r)?;
		let subscribe = decode_patterns(r)?;
		let expires = match r.varint()? {
			0 => None,
			ms => Some(Duration::from_millis(ms)),
		};
		Ok(Self {
			publish,
			subscribe,
			expires,
		})
	}

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		if !version.has_auth() {
			return Err(EncodeError::Version);
		}
		encode_patterns(&self.publish, w)?;
		encode_patterns(&self.subscribe, w)?;
		// 0 means never, so a grant that has already lapsed rounds up to the
		// smallest value that still reads as an expiry.
		let expires = match self.expires {
			None => 0,
			Some(expires) => (expires.as_nanos().div_ceil(1_000_000).min(MAX_EXPIRES_MS as u128) as u64).max(1),
		};
		w.varint(expires)
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
	fn decode_msg(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		if !version.has_auth() {
			return Err(DecodeError::Version);
		}
		let code = r.varint()?;
		let len = r.varint()?;
		if len > MAX_REASON as u64 {
			return Err(DecodeError::InvalidValue);
		}
		let reason = String::from_utf8(r.slice(len as usize)?.to_vec())?;
		Ok(Self { code, reason })
	}

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		if !version.has_auth() {
			return Err(EncodeError::Version);
		}
		if self.reason.len() > MAX_REASON {
			return Err(EncodeError::TooLarge);
		}
		w.varint(self.code)?;
		w.string(&self.reason)
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
fn encode_typed<M: Message>(w: &mut Encoder<'_>, typ: u64, msg: &M, version: Version) -> Result<(), EncodeError> {
	w.varint(typ)?;
	msg.encode(w, version)
}

impl Encode<Version> for AuthReply {
	fn encode(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		match self {
			Self::Ok(ok) => encode_typed(w, AUTH_OK, ok, version),
			Self::Error(err) => encode_typed(w, AUTH_ERROR, err, version),
		}
	}
}

impl Decode<Version> for AuthReply {
	fn decode(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		match r.varint()? {
			AUTH_OK => Ok(Self::Ok(AuthOk::decode(r, version)?)),
			AUTH_ERROR => Ok(Self::Error(AuthError::decode(r, version)?)),
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

	fn encode<T: Encode<Version>>(msg: &T, version: Version) -> Result<Vec<u8>, EncodeError> {
		let mut buf = Vec::new();
		msg.encode(&mut Encoder::new(&mut buf, version.into()), version)?;
		Ok(buf)
	}

	fn round_trip<T: Encode<Version> + Decode<Version>>(msg: &T) -> T {
		let buf = encode(msg, Version::Lite07).unwrap();
		let mut r = Decoder::new(&buf, Version::Lite07.into());
		let got = T::decode(&mut r, Version::Lite07).unwrap();
		assert!(r.is_empty(), "trailing bytes after decode");
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
		let buf = encode(&msg, Version::Lite07).unwrap();
		#[rustfmt::skip]
		let want: &[u8] = &[
			0x00, // AUTH_OK
			0x1a, // length
			0x02, // publish count, in canonical order
			0x09, b'*', b'*', b'/', b'b', b'.', b'h', b'a', b'n', b'g',
			0x0a, b'r', b'o', b'o', b'm', b'/', b'*', b'/', b'c', b'a', b'm',
			0x01, // subscribe count
			0x00, // the empty pattern: the root alone
			0x83, 0xe8, // expires: 1000ms, as a leading-ones varint
		];
		assert_eq!(&buf[..], want);
	}

	/// Only valid, canonical text decodes: each pattern has exactly one encoding.
	#[test]
	fn invalid_patterns_are_refused() {
		for text in ["*/**", "/room", "room/", "room//a", "a*b*c", "**/**", "a**"] {
			let mut buf = Vec::new();
			let mut w = Encoder::new(&mut buf, Version::Lite07.into());
			w.varint(AUTH_OK).unwrap();
			let prefix = w.prefix_varint();
			w.varint(1).unwrap();
			w.string(text).unwrap();
			w.varint(0).unwrap();
			w.varint(0).unwrap();
			w.fill(prefix).unwrap();
			assert!(
				matches!(
					AuthReply::decode(&mut Decoder::new(&buf, Version::Lite07.into()), Version::Lite07),
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

	/// A message the peer would refuse as too large is never encoded, so the acceptor can
	/// answer instead of sending it.
	#[test]
	fn oversized_message_is_refused() {
		let msg = Auth {
			token: Bytes::from(vec![0; super::super::message::MAX_MESSAGE_SIZE + 1]),
		};
		assert!(matches!(encode(&msg, Version::Lite07), Err(EncodeError::TooLarge)));
	}

	#[test]
	fn older_versions_have_no_auth() {
		for version in [
			Version::Lite01,
			Version::Lite02,
			Version::Lite03,
			Version::Lite04,
			Version::Lite05,
			Version::Lite06,
		] {
			assert!(matches!(
				encode(&Auth { token: Bytes::new() }, version),
				Err(EncodeError::Version)
			));
			assert!(matches!(
				Auth::decode_msg(&mut Decoder::new(&[0], version.into()), version),
				Err(DecodeError::Version)
			));
		}
	}
}
