//! The moq-transport error code registries, per negotiated draft.
//!
//! This module holds the stream reset registry and the session close encoder; [`request`]
//! holds the one for rejecting a request. The reset and request registries are disjoint, so
//! the same integer means different things in each.
//!
//! # Stream reset codes
//!
//! Sent on RESET_STREAM and STOP_SENDING, and read back off both. The values are the
//! draft's, not [`StreamError::to_code`]'s: the two registries agree on most of what they
//! both assign, but not all of it.

use super::Version;
use crate::{SessionError, StreamError};

/// An implementation-specific error: the stream died on our side, with no registry entry
/// for why.
pub const INTERNAL_ERROR: u32 = 0x0;

/// The stream was cancelled by either endpoint. A routine unsubscribe, not a failure.
pub const CANCELLED: u32 = 0x1;

/// The content missed its delivery deadline.
const DELIVERY_TIMEOUT: u32 = 0x2;

/// The session is closing, taking this stream with it.
const SESSION_CLOSED: u32 = 0x3;

/// A GOAWAY was sent or received.
const GOING_AWAY: u32 = 0x4;

/// The subscription outran the publisher's resource limits.
const TOO_FAR_BEHIND: u32 = 0x5;

/// The track's content could not be parsed.
const MALFORMED_TRACK: u32 = 0x12;

/// The code to reset a stream, or send STOP_SENDING, with.
///
/// Conditions without a matching reset code use INTERNAL_ERROR. Request rejection has
/// its own registry: a missing track belongs in a request error response, not a reset.
/// moq-lite's own 48-63 range and its application offset have no corresponding ranges here.
pub fn to_stream_code(err: &StreamError) -> u32 {
	match err {
		StreamError::Internal => INTERNAL_ERROR,
		StreamError::Cancel => CANCELLED,
		// We never negotiate the section 8 DELIVERY_TIMEOUT parameter, so this is only ever
		// our own delivery deadline. That is what the code describes, and the same claim
		// moq-lite makes with it, so a relay can carry a peer's timeout across either wire.
		StreamError::DeliveryTimeout => DELIVERY_TIMEOUT,
		// Flattened, as on the moq-lite wire: the session registry is disjoint, so the
		// specific reason travels on the session close instead.
		StreamError::Session(_) => SESSION_CLOSED,
		StreamError::GoingAway => GOING_AWAY,
		StreamError::TooFarBehind => TOO_FAR_BEHIND,
		StreamError::MalformedTrack => MALFORMED_TRACK,
		_ => INTERNAL_ERROR,
	}
}

/// Read a stream reset (or STOP_SENDING) code.
///
/// A code the drafts do not assign stays [`StreamError::Unknown`], which surfaces as
/// [`Error::Stream`](crate::Error::Stream): an error, but never one given a meaning it did
/// not carry. That includes the codes this crate has no local counterpart for
/// (UNKNOWN_OBJECT_STATUS, EXCESSIVE_LOAD) and every value a later
/// draft may add.
pub fn from_stream_code(code: u32) -> StreamError {
	match code {
		INTERNAL_ERROR => StreamError::Internal,
		CANCELLED => StreamError::Cancel,
		DELIVERY_TIMEOUT => StreamError::DeliveryTimeout,
		// The peer's session code is not on this stream, so the reason is unknown here.
		SESSION_CLOSED => StreamError::Session(SessionError::Internal),
		GOING_AWAY => StreamError::GoingAway,
		TOO_FAR_BEHIND => StreamError::TooFarBehind,
		MALFORMED_TRACK => StreamError::MalformedTrack,
		code => StreamError::Unknown(code),
	}
}

/// The code to close the session with.
///
/// The shared session registry, except moq-lite's own 0x30-0x3f range: a bridge must not
/// carry those codes onto this wire, so an unknown one closes as unspecified instead.
pub(crate) fn to_session_code(error: &SessionError) -> u32 {
	match error {
		SessionError::Unknown(0x30..=0x3f) => SessionError::Internal.to_code(),
		error => error.to_code(),
	}
}

/// The moq-transport request error registry, per negotiated draft.
///
/// Sent in a rejection response to a request, and read back off one. The registry is
/// disjoint from the stream reset one, so the same integer means something else there.
pub(crate) mod request {
	use super::Version;
	use crate::{DecodeError, Error, SessionError, StreamError};

