use crate::coding::{Decode, DecodeError, Decoder, Encode, EncodeError, Encoder, Prefix};

use super::Version;

pub(super) fn control(id: u64) -> crate::stats::Message {
	use super::*;
	use crate::stats::Message as Class;
	match id {
		PublishNamespace::ID | SubscribeNamespace::ID | Namespace::ID | NamespaceDone::ID => Class::Announce,
		TrackStatus::ID | SubscribeTracks::ID | PublishSkipped::ID => Class::TrackInfo,
		Subscribe::ID | SubscribeOk::ID | Publish::ID | PublishOk::ID | PublishDone::ID => Class::Subscribe,
		Fetch::ID | FetchOk::ID => Class::Fetch,
		GoAway::ID => Class::Goaway,
		_ => Class::Other,
	}
}

/// The largest control message body this endpoint sends or accepts.
///
/// Draft-23's MAX_CONTROL_MESSAGE_SIZE (section 9.1.8) defaults to 65535 and we never
/// advertise more, so the bound is the one a 16-bit length already implied.
pub(crate) const MAX_CONTROL_MESSAGE_SIZE: usize = u16::MAX as usize;

/// Reserve a control message's Message Length: a varint from draft-23 (section 9), a
/// `u16` before. [`fill_length`] sizes it.
pub(crate) fn prefix_length(w: &mut Encoder<'_>, version: Version) -> Prefix {
	match version.varint_length() {
		true => w.prefix_varint(),
		false => w.prefix_u16(),
	}
}

/// Size a Message Length reserved by [`prefix_length`], refusing a body past
/// [`MAX_CONTROL_MESSAGE_SIZE`], which the peer would close the session on.
pub(crate) fn fill_length(w: &mut Encoder<'_>, prefix: Prefix) -> Result<(), EncodeError> {
	if w.since(&prefix) > MAX_CONTROL_MESSAGE_SIZE {
		return Err(EncodeError::TooLarge);
	}
	w.fill(prefix)
}

/// Read a control message's Message Length in `version`'s encoding.
pub(crate) fn decode_length(r: &mut Decoder<'_>, version: Version) -> Result<usize, DecodeError> {
	if !version.varint_length() {
		return Ok(r.u16()? as usize);
	}
	let size = r.varint()?;
	match usize::try_from(size) {
		Ok(size) if size <= MAX_CONTROL_MESSAGE_SIZE => Ok(size),
		_ => Err(DecodeError::MessageTooLarge {
			size: usize::try_from(size).unwrap_or(usize::MAX),
			max: MAX_CONTROL_MESSAGE_SIZE,
		}),
	}
}

/// The Message Length at the head of `bytes` and the width of its field, in `version`'s
/// encoding. For tests that walk frames collected off a test transport.
#[cfg(test)]
pub(crate) fn peek_length(bytes: &[u8], version: Version) -> (usize, usize) {
	let mut r = Decoder::new(bytes, version.into());
	let len = decode_length(&mut r, version).expect("a whole Message Length");
	(len, r.position())
}

/// The Message Length of a `len` byte body in `version`'s encoding. For hand-built frames.
#[cfg(test)]
pub(crate) fn length_bytes(len: usize, version: Version) -> Vec<u8> {
	if !version.varint_length() {
		return u16::try_from(len)
			.expect("a length that fits 16 bits")
			.to_be_bytes()
			.to_vec();
	}
	let mut buf = Vec::new();
	Encoder::new(&mut buf, version.into())
		.varint(len as u64)
		.expect("a length that fits a varint");
	buf
}

/// A whole control message: its type, its Message Length in `version`'s encoding, its body.
#[cfg(test)]
pub(crate) fn framed(id: u64, body: &[u8], version: Version) -> Vec<u8> {
	let mut buf = Vec::new();
	Encoder::new(&mut buf, version.into())
		.varint(id)
		.expect("a type that fits a varint");
	buf.extend(length_bytes(body.len(), version));
	buf.extend_from_slice(body);
	buf
}

/// A trait for IETF messages that are automatically size-prefixed during encoding/decoding.
///
/// IETF messages carry a Message Length (see [`prefix_length`]) and have a message type ID
/// for control stream dispatch.
pub trait Message<'a>: Sized + std::fmt::Debug {
	const ID: u64;

	/// The type this message goes out as at `version`, which classifies it for stats.
	fn wire_id(_version: Version) -> u64 {
		Self::ID
	}

	/// Encode this message body (without size prefix).
	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError>;

	/// Decode a message body (without size prefix).
	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError>;
}

