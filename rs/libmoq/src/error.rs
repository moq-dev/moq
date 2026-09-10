use std::sync::Arc;

use crate::ffi;

/// Whether a protocol code is from the session or stream registry.
#[repr(C)]
#[allow(non_camel_case_types)]
#[derive(Clone, Copy, Debug)]
pub enum moq_error_scope {
	/// A session close code.
	MOQ_ERROR_SCOPE_SESSION = 0,
	/// A stream reset or stop code.
	MOQ_ERROR_SCOPE_STREAM = 1,
}

/// A recognized protocol kind. Pair with [`moq_error_scope`]: `CANCEL` is 0 on a session
/// and 1 on a stream. `APP` and `UNKNOWN` keep the numeric code in [`moq_protocol_error`].
#[repr(C)]
#[allow(non_camel_case_types)]
#[derive(Clone, Copy, Debug)]
pub enum moq_protocol_kind {
	/// Cancel.
	MOQ_PROTOCOL_KIND_CANCEL = 0,
	/// Internal.
	MOQ_PROTOCOL_KIND_INTERNAL = 1,
	/// Unauthorized.
	MOQ_PROTOCOL_KIND_UNAUTHORIZED = 2,
	/// Protocol violation.
	MOQ_PROTOCOL_KIND_PROTOCOL_VIOLATION = 3,
	/// Key value formatting.
	MOQ_PROTOCOL_KIND_KEY_VALUE_FORMATTING = 4,
	/// Goaway timeout.
	MOQ_PROTOCOL_KIND_GOAWAY_TIMEOUT = 5,
	/// Timeout.
	MOQ_PROTOCOL_KIND_TIMEOUT = 6,
	/// Version.
	MOQ_PROTOCOL_KIND_VERSION = 7,
	/// Required extension.
	MOQ_PROTOCOL_KIND_REQUIRED_EXTENSION = 8,
	/// Invalid role.
	MOQ_PROTOCOL_KIND_INVALID_ROLE = 9,
	/// Unexpected stream.
	MOQ_PROTOCOL_KIND_UNEXPECTED_STREAM = 10,
	/// Delivery timeout.
	MOQ_PROTOCOL_KIND_DELIVERY_TIMEOUT = 11,
	/// Session closed.
	MOQ_PROTOCOL_KIND_SESSION_CLOSED = 12,
	/// Going away.
	MOQ_PROTOCOL_KIND_GOING_AWAY = 13,
	/// Too far behind.
	MOQ_PROTOCOL_KIND_TOO_FAR_BEHIND = 14,
	/// Malformed track.
	MOQ_PROTOCOL_KIND_MALFORMED_TRACK = 15,
	/// Not found.
	MOQ_PROTOCOL_KIND_NOT_FOUND = 16,
	/// Unroutable.
	MOQ_PROTOCOL_KIND_UNROUTABLE = 17,
	/// Old.
	MOQ_PROTOCOL_KIND_OLD = 18,
	/// Evicted.
	MOQ_PROTOCOL_KIND_EVICTED = 19,
	/// Wrong size.
	MOQ_PROTOCOL_KIND_WRONG_SIZE = 20,
	/// Frame too large.
	MOQ_PROTOCOL_KIND_FRAME_TOO_LARGE = 21,
	/// Timestamp mismatch.
	MOQ_PROTOCOL_KIND_TIMESTAMP_MISMATCH = 22,
	/// App.
	MOQ_PROTOCOL_KIND_APP = 23,
	/// Unknown.
	MOQ_PROTOCOL_KIND_UNKNOWN = 24,
}

/// A protocol failure a peer sent: scope, verbatim wire code, and recognized kind.
///
/// Filled by [`crate::moq_error_protocol`] after a call returned a negative code. Do not parse
/// [`crate::moq_error`] for this; that string is diagnostics only.
#[repr(C)]
#[allow(non_camel_case_types)]
#[derive(Clone, Copy, Debug)]
pub struct moq_protocol_error {
	/// [`moq_error_scope`] discriminant.
	pub scope: u32,
	/// The integer on the wire, kept verbatim.
	pub code: u32,
	/// [`moq_protocol_kind`] discriminant.
	pub kind: u32,
}