	/// Which request the error answers.
	///
	/// Every request shares one REQUEST_ERROR registry, so the kind only decides what a
	/// refused route says (not here to a request for content, uninterested to an offer of
	/// it) and which codes only FETCH carries.
	#[derive(Clone, Copy, Debug, PartialEq, Eq)]
	pub(crate) enum Kind {
		/// SUBSCRIBE.
		Subscribe,
		/// FETCH.
		Fetch,
		/// PUBLISH.
		Publish,
		/// PUBLISH_NAMESPACE.
		PublishNamespace,
		/// SUBSCRIBE_NAMESPACE.
		SubscribeNamespace,
		/// TRACK_STATUS.
		TrackStatus,
	}

	/// An implementation-specific error, with no registry entry for why.
	const INTERNAL_ERROR: u64 = 0x0;

	/// The peer's credentials do not cover the request.
	const UNAUTHORIZED: u64 = 0x1;

	/// The request could not be answered before an implementation-specific deadline.
	const TIMEOUT: u64 = 0x2;

	/// The endpoint does not implement this request at all, as opposed to refusing this one.
	pub(crate) const NOT_SUPPORTED: u64 = 0x3;

	/// A second subscription to a track the peer already subscribes to.
	pub(crate) const DUPLICATE_SUBSCRIPTION: u64 = 0x19;

	/// The endpoint is overloaded and cannot process the request.
	pub(crate) const EXCESSIVE_LOAD: u64 = 0x9;

	/// A GOAWAY is draining the session, so no new request is accepted.
	const GOING_AWAY: u64 = 0x6;

	/// An invalid FETCH range.
	const INVALID_RANGE: u64 = 0x11;

	/// The track holds a Mandatory Track Property the endpoint does not understand.
	const UNSUPPORTED_EXTENSION: u64 = 0x33;

	/// A Range Filter exceeds MAX_FILTER_RANGES (draft-22 and later).
	pub(crate) const INVALID_FILTER: u64 = 0x36;

	/// A joining FETCH named no active subscription. Removed from the registry in draft-22.
	const INVALID_JOINING_REQUEST_ID: u64 = 0x32;

	/// The requested broadcast or track is not here.
	const DOES_NOT_EXIST: u64 = 0x10;

	/// The offered content is not wanted here.
	const UNINTERESTED: u64 = 0x20;

	/// The track's content could not be parsed.
	const MALFORMED_TRACK: u64 = 0x12;

	/// A well-formed AUTHORIZATION TOKEN this endpoint cannot use.
	const MALFORMED_AUTH_TOKEN: u64 = 0x4;

	/// An expired AUTHORIZATION TOKEN.
	const EXPIRED_AUTH_TOKEN: u64 = 0x5;

	fn invalid_range(kind: Kind) -> Option<u64> {
		(kind == Kind::Fetch).then_some(INVALID_RANGE)
	}

	fn invalid_joining_request_id(kind: Kind, version: Version) -> Option<u64> {
		if kind != Kind::Fetch {
			return None;
		}
		match version {
			Version::Draft18 => Some(INVALID_JOINING_REQUEST_ID),
			_ => None,
		}
	}

