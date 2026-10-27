use crate::coding::{Decode, DecodeError, Decoder, Encode, EncodeError, Encoder};

use super::Version;

pub(super) fn control(id: u64) -> crate::stats::Message {
	use super::*;
	use crate::stats::Message as Class;
	match id {
		PublishNamespace::ID | SubscribeNamespace::ID | Namespace::ID | NamespaceDone::ID => Class::Announce,
		TrackStatus::ID | SubscribeTracks::ID | PublishSkipped::ID => Class::TrackInfo,
		Subscribe::ID | SubscribeOk::ID | Publish::ID | PublishOk::ID | PublishDone::ID => Class::Subscribe,
		Fetch::ID | FetchOk::ID => Class::Fetch,
		auth::Auth::ID | auth::AuthOk::ID | auth::AuthError::ID => Class::Auth,
		GoAway::ID => Class::Goaway,
		_ => Class::Other,
	}
}

/// A trait for IETF messages that are automatically size-prefixed during encoding/decoding.
///
/// IETF messages use a u16 size prefix and have a message type ID for control stream dispatch.
pub trait Message: Sized + std::fmt::Debug {
	const ID: u64;

	/// Encode this message body (without size prefix).
	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError>;

	/// Decode a message body (without size prefix).
	fn decode_msg(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError>;
}

impl<T: Message> Encode<Version> for T {
	fn encode(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		tracing::trace!(?self, "encoding");
		let prefix = w.prefix_u16();
		self.encode_msg(w, version)?;
		w.fill(prefix)?;
		w.control = Some(control(T::ID));
		Ok(())
	}
}

/// A control message body not decoded yet: a `u16` length, then that many bytes.
///
/// Read after the message type, when the type decides how to decode the rest.
#[derive(Clone, Debug)]
pub struct Body(pub bytes::Bytes);

impl Decode<Version> for Body {
	fn decode(r: &mut Decoder<'_>, _: Version) -> Result<Self, DecodeError> {
		let size = r.u16()? as usize;
		Ok(Self(bytes::Bytes::copy_from_slice(r.slice(size)?)))
	}
}

impl Body {
	/// A decoder over the body, with `version`'s varints.
	pub fn decoder(&self, version: Version) -> Decoder<'_> {
		Decoder::new(&self.0, version.into())
	}

	/// Decode the whole body as `M`, which must fill it exactly.
	pub fn message<M: Message>(&self, version: Version) -> Result<M, DecodeError> {
		let mut data = self.decoder(version);
		// The body is complete, so running short inside it is malformed, not a wait for more.
		let msg = M::decode_msg(&mut data, version).map_err(DecodeError::complete)?;
		match data.is_empty() {
			true => Ok(msg),
			false => Err(DecodeError::TrailingBytes),
		}
	}
}

impl<T: Message> Decode<Version> for T {
	fn decode(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		let size = r.u16()? as usize;
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
			r.control = Some(control(T::ID));
		}
		result
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::stats::Message as Class;

	#[test]
	fn shared_replies_identify_no_request_class() {
		assert_eq!(control(super::super::NamespaceDone::ID), Class::Announce);
		for id in [super::super::RequestOk::ID, super::super::SubscribeUpdate::ID] {
			assert_eq!(control(id), Class::Other);
		}
	}
}
