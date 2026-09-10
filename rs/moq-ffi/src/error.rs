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
	/// The peer's token does not grant the requested path or operation.
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
	/// A required extension was not offered by the peer.
	RequiredExtension,
	/// The peer acted against the role it advertised at SETUP.
	InvalidRole,
	/// A stream was opened with an unknown or disallowed type.
	UnexpectedStream,
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
}

/// A protocol failure a peer sent (or this side will send): scope, verbatim code, kind,
/// and a diagnostic message.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct MoqProtocolError {
	/// Whether this code is from the session or stream registry.
	pub scope: MoqErrorScope,
	/// The integer on the wire, kept verbatim. Do not re-derive this from [`Self::kind`]:
	/// `to_code` is not injective for the reserved 32-63 range.
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
		moq_net::SessionError::ProtocolViolation => MoqProtocolKind::ProtocolViolation,
		moq_net::SessionError::KeyValueFormatting => MoqProtocolKind::KeyValueFormatting,
		moq_net::SessionError::GoawayTimeout => MoqProtocolKind::GoawayTimeout,
		moq_net::SessionError::Timeout => MoqProtocolKind::Timeout,
		moq_net::SessionError::Version => MoqProtocolKind::Version,
		moq_net::SessionError::RequiredExtension => MoqProtocolKind::RequiredExtension,
		moq_net::SessionError::InvalidRole => MoqProtocolKind::InvalidRole,
		moq_net::SessionError::UnexpectedStream => MoqProtocolKind::UnexpectedStream,
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
		moq_net::StreamError::NotFound => MoqProtocolKind::NotFound,
		moq_net::StreamError::Unroutable => MoqProtocolKind::Unroutable,
		moq_net::StreamError::Old => MoqProtocolKind::Old,
		moq_net::StreamError::Evicted => MoqProtocolKind::Evicted,
		moq_net::StreamError::WrongSize => MoqProtocolKind::WrongSize,
		moq_net::StreamError::FrameTooLarge => MoqProtocolKind::FrameTooLarge,
		moq_net::StreamError::TimestampMismatch => MoqProtocolKind::TimestampMismatch,
		moq_net::StreamError::App(_) => MoqProtocolKind::App,
		moq_net::StreamError::Unknown(_) => MoqProtocolKind::Unknown,
		_ => MoqProtocolKind::Unknown,
	}
}

/// Error returned by all UniFFI-exported functions.
#[derive(Debug, thiserror::Error, uniffi::Error)]
#[non_exhaustive]
pub enum MoqError {
	/// A protocol failure carrying the peer's session or stream code.
	#[error("{details}")]
	Protocol { details: MoqProtocolError },

	/// The underlying QUIC/WebTransport connection failed.
	#[error("transport: {0}")]
	Transport(String),

	/// A local failure without a session or stream code.
	#[error("internal: {0}")]
	Internal(String),

	#[error("{0}")]
	Media(String),

	#[error("{0}")]
	Mux(String),

	#[error("{0}")]
	JsonTrack(String),

	// Native codec errors, behind the optional `audio`/`video` features.
	#[cfg(all(feature = "audio", not(target_arch = "wasm32")))]
	#[error("{0}")]
	Audio(String),

	#[cfg(all(feature = "video", not(target_arch = "wasm32")))]
	#[error("{0}")]
	Video(String),

	#[error("url: {0}")]
	Url(String),

	#[error("timestamp overflow")]
	TimeOverflow,

	#[error("log level: {0}")]
	LogLevel(String),

	// Only the native path spawns onto a runtime, so only it can fail to join.
	#[cfg(not(target_arch = "wasm32"))]
	#[error("task: {0}")]
	Task(String),

	#[error("json: {0}")]
	Json(String),

	#[error("cancelled")]
	Cancelled,

	#[error("closed")]
	Closed,

	#[error("connect: {0}")]
	Connect(String),

	#[error("bind: {0}")]
	Bind(String),

	#[error("reject: {0}")]
	Reject(String),

	#[error("already responded")]
	AlreadyResponded,

	#[error("codec: {0}")]
	Codec(String),

	#[error("unauthorized")]
	Unauthorized,

	#[error("forbidden")]
	Forbidden,

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

	/// This track already committed to the other delivery order.
	///
	/// A track is read in arrival order or in sequence order, never both, and the first
	/// group read picks which. Reaching for the other one afterwards is this error rather
	/// than [`Self::Unsupported`]: both orders work fine here, the track just isn't
	/// reading in the one you asked for. Read the track through a second consumer if you
	/// genuinely need both.
	#[error("already committed to the other delivery order")]
	AlreadyCommitted,

	/// A route carried an invalid hop id or too many hops.
	#[error("invalid route: {0}")]
	InvalidRoute(String),

	/// A path pattern was empty, had a doubled slash, or used a reserved segment form.
	#[error("invalid pattern: {0}")]
	InvalidPattern(String),

	/// A catalog rendition named another broadcast, but this consumer came from a standalone
	/// broadcast rather than an origin, so there is nothing to resolve the reference against.
	#[error("unresolvable broadcast reference: {0}")]
	UnresolvableBroadcast(String),

	#[error("log: {0}")]
	Log(String),
}

impl From<moq_net::Error> for MoqError {
	fn from(err: moq_net::Error) -> Self {
		match err {
			moq_net::Error::Transport(message) => Self::Transport(message),
			moq_net::Error::NotFound => Self::NotFound,
			moq_net::Error::Closed | moq_net::Error::GoingAway | moq_net::Error::SessionClosed => Self::Closed,
			moq_net::Error::Cancel => Self::Cancelled,
			moq_net::Error::Unauthorized => Self::Unauthorized,
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
					Self::Internal(other.to_string())
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
			err => Self::Media(err.to_string()),
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
				.unwrap_or_else(|| Self::Mux(err.to_string())),
		}
	}
}

impl From<moq_json::Error> for MoqError {
	fn from(err: moq_json::Error) -> Self {
		match err {
			moq_json::Error::Net(err) => err.into(),
			err => Self::JsonTrack(err.to_string()),
		}
	}
}

#[cfg(all(feature = "audio", not(target_arch = "wasm32")))]
impl From<moq_audio::Error> for MoqError {
	fn from(err: moq_audio::Error) -> Self {
		Self::Audio(err.to_string())
	}
}

#[cfg(all(feature = "video", not(target_arch = "wasm32")))]
impl From<moq_video::Error> for MoqError {
	fn from(err: moq_video::Error) -> Self {
		Self::Video(err.to_string())
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
					Self::$variant(err.to_string())
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
}

#[cfg(not(target_arch = "wasm32"))]
from_message! {
	tokio::task::JoinError => Task,
}

#[cfg(test)]
mod tests {
	use super::*;

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
		// 0x20 is what we send for RequiredExtension, but a received 0x20 is not read back
		// as that placeholder.
		let err = MoqError::from(moq_net::Error::from(moq_net::SessionError::from_code(0x20)));
		match err {
			MoqError::Protocol { details: protocol } => {
				assert_eq!(protocol.code, 0x20);
				assert_eq!(protocol.kind, MoqProtocolKind::Unknown);
			}
			other => panic!("expected Protocol, got {other}"),
		}
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