	/// The code to reject a request with, on the negotiated draft.
	///
	/// Lossy on purpose, like [`to_stream_code`](super::to_stream_code): an error says what went
	/// wrong here, while the registry is what the peer can act on, so a dozen local failures are
	/// one INTERNAL_ERROR on the wire. A condition the draft does not register for this request
	/// falls back to INTERNAL_ERROR too, rather than borrowing a number from another draft, which
	/// would say something the peer's registry gives a different meaning.
	pub(crate) fn to_code(err: &Error, kind: Kind, version: Version) -> u64 {
		let registered = match err {
			Error::Unauthorized
			| Error::Session(SessionError::Unauthorized)
			| Error::Stream(StreamError::Unauthorized) => return UNAUTHORIZED,
			Error::Expired | Error::Session(SessionError::Expired) => return EXPIRED_AUTH_TOKEN,
			Error::Timeout | Error::Stream(StreamError::DeliveryTimeout) | Error::Session(SessionError::Timeout) => {
				return TIMEOUT;
			}
			Error::Unsupported
			| Error::Version
			| Error::Decode(DecodeError::MultipleAuthTokens)
			| Error::Session(SessionError::Version | SessionError::Unsupported) => {
				return NOT_SUPPORTED;
			}
			// moq-transport has no code for a FETCH that reached a datagram, which is never
			// cached, so it is a group that is not there. NOT_SUPPORTED would claim the peer
			// does not implement FETCH at all.
			Error::NotFound
			| Error::NotFetchable
			| Error::Stream(StreamError::NotFound | StreamError::NotFetchable) => return DOES_NOT_EXIST,
			Error::InvalidRange => invalid_range(kind),
			Error::InvalidJoiningRequestId => invalid_joining_request_id(kind, version),
			// A path with no route is one we will not carry. A subscriber that asked for it
			// cannot act on "we do not want it": what it needs to know is that we do not have
			// it, which is the same refusal from its side. Only the requests that offer content
			// say UNINTERESTED.
			Error::Unroutable | Error::Stream(StreamError::Unroutable) => {
				return match kind {
					Kind::Subscribe | Kind::Fetch | Kind::TrackStatus => DOES_NOT_EXIST,
					Kind::Publish | Kind::PublishNamespace | Kind::SubscribeNamespace => UNINTERESTED,
				};
			}
			Error::Decode(DecodeError::MalformedAuthToken) => return MALFORMED_AUTH_TOKEN,
			// Our own parse failure is, from the peer's side, a malformed track.
			Error::Decode(_)
			| Error::BoundsExceeded(_)
			| Error::MalformedTrack
			| Error::Stream(StreamError::MalformedTrack) => return MALFORMED_TRACK,
			Error::GoingAway | Error::Stream(StreamError::GoingAway) => return GOING_AWAY,
			Error::UnsupportedExtension => return UNSUPPORTED_EXTENSION,
			// Everything else, a duplicate included: no draft assigns it a value, and
			// INTERNAL_ERROR is how a peer reads an unregistered code anyway.
			_ => return INTERNAL_ERROR,
		};

		registered.unwrap_or(INTERNAL_ERROR)
	}

	/// Read a rejection code received on the negotiated draft.
	///
	/// A code the draft does not assign for this request, INTERNAL_ERROR included, stays
	/// [`Error::Remote`]: an error, but never one given a meaning it did not carry.
	pub(crate) fn from_code(code: u64, kind: Kind, version: Version) -> Error {
		match code {
			UNAUTHORIZED => Error::Unauthorized,
			TIMEOUT => Error::Timeout,
			NOT_SUPPORTED => Error::Unsupported,
			EXPIRED_AUTH_TOKEN => Error::Expired,
			DOES_NOT_EXIST => Error::NotFound,
			code if Some(code) == invalid_range(kind) => Error::InvalidRange,
			code if Some(code) == invalid_joining_request_id(kind, version) => Error::InvalidJoiningRequestId,
			UNINTERESTED => Error::Unroutable,
			MALFORMED_TRACK => Error::MalformedTrack,
			GOING_AWAY => Error::GoingAway,
			UNSUPPORTED_EXTENSION => Error::UnsupportedExtension,
			code => Error::Remote(u32::try_from(code).unwrap_or(u32::MAX)),
		}
	}

	#[cfg(test)]
	mod tests {
		use super::*;

		const ALL: [Version; 3] = [Version::Draft18, Version::Draft20, Version::Draft22];

		const KINDS: [Kind; 6] = [
			Kind::Subscribe,
			Kind::Fetch,
			Kind::Publish,
			Kind::PublishNamespace,
			Kind::SubscribeNamespace,
			Kind::TrackStatus,
		];

		/// Every error a rejection distinguishes, plus one it does not, so the checks below cover
		/// the whole registry rather than the variants someone remembered. A new arm in
		/// [`to_code`] belongs here.
		const EVERY_ERROR: [Error; 15] = [
			Error::Duplicate,
			Error::Unauthorized,
			Error::Expired,
			Error::Timeout,
			Error::Unsupported,
			Error::Session(SessionError::Unsupported),
			Error::NotFound,
			Error::NotFetchable,
			Error::InvalidRange,
			Error::InvalidJoiningRequestId,
			Error::Unroutable,
			Error::MalformedTrack,
			Error::GoingAway,
			Error::UnsupportedExtension,
			Error::Remote(0x30),
		];

