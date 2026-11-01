//! Errors returned across the UniFFI boundary.
//!
//! Protocol failures carry the peer's session or stream code verbatim. Transport and
//! internal failures stay in their own variants, so a binding can tell a wire code from
//! a local mistake.

use std::fmt;

/// Which registry a protocol code belongs to. Session and stream codes are disjoint, so
/// the same integer is a different failure in each.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum MoqErrorScope {
	/// A session close code.
	Session,
	/// A stream reset or stop code.
	Stream,
}

/// A recognized protocol kind, or [`Self::App`] / [`Self::Unknown`] when the code is not
/// one of the named ones. Pair with [`MoqErrorScope`]: `Cancel` is 0 on a session and 1
/// on a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum MoqProtocolKind {
	/// Ending normally, with no error. Session 0, stream 1.
	Cancel,
	/// Something went wrong that isn't worth a dedicated code. Session 1, stream 0.
	Internal,
	/// The peer's token does not grant the requested path or operation. Session 2, stream 0x3B.
	Unauthorized,
	/// The peer broke a protocol rule; the session is unusable.
	ProtocolViolation,
	/// A key-value pair was malformed or repeated more than allowed.
	KeyValueFormatting,
	/// The peer did not close within the GOAWAY drain deadline.
	GoawayTimeout,
	/// A control message took too long.
	Timeout,
	/// No version could be negotiated.
	Version,
	/// The content missed its delivery deadline.
	DeliveryTimeout,
	/// The session ended, taking this stream with it.
	SessionClosed,
	/// The session is going away (a GOAWAY was received).
	GoingAway,
	/// The reader fell too far behind and content was dropped to catch up.
	TooFarBehind,
	/// The track's content could not be parsed.
	MalformedTrack,
	/// The requested broadcast or track does not exist at the peer.
	NotFound,
	/// The broadcast is neither announced nor served, so there is no route to it.
	Unroutable,
	/// The group was superseded by a newer group and dropped.
	Old,
	/// The group was dropped under memory pressure.
	Evicted,
	/// A frame's payload length disagreed with its declared size.
	WrongSize,
	/// A frame declared a payload larger than the receiver accepts.
	FrameTooLarge,
	/// A frame's timestamp doesn't match its track's negotiated timescale.
	TimestampMismatch,
	/// An application-chosen code, offset into the 64+ range on the wire.
	App,
	/// A code this version does not recognize; [`MoqProtocolError::code`] is the value.
	Unknown,
	/// The session exceeded its announcement or subscription cap.
	TooManyRequests,
	/// The authorization token expired and can be refreshed. Session 0x18.
	Expired,
	/// A token registration exceeded the advertised cache size.
	AuthTokenCacheOverflow,
	/// The peer could not parse an authorization token.
	MalformedAuthToken,
	/// A token referred to an alias that was never registered.
	UnknownAuthTokenAlias,
	/// The peer reused or sent an invalid request ID.
	InvalidRequestId,
	/// The peer sent a path it may not send.
	InvalidPath,
	/// The peer sent an authority it may not send.
	InvalidAuthority,
	/// The peer exceeded its outstanding request update limit.
	TooManyRequestUpdates,
}

/// A protocol failure a peer sent (or this side will send): scope, verbatim code, kind,
/// and a diagnostic message.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct MoqProtocolError {
	/// Whether this code is from the session or stream registry.
	pub scope: MoqErrorScope,
	/// The integer on the wire, kept verbatim. Do not re-derive this from [`Self::kind`]:
	/// App and Unknown each cover many codes, and the same kind is a different integer in each scope.
	pub code: u32,
	/// The known kind when the code is recognized, otherwise [`MoqProtocolKind::App`] or
	/// [`MoqProtocolKind::Unknown`].
	pub kind: MoqProtocolKind,
	/// Human-readable reason, for logs. Do not parse this.
	pub message: String,
}

impl fmt::Display for MoqProtocolError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(&self.message)
	}
}

