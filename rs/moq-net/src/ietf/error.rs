//! The moq-transport stream reset code registry, per negotiated draft.
//!
//! Sent on RESET_STREAM and STOP_SENDING, and read back off both. The values are the
//! draft's, not [`StreamError::to_code`]'s: the two registries agree on most of what they
//! both assign, but not all of it, and this one grew (and moved a value) across the drafts
//! we negotiate, so a code only means something once you know which draft carried it.
//!
//! | Code | draft-14/15 | draft-16 | draft-17 | draft-18+ |
//! |------|-------------|----------|----------|-----------|
//! | 0x0  | INTERNAL_ERROR | INTERNAL_ERROR | INTERNAL_ERROR | INTERNAL_ERROR |
//! | 0x1  | CANCELLED | CANCELLED | CANCELLED | CANCELLED |
//! | 0x2  | DELIVERY_TIMEOUT | DELIVERY_TIMEOUT | DELIVERY_TIMEOUT | DELIVERY_TIMEOUT |
//! | 0x3  | SESSION_CLOSED | SESSION_CLOSED | SESSION_CLOSED | SESSION_CLOSED |
//! | 0x4  | - | UNKNOWN_OBJECT_STATUS | UNKNOWN_OBJECT_STATUS | GOING_AWAY |
//! | 0x5  | - | - | TOO_FAR_BEHIND | TOO_FAR_BEHIND |
//! | 0x12 | - | MALFORMED_TRACK | MALFORMED_TRACK | MALFORMED_TRACK |
//!
//! Encoding and decoding therefore move together and both take the negotiated version:
//! GOING_AWAY sent to a draft-17 peer reads as UNKNOWN_OBJECT_STATUS, and a draft-17
//! peer's UNKNOWN_OBJECT_STATUS read as GOING_AWAY would retire a session that is not
//! going anywhere.

use super::Version;
use crate::{SessionError, StreamError};

/// An implementation-specific error: the stream died on our side, with no registry entry
/// for why. Assigned by every draft we negotiate.
pub const INTERNAL_ERROR: u32 = 0x0;

/// The stream was cancelled by either endpoint. A routine unsubscribe, not a failure.
/// Assigned by every draft we negotiate.
pub const CANCELLED: u32 = 0x1;

/// The content missed its delivery deadline.
const DELIVERY_TIMEOUT: u32 = 0x2;

/// The session is closing, taking this stream with it.
const SESSION_CLOSED: u32 = 0x3;

/// A GOAWAY was sent or received. Draft-18 and later; draft-16 and 17 gave 0x4 to
/// UNKNOWN_OBJECT_STATUS instead.
const GOING_AWAY: u32 = 0x4;

/// The subscription outran the publisher's resource limits. Draft-17 and later.
const TOO_FAR_BEHIND: u32 = 0x5;

/// The track's content could not be parsed. Draft-16 and later.
const MALFORMED_TRACK: u32 = 0x12;

/// Whether the draft assigns 0x4 to GOING_AWAY.
///
/// Draft-16 and 17 assign it to UNKNOWN_OBJECT_STATUS, which draft-18 moved to 0x6 when it
/// took 0x4 for this. Draft-14 and 15 assign it nothing.
fn has_going_away(version: Version) -> bool {
	matches!(version, Version::Draft18 | Version::Draft19 | Version::Draft20)
}

/// Whether the draft assigns TOO_FAR_BEHIND. Added in draft-17.
fn has_too_far_behind(version: Version) -> bool {
	!matches!(version, Version::Draft14 | Version::Draft15 | Version::Draft16)
}

/// Whether the draft assigns MALFORMED_TRACK. Added in draft-16.
fn has_malformed_track(version: Version) -> bool {
	!matches!(version, Version::Draft14 | Version::Draft15)
}

/// The code to reset a stream, or send STOP_SENDING, with on the negotiated draft.
///
/// Only values the draft registers go out. Everything else is INTERNAL_ERROR, which is not
/// a loss: draft-20 section 14 makes a receiver treat any unregistered code as equivalent
/// to INTERNAL_ERROR, so an unregistered value would say the same thing less clearly. That
/// covers the conditions moq-lite carries in its provisional 32-63 range (a group dropped
/// as old or evicted, a malformed frame size) and application codes, which this registry
/// has no range for at all: 64 and above is ordinary registry space here, and part of it is
/// reserved for greasing.
pub fn to_stream_code(err: &StreamError, version: Version) -> u32 {
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
		StreamError::GoingAway if has_going_away(version) => GOING_AWAY,
		StreamError::TooFarBehind if has_too_far_behind(version) => TOO_FAR_BEHIND,
		StreamError::MalformedTrack if has_malformed_track(version) => MALFORMED_TRACK,
		_ => INTERNAL_ERROR,
	}
}

