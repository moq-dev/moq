//! The moq-transport error code registries, per negotiated draft.
//!
//! This module holds the stream reset registry; [`request`] holds the one for rejecting a
//! request. The two are disjoint, so the same integer means different things in each.
//!
//! # Stream reset codes
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
	matches!(
		version,
		Version::Draft18 | Version::Draft19 | Version::Draft20 | Version::Draft21
	)
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
/// Conditions without a matching reset code use INTERNAL_ERROR. Request rejection has
/// its own registry: a missing track belongs in a request error response, not a reset.
/// moq-lite's provisional and application ranges have no corresponding ranges here.
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

/// The moq-transport request error registry, per negotiated draft.
///
/// Sent in a rejection response to a request, and read back off one. Draft-14 gives each
/// error message its own registry and they disagree about 0x4; draft-15 folded them all
/// into REQUEST_ERROR with a single registry that renumbered almost everything. So a
/// request code only means something once you know both the draft and, on draft-14, which
/// request it answers.
///
/// | Condition | 14 SUBSCRIBE | 14 FETCH | 14 PUBLISH | 14 ANNOUNCE | 14 SUB_NS | 15+ |
/// |-----------|--------------|----------|------------|-------------|-----------|-----|
/// | INTERNAL_ERROR | 0x0 | 0x0 | 0x0 | 0x0 | 0x0 | 0x0 |
/// | UNAUTHORIZED | 0x1 | 0x1 | 0x1 | 0x1 | 0x1 | 0x1 |
/// | TIMEOUT | 0x2 | 0x2 | 0x2 | 0x2 | 0x2 | 0x2 |
/// | NOT_SUPPORTED | 0x3 | 0x3 | 0x3 | 0x3 | 0x3 | 0x3 |
/// | DOES_NOT_EXIST | 0x4 | 0x4 | - | - | - | 0x10 |
/// | UNINTERESTED | - | - | 0x4 | 0x4 | - | 0x20 |
/// | MALFORMED_TRACK | - | 0x9 | - | - | - | 0x12 |
/// | GOING_AWAY | - | - | - | - | - | 0x6 (17+) |
///
/// The renumbering is the trap: draft-14 SUBSCRIBE_ERROR gives 0x4 to TRACK_DOES_NOT_EXIST
/// and 0x10 to MALFORMED_AUTH_TOKEN, and draft-15 swaps the two. Sending either number
/// without the draft in hand tells the peer the opposite of what happened.
pub(crate) mod request {
	use super::Version;
	use crate::Error;

	/// Which request the error answers, so draft-14 picks the right registry.
	///
	/// Draft-15 and later carry one REQUEST_ERROR registry for every request, so from there
	/// on the kind only decides what a refused route says: not here to a request for
	/// content, uninterested to an offer of it.
	#[derive(Clone, Copy, Debug, PartialEq, Eq)]
	pub(crate) enum Kind {
		/// SUBSCRIBE_ERROR, draft-14 section 13.1.2.
		Subscribe,
		/// FETCH_ERROR, draft-14 section 13.1.5.
		Fetch,
		/// PUBLISH_ERROR, draft-14 section 13.1.4.
		Publish,
		/// ANNOUNCE_ERROR, draft-14 section 13.1.6.
		PublishNamespace,
		/// SUBSCRIBE_NAMESPACE_ERROR, draft-14 section 13.1.7.
		SubscribeNamespace,
	}

	/// An implementation-specific error, with no registry entry for why. Assigned by every
	/// draft, for every request.
	const INTERNAL_ERROR: u64 = 0x0;

	/// The peer's credentials do not cover the request.
	const UNAUTHORIZED: u64 = 0x1;

	/// The request could not be answered before an implementation-specific deadline.
	const TIMEOUT: u64 = 0x2;

	/// The endpoint does not implement this request at all, as opposed to refusing this one.
	const NOT_SUPPORTED: u64 = 0x3;