impl MoqProtocolError {
	/// Build from a received or to-be-sent session error, carrying its code verbatim.
	pub(crate) fn from_session(err: &moq_net::SessionError) -> Self {
		Self {
			scope: MoqErrorScope::Session,
			code: err.to_code(),
			kind: session_kind(err),
			message: err.to_string(),
		}
	}

	/// Build from a received or to-be-sent stream error, carrying its code verbatim.
	pub(crate) fn from_stream(err: &moq_net::StreamError) -> Self {
		Self {
			scope: MoqErrorScope::Stream,
			code: err.to_code(),
			kind: stream_kind(err),
			message: err.to_string(),
		}
	}
}

fn session_kind(err: &moq_net::SessionError) -> MoqProtocolKind {
	match err {
		moq_net::SessionError::Cancel => MoqProtocolKind::Cancel,
		moq_net::SessionError::Internal => MoqProtocolKind::Internal,
		moq_net::SessionError::Unauthorized => MoqProtocolKind::Unauthorized,
		moq_net::SessionError::Expired => MoqProtocolKind::Expired,
		moq_net::SessionError::ProtocolViolation => MoqProtocolKind::ProtocolViolation,
		moq_net::SessionError::KeyValueFormatting => MoqProtocolKind::KeyValueFormatting,
		moq_net::SessionError::GoawayTimeout => MoqProtocolKind::GoawayTimeout,
		moq_net::SessionError::Timeout => MoqProtocolKind::Timeout,
		moq_net::SessionError::Version => MoqProtocolKind::Version,
		moq_net::SessionError::TooManyRequests => MoqProtocolKind::TooManyRequests,
		moq_net::SessionError::AuthTokenCacheOverflow => MoqProtocolKind::AuthTokenCacheOverflow,
		moq_net::SessionError::MalformedAuthToken => MoqProtocolKind::MalformedAuthToken,
		moq_net::SessionError::UnknownAuthTokenAlias => MoqProtocolKind::UnknownAuthTokenAlias,
		moq_net::SessionError::InvalidRequestId => MoqProtocolKind::InvalidRequestId,
		moq_net::SessionError::InvalidPath => MoqProtocolKind::InvalidPath,
		moq_net::SessionError::InvalidAuthority => MoqProtocolKind::InvalidAuthority,
		moq_net::SessionError::TooManyRequestUpdates => MoqProtocolKind::TooManyRequestUpdates,
		moq_net::SessionError::App(_) => MoqProtocolKind::App,
		moq_net::SessionError::Unknown(_) => MoqProtocolKind::Unknown,
		_ => MoqProtocolKind::Unknown,
	}
}

fn stream_kind(err: &moq_net::StreamError) -> MoqProtocolKind {
	match err {
		moq_net::StreamError::Session(_) => MoqProtocolKind::SessionClosed,
		moq_net::StreamError::Internal => MoqProtocolKind::Internal,
		moq_net::StreamError::Cancel => MoqProtocolKind::Cancel,
		moq_net::StreamError::DeliveryTimeout => MoqProtocolKind::DeliveryTimeout,
		moq_net::StreamError::GoingAway => MoqProtocolKind::GoingAway,
		moq_net::StreamError::TooFarBehind => MoqProtocolKind::TooFarBehind,
		moq_net::StreamError::MalformedTrack => MoqProtocolKind::MalformedTrack,
		// A datagram reached by a FETCH is, to a binding, a miss like any other.
		moq_net::StreamError::NotFound | moq_net::StreamError::NotFetchable => MoqProtocolKind::NotFound,
		moq_net::StreamError::Unroutable => MoqProtocolKind::Unroutable,
		moq_net::StreamError::Old => MoqProtocolKind::Old,
		moq_net::StreamError::Evicted => MoqProtocolKind::Evicted,
		moq_net::StreamError::WrongSize => MoqProtocolKind::WrongSize,
		moq_net::StreamError::FrameTooLarge => MoqProtocolKind::FrameTooLarge,
		moq_net::StreamError::TimestampMismatch => MoqProtocolKind::TimestampMismatch,
		moq_net::StreamError::Unauthorized => MoqProtocolKind::Unauthorized,
		moq_net::StreamError::App(_) => MoqProtocolKind::App,
		moq_net::StreamError::Unknown(_) => MoqProtocolKind::Unknown,
		_ => MoqProtocolKind::Unknown,
	}
}

