use crate::coding::{Decode, DecodeError, Decoder, Encode, EncodeError, Encoder};

use super::Version;

// Lite control messages are buffered whole before decoding, so the limit is checked as
// soon as the length prefix arrives. The same ceiling as SETUP: paths, track names, and
// hop chains fit with room to spare, and the JavaScript reader matches it.
pub(super) const MAX_MESSAGE_SIZE: usize = u16::MAX as usize;

/// Read a lite message's varint size prefix, refusing one past `max`.
pub(super) fn decode_size(r: &mut Decoder<'_>, max: usize) -> Result<usize, DecodeError> {
	let size = r.varint()?;
	match usize::try_from(size) {
		Ok(size) if size <= max => Ok(size),
		_ => Err(DecodeError::MessageTooLarge {
			size: usize::try_from(size).unwrap_or(usize::MAX),
			max,
		}),
	}
}

/// A trait for lite messages that are automatically size-prefixed during encoding/decoding.
///
/// Lite messages use a varint size prefix.
pub trait Message<'a>: Sized + std::fmt::Debug {
	/// The control metric class, or none for media headers.
	const CONTROL: Option<crate::stats::Message> = None;
	/// The largest body this receiver accepts for this message.
	const MAX_SIZE: usize = MAX_MESSAGE_SIZE;

	/// Encode this message body (without size prefix).
	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError>;

	/// Decode a message body (without size prefix).
	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError>;
}

impl<'a, T: Message<'a>> Encode<Version> for T {
	fn encode(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		tracing::trace!(?self, "encoding");
		let prefix = w.prefix_varint();
		self.encode_msg(w, version)?;
		// Never emit a body our own receiver would refuse.
		if w.since(&prefix) > Self::MAX_SIZE {
			w.discard(prefix);
			return Err(EncodeError::TooLarge);
		}
		w.fill(prefix)?;
		w.control = T::CONTROL;
		Ok(())
	}
}

impl<'a, T: Message<'a>> Decode<'a, Version> for T {
	fn decode(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		decode_with(r, |body| T::decode_msg(body, version))
	}
}

/// Decode a size-prefixed `T`, reading its body with `decode` in place of
/// [`Message::decode_msg`], for a body whose bounds come from the session.
pub(super) fn decode_with<'a, T: Message<'a>>(
	r: &mut Decoder<'a>,
	decode: impl FnOnce(&mut Decoder<'a>) -> Result<T, DecodeError>,
) -> Result<T, DecodeError> {
	let size = decode_size(r, T::MAX_SIZE)?;
	let mut body = r.sub(size)?;

	// The body is complete, so running short inside it is malformed, not a wait for more.
	let result = decode(&mut body)
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
		r.control = T::CONTROL;
	}
	result
}

#[cfg(test)]
mod tests {
	use super::*;

	#[derive(Debug)]
	struct Empty;

	impl Message<'_> for Empty {
		fn encode_msg(&self, _: &mut Encoder<'_>, _: Version) -> Result<(), EncodeError> {
			Ok(())
		}

		fn decode_msg(_: &mut Decoder<'_>, _: Version) -> Result<Self, DecodeError> {
			Ok(Self)
		}
	}

	/// A lite size prefix announcing `size` bytes, with no body behind it.
	fn prefix(size: usize) -> Vec<u8> {
		let mut wire = Vec::new();
		Encoder::new(&mut wire, Version::Lite06.into())
			.varint(size as u64)
			.unwrap();
		wire
	}

	#[test]
	fn rejects_oversized_message_before_reading_the_body() {
		let wire = prefix(MAX_MESSAGE_SIZE + 1);

		let err = Empty::decode_slice(&wire, Version::Lite06).unwrap_err();
		assert!(matches!(
			err,
			DecodeError::MessageTooLarge {
				size,
				max: MAX_MESSAGE_SIZE,
			} if size == MAX_MESSAGE_SIZE + 1
		));
	}

	#[test]
	fn accepts_message_at_the_limit() {
		let wire = prefix(MAX_MESSAGE_SIZE);

		let err = Empty::decode_slice(&wire, Version::Lite06).unwrap_err();
		assert!(matches!(err, DecodeError::Short));
	}

	/// A peer can no longer make a control stream buffer megabytes: every message past
	/// the SETUP ceiling is refused at its length prefix, announcements included.
	#[test]
	fn control_messages_are_refused_at_the_prefix() {
		let oversized = |prefix: &[u8]| {
			let mut wire = prefix.to_vec();
			Encoder::new(&mut wire, Version::Lite06.into()).varint(65_536).unwrap();
			wire
		};

		let wire = oversized(&[]);
		let err = super::super::Subscribe::decode_slice(&wire, Version::Lite06).unwrap_err();
		assert!(matches!(err, DecodeError::MessageTooLarge { .. }), "{err:?}");

		// ANNOUNCE_START: the type, then the length.
		let wire = oversized(&[0]);
		let err = super::super::AnnounceBroadcast::decode_slice(&wire, Version::Lite06).unwrap_err();
		assert!(matches!(err, DecodeError::MessageTooLarge { .. }), "{err:?}");
	}

	/// ANNOUNCE_INIT carries the whole initial set in one message, so it keeps the room
	/// a large origin needs.
	#[test]
	fn announce_init_waits_for_a_large_body() {
		let mut wire = Vec::new();
		Encoder::new(&mut wire, Version::Lite02.into())
			.varint(1024 * 1024)
			.unwrap();
		let err = super::super::AnnounceInit::decode_slice(&wire, Version::Lite02).unwrap_err();
		assert!(matches!(err, DecodeError::Short), "{err:?}");
	}
}