	/// A GOAWAY is draining the session, so no new request is accepted. Draft-17 and later.
	const GOING_AWAY: u64 = 0x6;

	/// The requested broadcast or track is not here, as draft-14 numbers it under the name
	/// TRACK_DOES_NOT_EXIST.
	const DOES_NOT_EXIST_14: u64 = 0x4;

	/// The same condition from draft-15 on, renamed DOES_NOT_EXIST and moved off 0x4.
	const DOES_NOT_EXIST: u64 = 0x10;

	/// The offered content is not wanted here, as draft-14 numbers it.
	const UNINTERESTED_14: u64 = 0x4;

	/// The same condition from draft-15 on.
	const UNINTERESTED: u64 = 0x20;

	/// The track's content could not be parsed, as draft-14 numbers it on FETCH_ERROR.
	const MALFORMED_TRACK_14: u64 = 0x9;

	/// The same condition from draft-15 on.
	const MALFORMED_TRACK: u64 = 0x12;

	/// The value for "the thing you asked for is not here", or `None` where the draft and
	/// request assign none.
	fn does_not_exist(kind: Kind, version: Version) -> Option<u64> {
		match version {
			Version::Draft14 => match kind {
				Kind::Subscribe | Kind::Fetch => Some(DOES_NOT_EXIST_14),
				_ => None,
			},
			_ => Some(DOES_NOT_EXIST),
		}
	}

	/// The value for "we do not want this", or `None` where the draft and request assign
	/// none. Draft-14 registers it only on the two requests that offer content.
	fn uninterested(kind: Kind, version: Version) -> Option<u64> {
		match version {
			Version::Draft14 => match kind {
				Kind::Publish | Kind::PublishNamespace => Some(UNINTERESTED_14),
				_ => None,
			},
			_ => Some(UNINTERESTED),
		}
	}

	/// The value for a track we could not parse, or `None` where the draft and request
	/// assign none.
	fn malformed_track(kind: Kind, version: Version) -> Option<u64> {
		match version {
			Version::Draft14 => match kind {
				Kind::Fetch => Some(MALFORMED_TRACK_14),
				_ => None,
			},
			_ => Some(MALFORMED_TRACK),
		}
	}

	/// The value for a draining session, or `None` before draft-17 registered one.
	fn going_away(version: Version) -> Option<u64> {
		match version {
			Version::Draft14 | Version::Draft15 | Version::Draft16 => None,
			_ => Some(GOING_AWAY),
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
			Error::Unauthorized => return UNAUTHORIZED,
			Error::Timeout => return TIMEOUT,
			Error::Unsupported | Error::Version => return NOT_SUPPORTED,
			Error::NotFound => does_not_exist(kind, version),
			// A path with no route is one we will not carry. A subscriber that asked for it
			// cannot act on "we do not want it": what it needs to know is that we do not have
			// it, which is the same refusal from its side. Only the requests that offer content
			// say UNINTERESTED.
			Error::Unroutable => match kind {
				Kind::Subscribe | Kind::Fetch => does_not_exist(kind, version),
				Kind::Publish | Kind::PublishNamespace | Kind::SubscribeNamespace => uninterested(kind, version),
			},
			// Our own parse failure is, from the peer's side, a malformed track.
			Error::Decode(_) | Error::BoundsExceeded(_) | Error::MalformedTrack => malformed_track(kind, version),
			Error::GoingAway => going_away(version),
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
			code if Some(code) == does_not_exist(kind, version) => Error::NotFound,
			code if Some(code) == uninterested(kind, version) => Error::Unroutable,
			code if Some(code) == malformed_track(kind, version) => Error::MalformedTrack,
			code if Some(code) == going_away(version) => Error::GoingAway,
			code => Error::Remote(u32::try_from(code).unwrap_or(u32::MAX)),
		}
	}

	#[cfg(test)]
	mod tests {
		use super::*;

