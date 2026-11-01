use bytes::{Buf, BufMut};

use crate::coding::{Decode, DecodeError, Encode, EncodeError, Sizer};

use super::Version;

/// Draft 14's TRACK_STATUS_OK and TRACK_STATUS_ERROR, which later drafts give to
/// NAMESPACE_DONE and PUBLISH_BLOCKED.
const TRACK_STATUS_OK_14: u64 = 0x0E;
const TRACK_STATUS_ERROR_14: u64 = 0x0F;

/// The control metric class of an IETF message type on `version`.
pub(crate) fn control(id: u64, version: Version) -> crate::stats::Control {
	use super::*;
	use crate::stats::Control;
	// Draft 14 reuses later drafts' NAMESPACE_DONE type for TRACK_STATUS_OK.
	if version == Version::Draft14 && (id == TRACK_STATUS_OK_14 || id == TRACK_STATUS_ERROR_14) {
		return Control::TrackInfo;
	}
	// Before draft 16, 0x02 updates only a subscription; later it is the shared REQUEST_UPDATE.
	if matches!(version, Version::Draft14 | Version::Draft15) && id == SubscribeUpdate::ID {
		return Control::Subscribe;
	}
	// Draft 14's SUBSCRIBE_ERROR becomes the shared REQUEST_ERROR in draft 15.
	if version == Version::Draft14 && id == SubscribeError::ID {
		return Control::Subscribe;
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
		| NamespaceDone::ID => Control::Announce,
		TrackStatus::ID | SUBSCRIBE_TRACKS_ID | PublishBlocked::ID => Control::TrackInfo,
		Subscribe::ID
		| SubscribeOk::ID
		| Unsubscribe::ID
		| Publish::ID
		| PublishOk::ID
		| PublishError::ID
		| PublishDone::ID => Control::Subscribe,
		Fetch::ID | FetchOk::ID | FetchError::ID | FetchCancel::ID => Control::Fetch,
		GoAway::ID => Control::Goaway,
		_ => Control::Other,
	}
}

/// A trait for IETF messages that are automatically size-prefixed during encoding/decoding.
///
/// IETF messages use a u16 size prefix and have a message type ID for control stream dispatch.
pub trait Message: Sized + std::fmt::Debug {
	const ID: u64;

	/// Encode this message body (without size prefix).
	fn encode_msg<W: BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError>;

	/// Decode a message body (without size prefix).
	fn decode_msg<B: Buf>(buf: &mut B, version: Version) -> Result<Self, DecodeError>;
}

impl<T: Message> Encode<Version> for T {
	fn encode<W: BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		tracing::trace!(?self, "encoding");
		let mut sizer = Sizer::default();
		self.encode_msg(&mut sizer, version)?;
		let size: u16 = sizer.size.try_into().map_err(|_| EncodeError::TooLarge)?;
		size.encode(w, version)?;
		self.encode_msg(w, version)
	}

	fn control(&self, version: Version) -> Option<crate::stats::Control> {
		Some(control(T::ID, version))
	}
}

impl<T: Message> Decode<Version> for T {
	fn control(&self, version: Version) -> Option<crate::stats::Control> {
		Some(control(T::ID, version))
	}

	fn decode<B: Buf>(buf: &mut B, version: Version) -> Result<Self, DecodeError> {
		let size = u16::decode(buf, version)? as usize;

		if tracing::enabled!(tracing::Level::TRACE) {
			if buf.remaining() < size {
				return Err(DecodeError::Short);
			}
			let raw = buf.copy_to_bytes(size);
			let mut slice = &raw[..];
			match Self::decode_msg(&mut slice, version).map_err(DecodeError::complete) {
				Ok(result) => {
					if slice.remaining() > 0 {
						return Err(DecodeError::Long);
					}
					tracing::trace!(?result, "decoded");
					Ok(result)
				}
				Err(e) => {
					// A malformed message can contain credentials too.
					tracing::warn!(%e, "decode failed");
					Err(e)
				}
			}
		} else {
			if buf.remaining() < size {
				return Err(DecodeError::Short);
			}
			let mut limited = buf.take(size);
			match Self::decode_msg(&mut limited, version).map_err(DecodeError::complete) {
				Ok(result) => {
					if limited.remaining() > 0 {
						return Err(DecodeError::Long);
					}
					Ok(result)
				}
				Err(e) => {
					tracing::warn!(%e, "decode failed");
					Err(e)
				}
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::stats::Control;

	/// Draft 14's track-status replies reuse type IDs later drafts give other classes.
	#[test]
	fn draft14_track_status_ids_are_track_info() {
		for id in [TRACK_STATUS_OK_14, TRACK_STATUS_ERROR_14] {
			assert_eq!(control(id, Version::Draft14), Control::TrackInfo);
		}
		assert_eq!(
			control(super::super::NamespaceDone::ID, Version::Draft16),
			Control::Announce
		);
		assert_eq!(
			control(super::super::SubscribeUpdate::ID, Version::Draft15),
			Control::Subscribe
		);
		// Shared replies and updates identify no request class.
		for id in [super::super::RequestOk::ID, super::super::SubscribeUpdate::ID] {
			assert_eq!(control(id, Version::Draft16), Control::Other);
		}
	}

	/// A draft 14 subscription refusal counts as subscribe traffic, but not the
	/// REQUEST_ERROR that reuses its type ID.
	#[test]
	fn draft14_subscribe_error_is_subscribe() {
		let id = super::super::SubscribeError::ID;
		assert_eq!(control(id, Version::Draft14), Control::Subscribe);
		assert_eq!(control(id, Version::Draft15), Control::Other);
	}
}