		/// A relay bridging a moq-lite revocation onto a moq-transport request refuses it as
		/// UNAUTHORIZED, the same as a local refusal.
		#[test]
		fn a_bridged_revocation_is_unauthorized() {
			for version in ALL {
				for kind in KINDS {
					assert_eq!(
						to_code(&Error::Stream(StreamError::Unauthorized), kind, version),
						UNAUTHORIZED
					);
				}
			}
		}

		/// A missing broadcast is DOES_NOT_EXIST, never 0x4, which is MALFORMED_AUTH_TOKEN:
		/// getting this backwards tells a peer its token is broken when the broadcast simply
		/// is not here, so it re-authenticates instead of waiting for the announcement.
		#[test]
		fn a_missing_broadcast_is_not_here() {
			for kind in [Kind::Subscribe, Kind::Fetch] {
				for version in ALL {
					assert_eq!(to_code(&Error::NotFound, kind, version), 0x10);
					assert!(matches!(from_code(0x10, kind, version), Error::NotFound));
					assert!(matches!(from_code(0x4, kind, version), Error::Remote(0x4)));
				}
			}

			// A broadcast with no route is, to the peer that asked for it, not here either.
			assert_eq!(to_code(&Error::Unroutable, Kind::Subscribe, Version::Draft22), 0x10);

			// So is a datagram group: never cached, so a FETCH reaching one finds nothing, and
			// NOT_SUPPORTED would say the endpoint has no FETCH at all.
			assert_eq!(to_code(&Error::NotFetchable, Kind::Fetch, Version::Draft22), 0x10);
			// Including a lite-07 upstream's NOT_FETCHABLE, bridged by a relay.
			assert_eq!(
				to_code(&Error::Stream(StreamError::NotFetchable), Kind::Fetch, Version::Draft22),
				0x10
			);
		}

		/// A refused route says UNINTERESTED to the requests that offer content.
		#[test]
		fn an_unroutable_offer_is_uninterested() {
			for kind in [Kind::Publish, Kind::PublishNamespace, Kind::SubscribeNamespace] {
				assert_eq!(to_code(&Error::Unroutable, kind, Version::Draft22), 0x20);
				assert!(matches!(from_code(0x20, kind, Version::Draft22), Error::Unroutable));
			}
		}

		#[test]
		fn token_refusals_use_their_codes() {
			let err = Error::Decode(DecodeError::MalformedAuthToken);
			for version in ALL {
				for kind in KINDS {
					assert_eq!(to_code(&err, kind, version), 0x4);
					assert_eq!(to_code(&Error::Expired, kind, version), 0x5);
					assert!(matches!(from_code(0x5, kind, version), Error::Expired));
				}
			}
		}

		#[test]
		fn going_away_and_unsupported_extension_round_trip() {
			for version in ALL {
				for kind in KINDS {
					assert_eq!(to_code(&Error::GoingAway, kind, version), GOING_AWAY);
					assert!(matches!(from_code(GOING_AWAY, kind, version), Error::GoingAway));
					assert_eq!(to_code(&Error::UnsupportedExtension, kind, version), 0x33);
					assert!(matches!(from_code(0x33, kind, version), Error::UnsupportedExtension));
				}
			}
		}

		#[test]
		fn fetch_refusals_follow_each_drafts_registry() {
			for version in ALL {
				assert_eq!(to_code(&Error::InvalidRange, Kind::Fetch, version), 0x11);
				assert!(matches!(from_code(0x11, Kind::Fetch, version), Error::InvalidRange));
				let joining = match version {
					Version::Draft18 => Some(0x32),
					_ => None,
				};
				assert_eq!(
					to_code(&Error::InvalidJoiningRequestId, Kind::Fetch, version),
					joining.unwrap_or(0)
				);
				if let Some(code) = joining {
					assert!(matches!(
						from_code(code, Kind::Fetch, version),
						Error::InvalidJoiningRequestId
					));
				} else {
					assert!(matches!(from_code(0x32, Kind::Fetch, version), Error::Remote(0x32)));
				}
			}
		}

		/// Every code we send must decode back to what we meant on the same draft and
		/// request, or two moq-net peers disagree about what a rejection said.
		#[test]
		fn every_emitted_code_round_trips() {
			for version in ALL {
				for kind in KINDS {
					for err in &EVERY_ERROR {
						let code = to_code(err, kind, version);
						let decoded = from_code(code, kind, version);
						assert_eq!(
							to_code(&decoded, kind, version),
							code,
							"{err:?} on {version:?}/{kind:?} did not survive a round trip"
						);
					}
				}
			}
		}