		const ALL: [Version; 8] = [
			Version::Draft14,
			Version::Draft15,
			Version::Draft16,
			Version::Draft17,
			Version::Draft18,
			Version::Draft19,
			Version::Draft20,
			Version::Draft21,
		];

		const KINDS: [Kind; 5] = [
			Kind::Subscribe,
			Kind::Fetch,
			Kind::Publish,
			Kind::PublishNamespace,
			Kind::SubscribeNamespace,
		];

		/// Every error a rejection distinguishes, plus one it does not, so the checks below cover
		/// the whole registry rather than the variants someone remembered. A new arm in
		/// [`to_code`] belongs here.
		const EVERY_ERROR: [Error; 9] = [
			Error::Duplicate,
			Error::Unauthorized,
			Error::Timeout,
			Error::Unsupported,
			Error::NotFound,
			Error::Unroutable,
			Error::MalformedTrack,
			Error::GoingAway,
			Error::Remote(0x30),
		];

		/// Draft-14 numbers a missing track 0x4 and draft-15 moved it to 0x10, which is
		/// draft-14's MALFORMED_AUTH_TOKEN. Getting this backwards tells a peer its token is
		/// broken when the broadcast simply is not here, so it re-authenticates instead of
		/// waiting for the announcement.
		#[test]
		fn a_missing_broadcast_uses_the_draft_s_own_number() {
			for kind in [Kind::Subscribe, Kind::Fetch] {
				assert_eq!(to_code(&Error::NotFound, kind, Version::Draft14), 0x4);
				assert!(matches!(from_code(0x4, kind, Version::Draft14), Error::NotFound));

				for version in ALL.into_iter().skip(1) {
					assert_eq!(to_code(&Error::NotFound, kind, version), 0x10);
					assert!(matches!(from_code(0x10, kind, version), Error::NotFound));
					// 0x4 is MALFORMED_AUTH_TOKEN from draft-15 on, which is not ours to claim.
					assert!(matches!(from_code(0x4, kind, version), Error::Remote(0x4)));
				}
			}

			// A broadcast with no route is, to the peer that asked for it, not here either.
			assert_eq!(to_code(&Error::Unroutable, Kind::Subscribe, Version::Draft20), 0x10);
		}

		/// Draft-14 gives 0x4 to UNINTERESTED on the requests that offer content and to
		/// TRACK_DOES_NOT_EXIST on the ones that ask for it, so the same integer on the same
		/// draft says two different things.
		#[test]
		fn draft_14_reads_0x4_per_request() {
			for kind in [Kind::Publish, Kind::PublishNamespace] {
				assert_eq!(to_code(&Error::Unroutable, kind, Version::Draft14), 0x4);
				assert!(matches!(from_code(0x4, kind, Version::Draft14), Error::Unroutable));
				// Those requests offer content, so "it is not here" is not a rejection they
				// can carry.
				assert_eq!(to_code(&Error::NotFound, kind, Version::Draft14), INTERNAL_ERROR);
			}

			// And neither meaning reaches a request draft-14 gives neither to.
			assert!(matches!(
				from_code(0x4, Kind::SubscribeNamespace, Version::Draft14),
				Error::Remote(0x4)
			));
		}