/// Error returned by all UniFFI-exported functions.
///
/// Exports `Display`, which the bindings surface as the error's string form.
/// Diagnostic payloads are named `reason`; structured protocol failures use `details`.
#[derive(Debug, Clone, thiserror::Error, uniffi::Error)]
#[uniffi::export(Display)]
#[non_exhaustive]
pub enum MoqError {
	/// A protocol failure carrying the peer's session or stream code.
	#[error("{details}")]
	Protocol {
		/// The protocol registry, wire code, and diagnostic message.
		details: MoqProtocolError,
	},

	/// The underlying QUIC/WebTransport connection failed.
	#[error("transport: {reason}")]
	Transport {
		/// A diagnostic message describing the failure.
		reason: String,
	},

	/// A local failure without a session or stream code.
	#[error("internal: {reason}")]
	Internal {
		/// A diagnostic message describing the failure.
		reason: String,
	},

	/// The media catalog or container could not be processed.
	#[error("{reason}")]
	Media {
		/// A diagnostic message describing the failure.
		reason: String,
	},

	/// Media could not be imported or exported.
	#[error("{reason}")]
	Mux {
		/// A diagnostic message describing the failure.
		reason: String,
	},

	/// A JSON track could not be processed.
	#[error("{reason}")]
	JsonTrack {
		/// A diagnostic message describing the failure.
		reason: String,
	},

	// Native codec errors, behind the optional `audio`/`video` features.
	/// An audio codec operation failed.
	#[cfg(feature = "audio")]
	#[error("{reason}")]
	Audio {
		/// A diagnostic message describing the failure.
		reason: String,
	},

	/// A video codec operation failed.
	#[cfg(feature = "video")]
	#[error("{reason}")]
	Video {
		/// A diagnostic message describing the failure.
		reason: String,
	},

	/// The connection URL could not be parsed.
	#[error("url: {reason}")]
	Url {
		/// A diagnostic message describing the failure.
		reason: String,
	},

	/// A timestamp is outside the supported range.
	#[error("timestamp overflow")]
	TimeOverflow,

	/// The requested log level is invalid.
	#[error("log level: {reason}")]
	LogLevel {
		/// A diagnostic message describing the failure.
		reason: String,
	},

	/// An owned native task panicked; cancellation is reported separately.
	#[error("task: {reason}")]
	Task {
		/// A diagnostic message describing the failure.
		reason: String,
	},

	/// JSON could not be parsed or encoded.
	#[error("json: {reason}")]
	Json {
		/// A diagnostic message describing the failure.
		reason: String,
	},

	/// The operation was cancelled.
	#[error("cancelled")]
	Cancelled,

	/// The handle or connection is closed.
	#[error("closed")]
	Closed,

	/// A configuration call lost the race with an in-flight async operation.
	///
	/// The handle is still live: wait for the operation, then try again. A
	/// cancelled handle is [`Self::Cancelled`] instead.
	#[error("busy")]
	Busy,

	/// A connection could not be established.
	#[error("connect: {reason}")]
	Connect {
		/// A diagnostic message describing the failure.
		reason: String,
	},

	/// The local server socket could not be bound.
	#[error("bind: {reason}")]
	Bind {
		/// A diagnostic message describing the failure.
		reason: String,
	},

	/// A connection was refused during the handshake.
	#[error("reject: {reason}")]
	Reject {
		/// A diagnostic message describing the failure.
		reason: String,
	},

	/// The request has already been accepted or rejected.
	#[error("already responded")]
	AlreadyResponded,