/// Read a stream reset (or STOP_SENDING) code received on the negotiated draft.
///
/// A code the draft does not assign stays [`StreamError::Unknown`], which surfaces as
/// [`Error::Remote`](crate::Error::Remote): an error, but never one given a meaning it did
/// not carry. That includes the codes this crate has no local counterpart for
/// (UNKNOWN_OBJECT_STATUS, EXPIRED_AUTH_TOKEN, EXCESSIVE_LOAD) and every value a later
/// draft may add.
pub fn from_stream_code(code: u32, version: Version) -> StreamError {
	match code {
		INTERNAL_ERROR => StreamError::Internal,
		CANCELLED => StreamError::Cancel,
		DELIVERY_TIMEOUT => StreamError::DeliveryTimeout,
		// The peer's session code is not on this stream, so the reason is unknown here.
		SESSION_CLOSED => StreamError::Session(SessionError::Internal),
		GOING_AWAY if has_going_away(version) => StreamError::GoingAway,
		TOO_FAR_BEHIND if has_too_far_behind(version) => StreamError::TooFarBehind,
		MALFORMED_TRACK if has_malformed_track(version) => StreamError::MalformedTrack,
		code => StreamError::Unknown(code),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::Error;

	const ALL: [Version; 7] = [
		Version::Draft14,
		Version::Draft15,
		Version::Draft16,
		Version::Draft17,
		Version::Draft18,
		Version::Draft19,
		Version::Draft20,
	];

	/// A routine unsubscribe must not read as a fault on our side. moq-lite's own error
	/// enum encodes a cancellation as 0, which is this wire's INTERNAL_ERROR, so the codes
	/// come from here instead.
	#[test]
	fn a_cancellation_is_not_an_internal_error() {
		for version in ALL {
			assert_eq!(to_stream_code(&StreamError::Cancel, version), CANCELLED);
			assert_eq!(from_stream_code(CANCELLED, version), StreamError::Cancel);
			assert!(matches!(
				Error::from(from_stream_code(CANCELLED, version)),
				Error::Cancel
			));
		}

		assert_ne!(CANCELLED, Error::Cancel.to_code(), "the two spaces disagree about 0");
		assert_eq!(Error::Cancel.to_code(), INTERNAL_ERROR);
	}

	/// Every code we send must decode back to what we meant on the same draft, or two
	/// moq-net peers disagree about what a stream reset said.
	#[test]
	fn every_emitted_code_round_trips() {
		let errors = [
			StreamError::Internal,
			StreamError::Cancel,
			StreamError::DeliveryTimeout,
			StreamError::GoingAway,
			StreamError::TooFarBehind,
			StreamError::MalformedTrack,
			StreamError::NotFound,
			StreamError::Old,
			StreamError::Evicted,
			StreamError::App(7),
		];

		for version in ALL {
			for err in &errors {
				let code = to_stream_code(err, version);
				let decoded = from_stream_code(code, version);
				assert_eq!(
					to_stream_code(&decoded, version),
					code,
					"{err:?} on {version:?} did not survive a round trip"
				);
			}

			// A session teardown flattens to SESSION_CLOSED and comes back as one, rather
			// than as the session's own reason, which the stream never carried.
			let code = to_stream_code(&StreamError::Session(SessionError::Unauthorized), version);
			assert_eq!(code, SESSION_CLOSED);
			assert_eq!(
				from_stream_code(code, version),
				StreamError::Session(SessionError::Internal)
			);
		}
	}

	/// Draft-18 took 0x4 for GOING_AWAY from UNKNOWN_OBJECT_STATUS, so the same integer
	/// means different things on two drafts we both negotiate. Sending it to draft-17 would
	/// claim the next object's status is unknowable; reading theirs as GOING_AWAY would
	/// start draining a session that is not going anywhere.
	#[test]
	fn going_away_only_exists_from_draft_18() {
		for version in [Version::Draft14, Version::Draft15, Version::Draft16, Version::Draft17] {
			assert_eq!(to_stream_code(&StreamError::GoingAway, version), INTERNAL_ERROR);
			assert_eq!(from_stream_code(GOING_AWAY, version), StreamError::Unknown(GOING_AWAY));
		}

		for version in [Version::Draft18, Version::Draft19, Version::Draft20] {
			assert_eq!(to_stream_code(&StreamError::GoingAway, version), GOING_AWAY);
			assert_eq!(from_stream_code(GOING_AWAY, version), StreamError::GoingAway);
		}
	}

	/// The rest of the per-draft registry: TOO_FAR_BEHIND arrived in draft-17 and
	/// MALFORMED_TRACK in draft-16, so an older peer must be told neither.
	#[test]
	fn later_codes_are_not_sent_to_earlier_drafts() {
		for version in ALL {
			let too_far_behind = to_stream_code(&StreamError::TooFarBehind, version);
			let malformed = to_stream_code(&StreamError::MalformedTrack, version);

			assert_eq!(
				too_far_behind,
				match has_too_far_behind(version) {
					true => TOO_FAR_BEHIND,
					false => INTERNAL_ERROR,
				},
				"{version:?} disagrees about TOO_FAR_BEHIND"
			);
			assert_eq!(
				malformed,
				match has_malformed_track(version) {
					true => MALFORMED_TRACK,
					false => INTERNAL_ERROR,
				},
				"{version:?} disagrees about MALFORMED_TRACK"
			);
		}

		assert_eq!(
			from_stream_code(TOO_FAR_BEHIND, Version::Draft16),
			StreamError::Unknown(TOO_FAR_BEHIND)
		);
		assert_eq!(
			from_stream_code(MALFORMED_TRACK, Version::Draft15),
			StreamError::Unknown(MALFORMED_TRACK)
		);
	}

	/// Conditions this registry has no value for say INTERNAL_ERROR rather than borrowing
	/// moq-lite's provisional 32-63 range or its application offset. A peer treats an
	/// unregistered code as INTERNAL_ERROR anyway (draft-20 section 14), so the placeholder
	/// would carry no more meaning while looking like a registration.
	#[test]
	fn unregistered_conditions_are_internal() {
		for err in [
			StreamError::NotFound,
			StreamError::Unroutable,
			StreamError::Old,
			StreamError::Evicted,
			StreamError::WrongSize,
			StreamError::FrameTooLarge,
			StreamError::TimestampMismatch,
			StreamError::App(7),
			StreamError::Unknown(0x1234),
		] {
			assert_eq!(
				to_stream_code(&err, Version::Draft20),
				INTERNAL_ERROR,
				"{err:?} has no value in this registry"
			);
		}

		// And nothing decodes back into them: an unregistered code keeps its number and
		// stays opaque instead of being read as a meaning the wire did not carry.
		for code in [0x6, 0x7, 0x9, 0x20, 0x22, 64 + 7] {
			assert_eq!(from_stream_code(code, Version::Draft20), StreamError::Unknown(code));
			assert!(matches!(
				Error::from(from_stream_code(code, Version::Draft20)),
				Error::Remote(remote) if remote == code
			));
		}
	}

	/// Every stream error this crate can hold, so the conformance check below covers the
	/// whole space rather than the variants someone remembered. A new variant belongs here.
	const EVERY_ERROR: [StreamError; 16] = [
		StreamError::Session(SessionError::Cancel),
		StreamError::Internal,
		StreamError::Cancel,
		StreamError::DeliveryTimeout,
		StreamError::GoingAway,
		StreamError::TooFarBehind,
		StreamError::MalformedTrack,
		StreamError::NotFound,
		StreamError::Unroutable,
		StreamError::Old,
		StreamError::Evicted,
		StreamError::WrongSize,
		StreamError::FrameTooLarge,
		StreamError::TimestampMismatch,
		StreamError::App(7),
		StreamError::Unknown(0x22),
	];

	/// Every code we can put on a moq-transport stream has to be one the negotiated draft
	/// registers. moq-lite's own table is not: it emits provisional values in 32-63 and
	/// offsets application codes past 64, neither of which this registry has a range for,
	/// and it assigns 0x4 and 0x5 meanings the earlier drafts give to something else.
	///
	/// The table is transcribed from the drafts (draft-14 section 13.1.8 through draft-20
	/// section 15.11.4), not derived from the mapping, so a mistake in the mapping cannot
	/// talk the assertion into agreeing with it.
	#[test]
	fn only_registered_codes_reach_the_wire() {
		fn registered(version: Version) -> &'static [u32] {
			match version {
				Version::Draft14 | Version::Draft15 => &[0x0, 0x1, 0x2, 0x3],
				Version::Draft16 => &[0x0, 0x1, 0x2, 0x3, 0x4, 0x12],
				Version::Draft17 => &[0x0, 0x1, 0x2, 0x3, 0x4, 0x5, 0x9, 0x12],
				_ => &[0x0, 0x1, 0x2, 0x3, 0x4, 0x5, 0x6, 0x7, 0x9, 0x12],
			}
		}

		for version in ALL {
			for err in EVERY_ERROR {
				let code = to_stream_code(&err, version);
				assert!(
					registered(version).contains(&code),
					"{err:?} sends {code:#x}, which {version} does not register"
				);
			}
		}
	}

	/// A relay decodes a peer's code and re-encodes it onto the stream it tears down in
	/// response. That hop must not change what the code says, on the same draft.
	#[test]
	fn relaying_a_code_does_not_change_its_meaning() {
		for version in ALL {
			for code in [INTERNAL_ERROR, CANCELLED, DELIVERY_TIMEOUT, SESSION_CLOSED] {
				let relayed = StreamError::from(&Error::from(from_stream_code(code, version)));
				assert_eq!(
					to_stream_code(&relayed, version),
					code,
					"{code:#x} changed across a relay on {version:?}"
				);
			}
		}
	}
}