		/// Every code we put in a rejection has to be one the negotiated draft registers for
		/// that request. The table is transcribed from the drafts (draft-18 section 15.11.2
		/// on), not derived from the mapping, so a mistake in the mapping cannot talk the
		/// assertion into agreeing with it.
		#[test]
		fn only_registered_codes_reach_the_wire() {
			fn registered() -> &'static [u64] {
				&[
					0x0, 0x1, 0x2, 0x3, 0x4, 0x5, 0x6, 0x9, 0x10, 0x11, 0x12, 0x19, 0x20, 0x30, 0x31, 0x32, 0x33, 0x34,
					0x35, 0x36,
				]
			}

			// Sent but never read back as itself: a peer's MALFORMED_AUTH_TOKEN stays remote.
			let malformed = Error::Decode(DecodeError::MalformedAuthToken);
			for version in ALL {
				for kind in KINDS {
					for err in EVERY_ERROR.iter().chain([&malformed]) {
						let code = to_code(err, kind, version);
						assert!(
							registered().contains(&code),
							"{err:?} sends {code:#x}, which {version} does not register for {kind:?}"
						);
					}
				}
			}
		}

		/// A failure with no registered meaning says INTERNAL_ERROR rather than borrowing a
		/// number that means something else, and comes back as the opaque remote code it was.
		#[test]
		fn an_unregistered_error_is_internal() {
			for err in [Error::Duplicate, Error::Cancel, Error::ProtocolViolation, Error::Closed] {
				for kind in KINDS {
					assert_eq!(
						to_code(&err, kind, Version::Draft22),
						INTERNAL_ERROR,
						"{err} is not internal on {kind:?}"
					);
				}
			}

			assert!(matches!(
				from_code(INTERNAL_ERROR, Kind::Subscribe, Version::Draft22),
				Error::Remote(0)
			));
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::Error;

	/// A routine unsubscribe must not read as a fault on our side. moq-lite's own error
	/// enum encodes a cancellation as 0, which is this wire's INTERNAL_ERROR, so the codes
	/// come from here instead.
	#[test]
	fn a_cancellation_is_not_an_internal_error() {
		assert_eq!(to_stream_code(&StreamError::Cancel), CANCELLED);
		assert_eq!(from_stream_code(CANCELLED), StreamError::Cancel);
		assert!(matches!(
			Error::from(from_stream_code(CANCELLED)),
			Error::Stream(StreamError::Cancel)
		));

		assert_ne!(
			CANCELLED,
			SessionError::Cancel.to_code(),
			"the two spaces disagree about 0"
		);
		assert_eq!(SessionError::Cancel.to_code(), INTERNAL_ERROR);
	}

	/// moq-lite's own session range never reaches this wire, while the shared registry and
	/// its reserved range pass through as on moq-lite.
	#[test]
	fn the_lite_session_range_is_not_sent() {
		for code in 0x30..=0x3f {
			assert_eq!(to_session_code(&SessionError::Unknown(code)), 0x1);
			assert_eq!(
				SessionError::Unknown(code).to_code(),
				code,
				"the shared encoder is unchanged"
			);
		}
		assert_eq!(to_session_code(&SessionError::Unknown(0x20)), 0x20);
		assert_eq!(
			to_session_code(&SessionError::Unauthorized),
			SessionError::Unauthorized.to_code()
		);
		assert_eq!(to_session_code(&SessionError::App(7)), 64 + 7);
	}

	/// Every code we send must decode back to what we meant, or two moq-net peers disagree
	/// about what a stream reset said.
	#[test]
	fn every_emitted_code_round_trips() {
		let errors = [
			StreamError::Internal,
			StreamError::Cancel,
			StreamError::DeliveryTimeout,
			StreamError::ControlTimeout,
			StreamError::GoingAway,
			StreamError::TooFarBehind,
			StreamError::MalformedTrack,
			StreamError::NotFound,
			StreamError::Old,
			StreamError::Evicted,
			StreamError::App(7),
		];

		for err in &errors {
			let code = to_stream_code(err);
			let decoded = from_stream_code(code);
			assert_eq!(to_stream_code(&decoded), code, "{err:?} did not survive a round trip");
		}

		// A session teardown flattens to SESSION_CLOSED and comes back as one, rather
		// than as the session's own reason, which the stream never carried.
		let code = to_stream_code(&StreamError::Session(SessionError::Unauthorized));
		assert_eq!(code, SESSION_CLOSED);
		assert_eq!(from_stream_code(code), StreamError::Session(SessionError::Internal));
	}

	/// Conditions this registry has no value for say INTERNAL_ERROR rather than borrowing
	/// moq-lite's own 48-63 range or its application offset. A peer treats an unregistered
	/// code as INTERNAL_ERROR anyway, so the borrowed value would
	/// carry no more meaning while looking like a registration.
	#[test]
	fn unregistered_conditions_are_internal() {
		for err in [
			StreamError::NotFound,
			StreamError::NotFetchable,
			StreamError::Unroutable,
			StreamError::Old,
			StreamError::Evicted,
			StreamError::WrongSize,
			StreamError::FrameTooLarge,
			StreamError::GroupTooLarge,
			StreamError::TimestampMismatch,
			StreamError::Unauthorized,
			StreamError::ControlTimeout,
			StreamError::App(7),
			StreamError::Unknown(0x1234),
		] {
			assert_eq!(
				to_stream_code(&err),
				INTERNAL_ERROR,
				"{err:?} has no value in this registry"
			);
		}

		// And nothing decodes back into them: an unregistered code keeps its number and
		// stays opaque instead of being read as a meaning the wire did not carry.
		for code in [
			0x6,
			0x7,
			0x9,
			0x20,
			0x22,
			0x33,
			0x34,
			0x35,
			0x36,
			0x37,
			0x38,
			0x39,
			0x3a,
			0x3b,
			64 + 7,
		] {
			assert_eq!(from_stream_code(code), StreamError::Unknown(code));
			assert!(matches!(
				Error::from(from_stream_code(code)),
				Error::Stream(StreamError::Unknown(remote)) if remote == code
			));
		}
	}

	/// Every stream error this crate can hold, so the conformance check below covers the
	/// whole space rather than the variants someone remembered. A new variant belongs here.
	const EVERY_ERROR: [StreamError; 20] = [
		StreamError::Session(SessionError::Cancel),
		StreamError::Internal,
		StreamError::Cancel,
		StreamError::DeliveryTimeout,
		StreamError::ControlTimeout,
		StreamError::GoingAway,
		StreamError::TooFarBehind,
		StreamError::MalformedTrack,
		StreamError::NotFound,
		StreamError::NotFetchable,
		StreamError::Unroutable,
		StreamError::Old,
		StreamError::Evicted,
		StreamError::WrongSize,
		StreamError::FrameTooLarge,
		StreamError::GroupTooLarge,
		StreamError::TimestampMismatch,
		StreamError::Unauthorized,
		StreamError::App(7),
		StreamError::Unknown(0x22),
	];

	/// Every code we can put on a moq-transport stream has to be one the negotiated draft
	/// registers. moq-lite's own table is not: it assigns values in 48-63 and offsets
	/// application codes past 64, neither of which this registry has a range for,
	/// and it assigns 0x4 and 0x5 meanings the earlier drafts give to something else.
	///
	/// The table is transcribed from the drafts (draft-18 section 15.11.4 on), not derived
	/// from the mapping, so a mistake in the mapping cannot talk the assertion into
	/// agreeing with it.
	#[test]
	fn only_registered_codes_reach_the_wire() {
		const REGISTERED: &[u32] = &[0x0, 0x1, 0x2, 0x3, 0x4, 0x5, 0x6, 0x7, 0x9, 0x12];
		for err in EVERY_ERROR {
			let code = to_stream_code(&err);
			assert!(
				REGISTERED.contains(&code),
				"{err:?} sends {code:#x}, which is not registered"
			);
		}
	}

	/// A relay decodes a peer's code and re-encodes it onto the stream it tears down in
	/// response. That hop must not change what the code says.
	#[test]
	fn relaying_a_code_does_not_change_its_meaning() {
		for code in [INTERNAL_ERROR, CANCELLED, DELIVERY_TIMEOUT, SESSION_CLOSED] {
			let relayed = StreamError::from(&Error::from(from_stream_code(code)));
			assert_eq!(to_stream_code(&relayed), code, "{code:#x} changed across a relay");
		}
	}
}