		/// GOING_AWAY arrived in draft-17. Sent to an earlier peer, 0x6 is unassigned there,
		/// so say INTERNAL_ERROR outright rather than a number that means nothing.
		#[test]
		fn going_away_only_exists_from_draft_17() {
			for kind in KINDS {
				for version in [Version::Draft14, Version::Draft15, Version::Draft16] {
					assert_eq!(to_code(&Error::GoingAway, kind, version), INTERNAL_ERROR);
					assert!(matches!(from_code(GOING_AWAY, kind, version), Error::Remote(0x6)));
				}

				for version in [
					Version::Draft17,
					Version::Draft18,
					Version::Draft19,
					Version::Draft20,
					Version::Draft21,
				] {
					assert_eq!(to_code(&Error::GoingAway, kind, version), GOING_AWAY);
					assert!(matches!(from_code(GOING_AWAY, kind, version), Error::GoingAway));
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
		/// that request. The table is transcribed from the drafts (draft-14 section 13.1
		/// through draft-20 section 15.11.2), not derived from the mapping, so a mistake in
		/// the mapping cannot talk the assertion into agreeing with it.
		#[test]
		fn only_registered_codes_reach_the_wire() {
			fn registered(kind: Kind, version: Version) -> &'static [u64] {
				match (version, kind) {
					(Version::Draft14, Kind::Subscribe) => &[0x0, 0x1, 0x2, 0x3, 0x4, 0x5, 0x10, 0x12],
					(Version::Draft14, Kind::Fetch) => &[0x0, 0x1, 0x2, 0x3, 0x4, 0x5, 0x6, 0x7, 0x8, 0x9, 0x10, 0x12],
					(Version::Draft14, Kind::Publish) => &[0x0, 0x1, 0x2, 0x3, 0x4],
					(Version::Draft14, Kind::PublishNamespace) => &[0x0, 0x1, 0x2, 0x3, 0x4, 0x10, 0x12],
					(Version::Draft14, Kind::SubscribeNamespace) => &[0x0, 0x1, 0x2, 0x3, 0x4, 0x5, 0x10, 0x12],
					(Version::Draft15, _) => &[0x0, 0x1, 0x2, 0x3, 0x4, 0x5, 0x10, 0x11, 0x12, 0x20, 0x30, 0x32, 0x33],
					(Version::Draft16, _) => &[0x0, 0x1, 0x2, 0x3, 0x4, 0x5, 0x10, 0x11, 0x12, 0x19, 0x20, 0x30, 0x32],
					(Version::Draft17, _) => &[
						0x0, 0x1, 0x2, 0x3, 0x4, 0x5, 0x6, 0x9, 0x10, 0x11, 0x12, 0x19, 0x20, 0x30, 0x31, 0x32,
					],
					_ => &[
						0x0, 0x1, 0x2, 0x3, 0x4, 0x5, 0x6, 0x9, 0x10, 0x11, 0x12, 0x19, 0x20, 0x30, 0x31, 0x32, 0x33,
						0x34, 0x35, 0x36,
					],
				}
			}

			for version in ALL {
				for kind in KINDS {
					for err in &EVERY_ERROR {
						let code = to_code(err, kind, version);
						assert!(
							registered(kind, version).contains(&code),
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
						to_code(&err, kind, Version::Draft20),
						INTERNAL_ERROR,
						"{err} is not internal on {kind:?}"
					);
				}
			}

			assert!(matches!(
				from_code(INTERNAL_ERROR, Kind::Subscribe, Version::Draft20),
				Error::Remote(0)
			));
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::Error;

	const ALL: [Version; 8] = [
		Version::Draft14,
		Version::Draft15,
		Version::Draft16,
		Version::Draft17,
		Version::Draft18,
		Version::Draft19,
		Version::Draft20,
		Version::Draft21,
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

		for version in [Version::Draft18, Version::Draft19, Version::Draft20, Version::Draft21] {
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
			StreamError::GroupTooLarge,
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
		for code in [0x6, 0x7, 0x9, 0x20, 0x22, 0x33, 0x34, 0x35, 64 + 7] {
			assert_eq!(from_stream_code(code, Version::Draft20), StreamError::Unknown(code));
			assert!(matches!(
				Error::from(from_stream_code(code, Version::Draft20)),
				Error::Remote(remote) if remote == code
			));
		}
	}

	/// Every stream error this crate can hold, so the conformance check below covers the
	/// whole space rather than the variants someone remembered. A new variant belongs here.
	const EVERY_ERROR: [StreamError; 17] = [
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
		StreamError::GroupTooLarge,
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