	/// Codec configuration or bitstream parsing failed.
	#[error("codec: {reason}")]
	Codec {
		/// A diagnostic message describing the failure.
		reason: String,
	},

	/// Authorization is required or invalid.
	#[error("unauthorized")]
	Unauthorized,

	/// Authorization does not permit the requested operation.
	#[error("forbidden")]
	Forbidden,

	/// The authorization token expired; refresh it before retrying.
	#[error("authorization token expired")]
	Expired,

	/// The requested track or group is not available.
	#[error("not found")]
	NotFound,

	/// The requested operation is not supported.
	///
	/// A statement about this build or this peer, not about the call: the feature is
	/// unavailable however the caller asks for it. Caller misuse gets its own error, so
	/// that a binding can tell "MoQ can't do this here" from "you held it wrong".
	#[error("unsupported")]
	Unsupported,

	/// This track or group already committed to another way of reading it.
	///
	/// A track is read in arrival order, in sequence order, or by a typed reader such as a
	/// JSON consumer, never two of them, and the first group read picks the order. A group is
	/// read raw or by a media group consumer, and the first frame read picks raw. Reaching
	/// for another one afterwards is this error rather than [`Self::Unsupported`]: each works
	/// fine here, the handle just isn't reading the way you asked for. Read through a second
	/// consumer if you genuinely need both.
	#[error("already committed to another way of reading")]
	AlreadyCommitted,

	/// A path pattern was empty, had a doubled slash, or used a reserved segment form.
	#[error("invalid pattern: {reason}")]
	InvalidPattern {
		/// A diagnostic message describing the failure.
		reason: String,
	},

	/// An epoch was not a lowercase hyphenated UUIDv7.
	#[error("{reason}")]
	InvalidEpoch {
		/// A diagnostic message describing the failure.
		reason: String,
	},

	/// A catalog rendition named another broadcast, but this consumer came from a standalone
	/// broadcast rather than an origin, so there is nothing to resolve the reference against.
	#[error("unresolvable broadcast reference: {reason}")]
	UnresolvableBroadcast {
		/// A diagnostic message describing the failure.
		reason: String,
	},

	/// The logger could not be initialized.
	#[error("log: {reason}")]
	Log {
		/// A diagnostic message describing the failure.
		reason: String,
	},

	/// A configuration record held a value the constructor cannot use.
	#[error("config: {reason}")]
	Config {
		/// A diagnostic message describing the failure.
		reason: String,
	},

	/// A catalog needs a newer moq-hang draft than this build reads; upgrade to read it.
	#[error("unsupported catalog version {version}")]
	UnsupportedCatalog {
		/// The catalog version this build cannot read.
		version: u64,
	},
}

impl From<moq_net::Error> for MoqError {
	fn from(err: moq_net::Error) -> Self {
		match err {
			moq_net::Error::Transport(message) => Self::Transport { reason: message },
			moq_net::Error::IdleTimeout => Self::Transport {
				reason: "transport idle timeout".into(),
			},
			moq_net::Error::NotFound | moq_net::Error::NotFetchable => Self::NotFound,
			// A track whose own session closed aborts its open groups with this; a remote
			// SESSION_CLOSED decodes as `Internal`, so only the local close lands here.
			moq_net::Error::Closed
			| moq_net::Error::GoingAway
			| moq_net::Error::Stream(moq_net::StreamError::Session(moq_net::SessionError::Cancel)) => Self::Closed,
			moq_net::Error::Cancel => Self::Cancelled,
			moq_net::Error::Unauthorized => Self::Unauthorized,
			moq_net::Error::Expired => Self::Expired,
			moq_net::Error::Unsupported | moq_net::Error::Version => Self::Unsupported,
			moq_net::Error::Session(err) => Self::Protocol {
				details: MoqProtocolError::from_session(&err),
			},
			moq_net::Error::Stream(err) => Self::Protocol {
				details: MoqProtocolError::from_stream(&err),
			},
			// Local abort/cancel uses `App` without a registry; abort APIs are stream-scoped.
			moq_net::Error::App(app) => Self::Protocol {
				details: MoqProtocolError::from_stream(&moq_net::StreamError::App(app)),
			},
			other => {
				// Local stream conditions have a defined outgoing code. A failure without
				// one stays local instead of inventing an INTERNAL_ERROR from either registry.
				let stream = moq_net::StreamError::from(&other);
				if matches!(
					stream,
					moq_net::StreamError::Internal | moq_net::StreamError::Session(_)
				) {
					Self::Internal {
						reason: other.to_string(),
					}
				} else {
					Self::Protocol {
						details: MoqProtocolError::from_stream(&stream),
					}
				}
			}
		}
	}
}