/// Status code returned by FFI functions.
///
/// Negative values indicate errors, zero indicates success,
/// and positive values are valid resource handles.
pub type Status = i32;

/// Error types that can occur in the FFI layer.
///
/// Each error variant maps to a specific negative error code
/// returned to C callers.
#[derive(Debug, thiserror::Error, Clone)]
#[non_exhaustive]
pub enum Error {
	/// Error from the underlying MoQ protocol layer.
	#[error("moq error: {0}")]
	Moq(#[from] moq_net::Error),

	/// Error from the native helper layer (moq-tokio).
	#[error("native error: {0}")]
	Native(#[from] moq_tokio::Error),

	/// URL parsing error.
	#[error("url error: {0}")]
	Url(String),

	/// UTF-8 string validation error.
	#[error("utf8 error: {0}")]
	Utf8(#[from] std::str::Utf8Error),

	/// Connection establishment error.
	#[error("connect error: {0}")]
	Connect(Arc<anyhow::Error>),

	/// Null or invalid pointer passed from C.
	#[error("invalid pointer")]
	InvalidPointer,

	/// Invalid resource ID.
	#[error("invalid id")]
	InvalidId,

	/// Resource not found.
	#[error("not found")]
	NotFound,

	/// Session task not found.
	#[error("session not found")]
	SessionNotFound,

	/// Origin producer not found.
	#[error("origin not found")]
	OriginNotFound,

	/// Announcement not found.
	#[error("announcement not found")]
	AnnouncementNotFound,

	/// Broadcast not found.
	#[error("broadcast not found")]
	BroadcastNotFound,

	/// Catalog not found.
	#[error("catalog not found")]
	CatalogNotFound,

	/// Media decoder not found.
	#[error("media not found")]
	MediaNotFound,

	/// Track task not found.
	#[error("track not found")]
	TrackNotFound,

	/// Group producer not found.
	#[error("group not found")]
	GroupNotFound,

	/// Frame not found.
	#[error("frame not found")]
	FrameNotFound,

	/// Unknown media format specified.
	#[error("unknown format: {0}")]
	UnknownFormat(String),

	/// Initialization failed (e.g. logging setup).
	#[error("init failed: {0}")]
	InitFailed(Arc<anyhow::Error>),

	/// Buffer was not fully consumed.
	#[error("buffer was not fully consumed")]
	BufferNotConsumed,

	/// Timestamp value overflow.
	#[error("timestamp overflow")]
	TimestampOverflow(#[from] moq_net::TimeOverflow),

	/// Log level parsing error.
	#[error("level error: {0}")]
	Level(String),

	/// Invalid error code conversion.
	#[error("invalid code")]
	InvalidCode,

	/// Panic occurred in Rust code.
	#[error("panic")]
	Panic,

	/// Session is offline.
	#[error("offline")]
	Offline,

	/// Connection was rejected as unauthorized by the server.
	#[error("unauthorized")]
	Unauthorized,

	/// Connection was forbidden by the server.
	#[error("forbidden")]
	Forbidden,

	/// Error from the hang media layer.
	#[error("hang error: {0}")]
	Hang(#[from] hang::Error),

	/// Error from the moq-mux consumer layer.
	#[error("mux error: {0}")]
	Mux(#[from] moq_mux::Error),

	/// Index out of bounds.
	#[error("no index")]
	NoIndex,

	/// Null byte found in C string.
	#[error("nul error")]
	NulError(#[from] std::ffi::NulError),

	/// Error from the moq-audio codec layer.
	#[error("audio error: {0}")]
	Audio(Arc<moq_audio::Error>),

	/// Error from the moq-video codec layer.
	#[error("video error: {0}")]
	Video(Arc<moq_video::Error>),

	/// Invalid JSON passed for a catalog section.
	#[error("json error: {0}")]
	Json(String),

	/// Error from the moq-json snapshot/stream layer.
	#[error("json track error: {0}")]
	JsonTrack(Arc<moq_json::Error>),

	/// A client configuration value could not be parsed or initialized.
	#[error("invalid config: {0}")]
	InvalidConfig(String),

	/// A catalog rendition named another broadcast, but the broadcast it came from was not
	/// resolved through an origin, so there is nothing to resolve the reference against.
	#[error("unresolvable broadcast reference: {0}")]
	UnresolvableBroadcast(String),
}

impl From<moq_json::Error> for Error {
	fn from(err: moq_json::Error) -> Self {
		match err {
			moq_json::Error::Net(e) => Error::Moq(e),
			e => Error::JsonTrack(Arc::new(e)),
		}
	}
}

// Dependency errors are flattened to their message so their crates stay out of this crate's
// public API.
impl From<serde_json::Error> for Error {
	fn from(err: serde_json::Error) -> Self {
		Error::Json(err.to_string())
	}
}

impl From<url::ParseError> for Error {
	fn from(err: url::ParseError) -> Self {
		Error::Url(err.to_string())
	}
}

impl From<moq_audio::Error> for Error {
	fn from(err: moq_audio::Error) -> Self {
		Error::Audio(Arc::new(err))
	}
}

impl From<moq_video::Error> for Error {
	fn from(err: moq_video::Error) -> Self {
		Error::Video(Arc::new(err))
	}
}

impl From<tracing::metadata::ParseLevelError> for Error {
	fn from(err: tracing::metadata::ParseLevelError) -> Self {
		Error::Level(err.to_string())
	}
}

impl Error {
	/// Structured protocol details when this is a session or stream code, not a local failure.
	pub(crate) fn protocol(&self) -> Option<moq_protocol_error> {
		std::iter::successors(Some(self as &(dyn std::error::Error + 'static)), |err| err.source())
			.find_map(|err| err.downcast_ref::<moq_net::Error>().and_then(protocol_of_net))
	}
}

fn protocol_of_net(err: &moq_net::Error) -> Option<moq_protocol_error> {
	match err {
		moq_net::Error::Transport(_) => None,
		moq_net::Error::Session(err) => Some(from_session(err)),
		moq_net::Error::Stream(err) => Some(from_stream(err)),
		moq_net::Error::App(app) => Some(from_stream(&moq_net::StreamError::App(*app))),
		_ => None,
	}
}

fn from_session(err: &moq_net::SessionError) -> moq_protocol_error {
	moq_protocol_error {
		scope: moq_error_scope::MOQ_ERROR_SCOPE_SESSION as u32,
		code: err.to_code(),
		kind: session_kind(err) as u32,
	}
}

fn from_stream(err: &moq_net::StreamError) -> moq_protocol_error {
	moq_protocol_error {
		scope: moq_error_scope::MOQ_ERROR_SCOPE_STREAM as u32,
		code: err.to_code(),
		kind: stream_kind(err) as u32,
	}
}

fn session_kind(err: &moq_net::SessionError) -> moq_protocol_kind {
	use moq_protocol_kind::*;
	match err {
		moq_net::SessionError::Cancel => MOQ_PROTOCOL_KIND_CANCEL,
		moq_net::SessionError::Internal => MOQ_PROTOCOL_KIND_INTERNAL,
		moq_net::SessionError::Unauthorized => MOQ_PROTOCOL_KIND_UNAUTHORIZED,
		moq_net::SessionError::ProtocolViolation => MOQ_PROTOCOL_KIND_PROTOCOL_VIOLATION,
		moq_net::SessionError::KeyValueFormatting => MOQ_PROTOCOL_KIND_KEY_VALUE_FORMATTING,
		moq_net::SessionError::GoawayTimeout => MOQ_PROTOCOL_KIND_GOAWAY_TIMEOUT,
		moq_net::SessionError::Timeout => MOQ_PROTOCOL_KIND_TIMEOUT,
		moq_net::SessionError::Version => MOQ_PROTOCOL_KIND_VERSION,
		moq_net::SessionError::RequiredExtension => MOQ_PROTOCOL_KIND_REQUIRED_EXTENSION,
		moq_net::SessionError::InvalidRole => MOQ_PROTOCOL_KIND_INVALID_ROLE,
		moq_net::SessionError::UnexpectedStream => MOQ_PROTOCOL_KIND_UNEXPECTED_STREAM,
		moq_net::SessionError::App(_) => MOQ_PROTOCOL_KIND_APP,
		moq_net::SessionError::Unknown(_) => MOQ_PROTOCOL_KIND_UNKNOWN,
		_ => MOQ_PROTOCOL_KIND_UNKNOWN,
	}
}

fn stream_kind(err: &moq_net::StreamError) -> moq_protocol_kind {
	use moq_protocol_kind::*;
	match err {
		moq_net::StreamError::Session(_) => MOQ_PROTOCOL_KIND_SESSION_CLOSED,
		moq_net::StreamError::Internal => MOQ_PROTOCOL_KIND_INTERNAL,
		moq_net::StreamError::Cancel => MOQ_PROTOCOL_KIND_CANCEL,
		moq_net::StreamError::DeliveryTimeout => MOQ_PROTOCOL_KIND_DELIVERY_TIMEOUT,
		moq_net::StreamError::GoingAway => MOQ_PROTOCOL_KIND_GOING_AWAY,
		moq_net::StreamError::TooFarBehind => MOQ_PROTOCOL_KIND_TOO_FAR_BEHIND,
		moq_net::StreamError::MalformedTrack => MOQ_PROTOCOL_KIND_MALFORMED_TRACK,
		moq_net::StreamError::NotFound => MOQ_PROTOCOL_KIND_NOT_FOUND,
		moq_net::StreamError::Unroutable => MOQ_PROTOCOL_KIND_UNROUTABLE,
		moq_net::StreamError::Old => MOQ_PROTOCOL_KIND_OLD,
		moq_net::StreamError::Evicted => MOQ_PROTOCOL_KIND_EVICTED,
		moq_net::StreamError::WrongSize => MOQ_PROTOCOL_KIND_WRONG_SIZE,
		moq_net::StreamError::FrameTooLarge => MOQ_PROTOCOL_KIND_FRAME_TOO_LARGE,
		moq_net::StreamError::TimestampMismatch => MOQ_PROTOCOL_KIND_TIMESTAMP_MISMATCH,
		moq_net::StreamError::App(_) => MOQ_PROTOCOL_KIND_APP,
		moq_net::StreamError::Unknown(_) => MOQ_PROTOCOL_KIND_UNKNOWN,
		_ => MOQ_PROTOCOL_KIND_UNKNOWN,
	}
}

impl ffi::ReturnCode for Error {
	fn error(&self) -> Option<&Error> {
		Some(self)
	}

	fn code(&self) -> i32 {
		match self {
			Error::Moq(_) => -2,
			Error::Url(_) => -3,
			Error::Utf8(_) => -4,
			Error::Connect(_) => -5,
			Error::InvalidPointer => -6,
			Error::InvalidId => -7,
			Error::NotFound => -8,
			Error::UnknownFormat(_) => -9,
			Error::InitFailed(_) => -10,
			Error::TimestampOverflow(_) => -13,
			Error::Level(_) => -14,
			Error::InvalidCode => -15,
			Error::Panic => -16,
			Error::Offline => -17,
			Error::Hang(_) => -18,
			Error::NoIndex => -19,
			Error::NulError(_) => -20,
			Error::SessionNotFound => -21,
			Error::OriginNotFound => -22,
			Error::AnnouncementNotFound => -23,
			Error::BroadcastNotFound => -24,
			Error::CatalogNotFound => -25,
			Error::MediaNotFound => -26,
			Error::TrackNotFound => -27,
			Error::FrameNotFound => -28,
			Error::Mux(_) => -29,
			Error::Audio(_) => -30,
			Error::BufferNotConsumed => -31,
			Error::GroupNotFound => -32,
			Error::Native(_) => -33,
			Error::Unauthorized => -34,
			Error::Forbidden => -35,
			Error::Video(_) => -36,
			Error::Json(_) => -37,
			Error::JsonTrack(_) => -38,
			Error::InvalidConfig(_) => -40,
			Error::UnresolvableBroadcast(_) => -41,
		}
	}
}