impl<'a, T: Message<'a>> Encode<Version> for T {
	fn encode(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		tracing::trace!(?self, "encoding");
		let prefix = prefix_length(w, version);
		self.encode_msg(w, version)?;
		fill_length(w, prefix)?;
		w.control = Some(control(T::wire_id(version)));
		Ok(())
	}
}

/// A control message body not decoded yet: its Message Length, then that many bytes.
///
/// Read after the message type, when the type decides how to decode the rest.
#[derive(Clone, Debug)]
pub struct Body(pub bytes::Bytes);

impl<'a> Decode<'a, Version> for Body {
	fn decode(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		let size = decode_length(r, version)?;
		Ok(Self(bytes::Bytes::copy_from_slice(r.slice(size)?)))
	}
}

impl Body {
	/// A decoder over the body, with `version`'s varints.
	pub fn decoder(&self, version: Version) -> Decoder<'_> {
		Decoder::new(&self.0, version.into())
	}

	/// Decode the whole body as `M`, which must fill it exactly.
	pub fn message<'a, M: Message<'a>>(&'a self, version: Version) -> Result<M, DecodeError> {
		let mut data = self.decoder(version);
		// The body is complete, so running short inside it is malformed, not a wait for more.
		let msg = M::decode_msg(&mut data, version).map_err(DecodeError::complete)?;
		match data.is_empty() {
			true => Ok(msg),
			false => Err(DecodeError::TrailingBytes),
		}
	}
}

impl<'a, T: Message<'a>> Decode<'a, Version> for T {
	fn decode(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		let size = decode_length(r, version)?;
		let mut body = r.sub(size)?;

		// The body is complete, so running short inside it is malformed, not a wait for more.
		let result = Self::decode_msg(&mut body, version)
			.map_err(DecodeError::complete)
			.and_then(|msg| match body.is_empty() {
				true => Ok(msg),
				false => Err(DecodeError::Long),
			});

		match &result {
			Ok(msg) => tracing::trace!(?msg, "decoded"),
			Err(err) => tracing::warn!(%err, "decode failed"),
		}
		if result.is_ok() {
			r.control = Some(control(T::wire_id(version)));
		}
		result
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::stats::Message as Class;

	fn frame(version: Version, body: &[u8]) -> Vec<u8> {
		let mut buf = Vec::new();
		let mut w = Encoder::new(&mut buf, version.into());
		let prefix = prefix_length(&mut w, version);
		w.slice(body);
		fill_length(&mut w, prefix).unwrap();
		buf
	}

	/// Draft-23 (section 9) carries the Message Length as a varint; earlier drafts keep 16 bits.
	#[test]
	fn length_is_a_varint_from_draft23() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			assert_eq!(frame(version, &[0xAA; 3]), [0x00, 0x03, 0xAA, 0xAA, 0xAA], "{version}");
		}
		assert_eq!(frame(Version::Draft23, &[0xAA; 3]), [0x03, 0xAA, 0xAA, 0xAA]);

		// A length past one varint byte moves the body up and still reads back whole.
		let body = vec![0x55; 300];
		for version in [Version::Draft22, Version::Draft23] {
			let wire = frame(version, &body);
			let mut r = Decoder::new(&wire, version.into());
			assert_eq!(Body::decode(&mut r, version).unwrap().0, body, "{version}");
			assert!(r.is_empty(), "{version}");
		}
	}

	/// We advertise no MAX_CONTROL_MESSAGE_SIZE, so the default 65535 bounds both directions
	/// (draft-23 section 9.1.8): a longer body is not sent, and a longer length is refused
	/// before its body is buffered.
	#[test]
	fn draft23_length_is_bounded() {
		let version = Version::Draft23;
		let mut buf = Vec::new();
		let mut w = Encoder::new(&mut buf, version.into());
		let prefix = prefix_length(&mut w, version);
		w.slice(&vec![0; MAX_CONTROL_MESSAGE_SIZE + 1]);
		assert!(matches!(fill_length(&mut w, prefix), Err(EncodeError::TooLarge)));

		let mut wire = Vec::new();
		Encoder::new(&mut wire, version.into())
			.varint(MAX_CONTROL_MESSAGE_SIZE as u64 + 1)
			.unwrap();
		let mut r = Decoder::new(&wire, version.into());
		assert!(matches!(
			decode_length(&mut r, version),
			Err(DecodeError::MessageTooLarge { .. })
		));
	}

	#[test]
	fn shared_replies_identify_no_request_class() {
		assert_eq!(control(super::super::NamespaceDone::ID), Class::Announce);
		for id in [super::super::RequestOk::ID, super::super::SubscribeUpdate::ID] {
			assert_eq!(control(id), Class::Other);
		}
	}
}