impl From<hang::Error> for MoqError {
	fn from(err: hang::Error) -> Self {
		match err {
			hang::Error::Moq(err) => err.into(),
			hang::Error::UnsupportedVersion(version) => Self::UnsupportedCatalog { version },
			err => Self::Media {
				reason: err.to_string(),
			},
		}
	}
}

impl From<moq_mux::Error> for MoqError {
	fn from(err: moq_mux::Error) -> Self {
		match err {
			moq_mux::Error::Moq(err) => err.into(),
			moq_mux::Error::Hang(err) => err.into(),
			moq_mux::Error::Json(err) => err.into(),
			err => std::iter::successors(Some(&err as &(dyn std::error::Error + 'static)), |err| err.source())
				.find_map(|err| err.downcast_ref::<moq_net::Error>())
				.map(|err| err.clone().into())
				.unwrap_or_else(|| Self::Mux {
					reason: err.to_string(),
				}),
		}
	}
}

impl From<moq_json::Error> for MoqError {
	fn from(err: moq_json::Error) -> Self {
		match err {
			moq_json::Error::Net(err) => err.into(),
			err => Self::JsonTrack {
				reason: err.to_string(),
			},
		}
	}
}

#[cfg(feature = "audio")]
impl From<moq_audio::Error> for MoqError {
	fn from(err: moq_audio::Error) -> Self {
		Self::Audio {
			reason: err.to_string(),
		}
	}
}

#[cfg(feature = "video")]
impl From<moq_video::Error> for MoqError {
	fn from(err: moq_video::Error) -> Self {
		Self::Video {
			reason: err.to_string(),
		}
	}
}

impl From<moq_net::TimeOverflow> for MoqError {
	fn from(_: moq_net::TimeOverflow) -> Self {
		Self::TimeOverflow
	}
}

// Dependency errors are flattened to their message so their crates stay out of this crate's
// public API.
macro_rules! from_message {
	($($ty:ty => $variant:ident),* $(,)?) => {
		$(
			impl From<$ty> for MoqError {
				fn from(err: $ty) -> Self {
					Self::$variant { reason: err.to_string() }
				}
			}
		)*
	};
}

from_message! {
	url::ParseError => Url,
	tracing::metadata::ParseLevelError => LogLevel,
	serde_json::Error => Json,
	moq_net::InvalidPattern => InvalidPattern,
	moq_net::InvalidEpoch => InvalidEpoch,
}

from_message! {
	tokio::task::JoinError => Task,
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn named_session_codes_keep_a_known_kind() {
		for (code, kind) in [
			(0x0, MoqProtocolKind::Cancel),
			(0x1, MoqProtocolKind::Internal),
			(0x2, MoqProtocolKind::Unauthorized),
			(0x3, MoqProtocolKind::ProtocolViolation),
			(0x4, MoqProtocolKind::InvalidRequestId),
			(0x6, MoqProtocolKind::KeyValueFormatting),
			(0x7, MoqProtocolKind::TooManyRequests),
			(0x8, MoqProtocolKind::InvalidPath),
			(0x10, MoqProtocolKind::GoawayTimeout),
			(0x11, MoqProtocolKind::Timeout),
			(0x13, MoqProtocolKind::AuthTokenCacheOverflow),
			(0x15, MoqProtocolKind::Version),
			(0x16, MoqProtocolKind::MalformedAuthToken),
			(0x17, MoqProtocolKind::UnknownAuthTokenAlias),
			(0x18, MoqProtocolKind::Expired),
			(0x19, MoqProtocolKind::InvalidAuthority),
			(0x1b, MoqProtocolKind::TooManyRequestUpdates),
		] {
			let error = MoqProtocolError::from_session(&moq_net::SessionError::from_code(code));
			assert_eq!(error.scope, MoqErrorScope::Session);
			assert_eq!(error.code, code);
			assert_eq!(error.kind, kind, "session code {code:#x}");
		}
	}

	#[test]
	fn session_limit_has_a_known_kind() {
		let error = super::MoqProtocolError::from_session(&moq_net::SessionError::TooManyRequests);
		assert_eq!(error.scope, super::MoqErrorScope::Session);
		assert_eq!(error.code, moq_net::SessionError::TooManyRequests.to_code());
		assert_eq!(error.kind, super::MoqProtocolKind::TooManyRequests);
	}

	#[test]
	fn unsupported_catalog_survives_the_mux() {
		let error = moq_mux::Error::Hang(hang::Error::UnsupportedVersion(5));
		assert!(matches!(
			MoqError::from(error),
			MoqError::UnsupportedCatalog { version: 5 }
		));
	}

	#[test]
	fn protocol_codes_survive_container_errors() {
		let error = moq_mux::Error::Cmaf(moq_mux::container::fmp4::Error::Moq(
			moq_net::StreamError::Unknown(31).into(),
		));
		let MoqError::Protocol { details } = MoqError::from(error) else {
			panic!("container error lost the peer's code");
		};
		assert_eq!(details.scope, MoqErrorScope::Stream);
		assert_eq!(details.code, 31);
		assert_eq!(details.kind, MoqProtocolKind::Unknown);
	}

	#[test]
	fn received_known_codes_keep_their_registry() {
		for code in [0, 1, 2, 3, 6, 0x10, 0x11, 0x15, 0x20, 468, u32::MAX] {
			let MoqError::Protocol { details } =
				MoqError::from(moq_net::Error::from(moq_net::SessionError::from_code(code)))
			else {
				panic!("lost session code {code}")
			};
			assert_eq!(details.scope, MoqErrorScope::Session);
			assert_eq!(details.code, code);
		}
		for code in [0, 1, 2, 3, 4, 5, 0x12, 0x20, 468, u32::MAX] {
			let MoqError::Protocol { details } =
				MoqError::from(moq_net::Error::from(moq_net::StreamError::from_code(code)))
			else {
				panic!("lost stream code {code}")
			};
			assert_eq!(details.scope, MoqErrorScope::Stream);
			assert_eq!(details.code, code);
		}
	}

	#[test]
	fn session_known_code_keeps_scope_and_code() {
		let err = MoqError::from(moq_net::Error::from(moq_net::SessionError::Unauthorized));
		match err {
			MoqError::Protocol { details: protocol } => {
				assert_eq!(protocol.scope, MoqErrorScope::Session);
				assert_eq!(protocol.code, 0x2);
				assert_eq!(protocol.kind, MoqProtocolKind::Unauthorized);
			}
			other => panic!("expected Protocol, got {other}"),
		}
	}

	/// A refused request carries no session code, so it must not claim the session expired.
	#[test]
	fn expired_request_is_not_a_session_close() {
		assert!(matches!(MoqError::from(moq_net::Error::Expired), MoqError::Expired));

		let err = MoqError::from(moq_net::Error::from(moq_net::SessionError::Expired));
		match err {
			MoqError::Protocol { details } => {
				assert_eq!(details.scope, MoqErrorScope::Session);
				assert_eq!(details.code, 0x18);
				assert_eq!(details.kind, MoqProtocolKind::Expired);
			}
			other => panic!("expected Protocol, got {other}"),
		}
	}

	#[test]
	fn session_app_code_is_verbatim() {
		let err = MoqError::from(moq_net::Error::from(moq_net::SessionError::App(404)));
		match err {
			MoqError::Protocol { details: protocol } => {
				assert_eq!(protocol.scope, MoqErrorScope::Session);
				assert_eq!(protocol.code, 64 + 404);
				assert_eq!(protocol.kind, MoqProtocolKind::App);
			}
			other => panic!("expected Protocol, got {other}"),
		}
	}

	#[test]
	fn session_unknown_code_is_verbatim() {
		let err = MoqError::from(moq_net::Error::from(moq_net::SessionError::Unknown(0x1f)));
		match err {
			MoqError::Protocol { details: protocol } => {
				assert_eq!(protocol.scope, MoqErrorScope::Session);
				assert_eq!(protocol.code, 0x1f);
				assert_eq!(protocol.kind, MoqProtocolKind::Unknown);
			}
			other => panic!("expected Protocol, got {other}"),
		}
	}

	#[test]
	fn stream_unauthorized_keeps_its_kind() {
		let err = MoqError::from(moq_net::Error::from(moq_net::StreamError::Unauthorized));
		match err {
			MoqError::Protocol { details: protocol } => {
				assert_eq!(protocol.scope, MoqErrorScope::Stream);
				assert_eq!(protocol.code, 0x3b);
				assert_eq!(protocol.kind, MoqProtocolKind::Unauthorized);
			}
			other => panic!("expected Protocol, got {other}"),
		}
	}

	#[test]
	fn stream_app_code_is_verbatim() {
		let err = MoqError::from(moq_net::Error::from(moq_net::StreamError::App(7)));
		match err {
			MoqError::Protocol { details: protocol } => {
				assert_eq!(protocol.scope, MoqErrorScope::Stream);
				assert_eq!(protocol.code, 64 + 7);
				assert_eq!(protocol.kind, MoqProtocolKind::App);
			}
			other => panic!("expected Protocol, got {other}"),
		}
	}

	#[test]
	fn reserved_range_received_stays_unknown() {
		// 0x20 sits in the draft's reserved range, so it is never given a kind.
		let err = MoqError::from(moq_net::Error::from(moq_net::SessionError::from_code(0x20)));
		match err {
			MoqError::Protocol { details: protocol } => {
				assert_eq!(protocol.code, 0x20);
				assert_eq!(protocol.kind, MoqProtocolKind::Unknown);
			}
			other => panic!("expected Protocol, got {other}"),
		}
	}

	/// A group cut by its own session closing is a shutdown; one reset by a peer is not.
	#[test]
	fn local_session_close_is_closed() {
		let local = moq_net::StreamError::Session(moq_net::SessionError::Cancel);
		assert!(matches!(MoqError::from(moq_net::Error::from(local)), MoqError::Closed));

		let received = moq_net::StreamError::from_code(0x3);
		let MoqError::Protocol { details } = MoqError::from(moq_net::Error::from(received)) else {
			panic!("a peer's SESSION_CLOSED lost its code");
		};
		assert_eq!(details.kind, MoqProtocolKind::SessionClosed);
	}

	#[test]
	fn transport_stays_out_of_protocol() {
		let err = MoqError::from(moq_net::Error::Transport("dial failed".into()));
		assert!(matches!(err, MoqError::Transport { .. }));
	}

	#[test]
	fn local_app_abort_is_a_stream_protocol_error() {
		let err = MoqError::from(moq_net::Error::App(404));
		match err {
			MoqError::Protocol { details: protocol } => {
				assert_eq!(protocol.scope, MoqErrorScope::Stream);
				assert_eq!(protocol.code, 64 + 404);
				assert_eq!(protocol.kind, MoqProtocolKind::App);
			}
			other => panic!("expected Protocol, got {other}"),
		}
	}
}
