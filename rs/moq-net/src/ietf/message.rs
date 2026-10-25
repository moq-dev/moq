use crate::coding::{Decode, DecodeError, Decoder, Encode, EncodeError, Encoder};

use super::Version;

pub(super) fn control(id: u64, version: Version) -> crate::stats::Message {
	use super::*;
	use crate::stats::Message as Class;
	// Draft 14 reuses later drafts' NAMESPACE_DONE type for TRACK_STATUS_OK.
	if version == Version::Draft14 && (id == TrackStatusOk::ID_14 || id == TRACK_STATUS_ERROR_14) {
		return Class::TrackInfo;
	}
	// Before draft 16, 0x02 updates only a subscription; later it is the shared REQUEST_UPDATE.
	if matches!(version, Version::Draft14 | Version::Draft15) && id == SubscribeUpdate::ID {
		return Class::Subscribe;
	}
	match id {
		PublishNamespace::ID
		| PublishNamespaceError::ID
		| PublishNamespaceDone::ID
		| PublishNamespaceCancel::ID
		| SubscribeNamespace::ID
		| SubscribeNamespaceLegacy::ID
		| SubscribeNamespaceOk::ID
		| SubscribeNamespaceError::ID
		| UnsubscribeNamespace::ID
		| NamespaceDone::ID => Class::Announce,
		TrackStatus::ID | SubscribeTracks::ID | PublishSkipped::ID => Class::TrackInfo,
		Subscribe::ID
		| SubscribeOk::ID
		| Unsubscribe::ID
		| Publish::ID
		| PublishOk::ID
		| PublishError::ID
		| PublishDone::ID => Class::Subscribe,
		Fetch::ID | FetchOk::ID | FetchError::ID | FetchCancel::ID => Class::Fetch,
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
		w.control = Some(control(T::ID, version));
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
			r.control = Some(control(T::ID, version));
		}
		result
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::stats::Message as Class;

	/// Draft 14's track-status replies reuse type IDs later drafts give other classes.
	#[test]
	fn draft14_track_status_ids_are_track_info() {
		for id in [super::super::TrackStatusOk::ID_14, super::super::TRACK_STATUS_ERROR_14] {
			assert_eq!(control(id, Version::Draft14), Class::TrackInfo);
		}
		assert_eq!(
			control(super::super::NamespaceDone::ID, Version::Draft16),
			Class::Announce
		);
		assert_eq!(
			control(super::super::SubscribeUpdate::ID, Version::Draft15),
			Class::Subscribe
		);
		// Shared replies and updates identify no request class.
		for id in [super::super::RequestOk::ID, super::super::SubscribeUpdate::ID] {
			assert_eq!(control(id, Version::Draft16), Class::Other);
		}
	}
}
