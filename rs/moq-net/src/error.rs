use crate::coding;

/// A code sent when terminating the session.
///
/// One of the two wire registries specified by moq-lite, which reuse moq-transport's codes
/// unchanged; 64+ are the application's. The stream registry is [`StreamError`] and the two
/// are disjoint, so the same integer means different things in each.
///
/// Every variant is a registered code, so the registry round-trips: 32 through 47 is
/// reserved, nothing is sent there, and a received one stays [`Unknown`](Self::Unknown).
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SessionError {
	/// Ending the session normally, with no error.
	#[error("no error")]
	Cancel,

	/// Something went wrong that isn't worth a dedicated code.
	#[error("internal error")]
	Internal,

	/// The peer's token does not grant the requested path or operation. Retrying with the
	/// same credentials will fail again.
	#[error("unauthorized")]
	Unauthorized,

	/// The peer broke a protocol rule; the session is unusable.
	#[error("protocol violation")]
	ProtocolViolation,

	/// A key-value pair was malformed or repeated more than allowed.
	#[error("key-value formatting error")]
	KeyValueFormatting,

	/// The peer did not close within the GOAWAY drain deadline.
	#[error("goaway timeout")]
	GoawayTimeout,

	/// A control message took too long.
	#[error("control message timeout")]
	Timeout,

	/// No version could be negotiated.
	#[error("version negotiation failed")]
	Version,

	/// An application-chosen code, offset into the 64+ range on the wire.
	#[error("app code={0}")]
	App(u16),

	/// A code this version does not recognize, kept verbatim.
	#[error("unknown code={0}")]
	Unknown(u32),
}

impl SessionError {
	/// The integer sent on the wire.
	pub fn to_code(&self) -> u32 {
		match self {
			Self::Cancel => 0x0,
			Self::Internal => 0x1,
			Self::Unauthorized => 0x2,
			Self::ProtocolViolation => 0x3,
			Self::KeyValueFormatting => 0x6,
			Self::GoawayTimeout => 0x10,
			Self::Timeout => 0x11,
			Self::Version => 0x15,
			Self::App(app) => *app as u32 + 64,
			Self::Unknown(code) => *code,
		}
	}

	/// Decode a code received off the wire.
	///
	/// Unlike a raw peer code, the registered ones are specified, so decoding them is a
	/// wire contract rather than an assumption. Anything unregistered stays
	/// [`Self::Unknown`], including the reserved 32-47.
	pub fn from_code(code: u32) -> Self {
		match code {
			0x0 => Self::Cancel,
			0x1 => Self::Internal,
			0x2 => Self::Unauthorized,
			0x3 => Self::ProtocolViolation,
			0x6 => Self::KeyValueFormatting,
			0x10 => Self::GoawayTimeout,
			0x11 => Self::Timeout,
			0x15 => Self::Version,
			code @ 64.. => match u16::try_from(code - 64) {
				Ok(app) => Self::App(app),
				Err(_) => Self::Unknown(code),
			},
			code => Self::Unknown(code),
		}
	}
}

/// A code sent when resetting a stream, or refusing to receive one.
///
/// The counterpart to [`SessionError`], and a disjoint space: a stream reset of 0 is
/// [`Internal`](Self::Internal), not a cancellation ([`Cancel`](Self::Cancel) is 1).
///
/// Conditions the shared codes don't cover are assigned in moq-lite's own 48-63 range and
/// round-trip like any other registered code. 32 through 47 is reserved: nothing is sent
/// there and a received one stays [`Unknown`](Self::Unknown).
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum StreamError {
	/// The session ended, taking this stream with it.
	///
	/// The specific [`SessionError`] is local only: the two registries are disjoint, so
	/// this encodes to a single `SESSION_CLOSED` and the peer learns the reason from the
	/// session close itself.
	#[error("session closed: {0}")]
	Session(#[from] SessionError),

	/// Something went wrong that isn't worth a dedicated code.
	#[error("internal error")]
	Internal,

	/// The sender is done with this stream, not failing. A routine unsubscribe.
	#[error("cancelled")]
	Cancel,

	/// The content missed its delivery deadline.
	#[error("delivery timeout")]
	DeliveryTimeout,

	/// The session is going away (a GOAWAY was received).
	#[error("going away")]
	GoingAway,

	/// The reader fell too far behind and content was dropped to catch up.
	#[error("too far behind")]
	TooFarBehind,

	/// The track's content could not be parsed.
	#[error("malformed track")]
	MalformedTrack,

	/// The requested broadcast or track does not exist at the peer.
	#[error("not found")]
	NotFound,

	/// The broadcast is neither announced nor served, so there is no route to it.
	#[error("unroutable")]
	Unroutable,

	/// The group was superseded by a newer group and dropped.
	#[error("old")]
	Old,

	/// The group was dropped under memory pressure. Unlike [`Old`](Self::Old) it was still
	/// current, so it can be re-fetched.
	#[error("evicted")]
	Evicted,

	/// A frame's payload length disagreed with its declared size.
	#[error("wrong frame size")]
	WrongSize,

	/// A frame declared a payload larger than the receiver accepts.
	#[error("frame too large")]
	FrameTooLarge,

	/// A group grew past its cache budget and was aborted.
	#[error("group too large")]
	GroupTooLarge,

	/// A frame's timestamp doesn't match its track's negotiated timescale.
	#[error("frame timestamp doesn't match track timescale")]
	TimestampMismatch,

	/// An application-chosen code, offset into the 64+ range on the wire.
	#[error("app code={0}")]
	App(u16),

	/// A code this version does not recognize, kept verbatim.
	#[error("unknown code={0}")]
	Unknown(u32),
}

impl StreamError {
	/// The integer sent on the wire.
	pub fn to_code(&self) -> u32 {
		match self {
			Self::Internal => 0x0,
			Self::Cancel => 0x1,
			Self::DeliveryTimeout => 0x2,
			// Flattened: the session registry is disjoint, so the specific reason
			// doesn't fit here and travels on the session close instead.
			Self::Session(_) => 0x3,
			Self::GoingAway => 0x4,
			Self::TooFarBehind => 0x5,
			Self::MalformedTrack => 0x12,
			// 0x30 NO_CAPACITY, 0x31 CONTROL_TIMEOUT: assigned by other work in this
			// range. Do not reuse them.
			Self::GroupTooLarge => 0x32,
			Self::NotFound => 0x33,
			Self::Old => 0x34,
			Self::Evicted => 0x35,
			Self::Unroutable => 0x36,
			Self::WrongSize => 0x37,
			Self::FrameTooLarge => 0x38,
			Self::TimestampMismatch => 0x39,
			Self::App(app) => *app as u32 + 64,
			Self::Unknown(code) => *code,
		}
	}

	/// Decode a code received off the wire.
	///
	/// Only the registered codes decode; anything else, the reserved 32-47 range included,
	/// stays [`Unknown`](Self::Unknown) rather than being given a meaning the draft does
	/// not assign.
	///
	/// `SESSION_CLOSED` decodes to `Session(SessionError::Internal)`: the peer's actual
	/// session code is not on this stream, so the specific reason is unknown here.
	pub fn from_code(code: u32) -> Self {
		match code {
			0x0 => Self::Internal,
			0x1 => Self::Cancel,
			0x2 => Self::DeliveryTimeout,
			0x3 => Self::Session(SessionError::Internal),
			0x4 => Self::GoingAway,
			0x5 => Self::TooFarBehind,
			0x12 => Self::MalformedTrack,
			0x32 => Self::GroupTooLarge,
			0x33 => Self::NotFound,
			0x34 => Self::Old,
			0x35 => Self::Evicted,
			0x36 => Self::Unroutable,
			0x37 => Self::WrongSize,
			0x38 => Self::FrameTooLarge,
			0x39 => Self::TimestampMismatch,
			code @ 64.. => match u16::try_from(code - 64) {
				Ok(app) => Self::App(app),
				Err(_) => Self::Unknown(code),
			},
			code => Self::Unknown(code),
		}
	}
}

/// Failures in this crate, both local conditions and codes received off the wire.
///
/// Local conditions are the flat variants (`Cancel`, `NotFound`, `Lagged`, and so
/// on): this side decided what went wrong. Codes received from a peer are nested
/// in [`Self::Session`] or [`Self::Stream`], preserving the registry and the numeric
/// value. [`Self::Remote`] is an unrecognized request-rejection code (the IETF
/// request registry), not a session or stream unknown.
///
/// The two registries are the wire. Mapping a local condition onto a code is
/// [`SessionError::from`] / [`StreamError::from`]; folding the flat variants into
/// those registries is a later decision.
#[derive(thiserror::Error, Debug, Clone)]
#[non_exhaustive]
pub enum Error {
	/// The underlying QUIC/WebTransport connection failed; carries the backend's message.
	#[error("transport: {0}")]
	Transport(String),

	/// A message off the wire could not be parsed.
	#[error(transparent)]
	Decode(#[from] coding::DecodeError),

	/// Version negotiation failed, or the negotiated version lacks a requested feature
	/// (e.g. a FETCH against a version without fetch support). Mostly a connect-time
	/// error, but the feature-gap case can surface mid-session, so it can't simply move
	/// to a connect-only error type.
	#[error("unsupported versions")]
	Version,

	/// A known stream type arrived where this version or state does not allow it. Closes
	/// the session as a protocol violation, or resets just the stream when it can be
	/// refused on its own.
	#[error("unexpected stream type")]
	UnexpectedStream,

	/// An integer was too large for the QUIC varint range.
	#[error(transparent)]
	BoundsExceeded(#[from] coding::BoundsExceeded),

	/// A path holds a segment no pattern can spell (`*` or `**`), so it can never
	/// be announced or matched.
	#[error("invalid path: {0}")]
	InvalidPath(#[from] crate::InvalidPattern),

	/// A duplicate ID was used
	// The broadcast/track is a duplicate
	#[error("duplicate")]
	Duplicate,

	/// Nobody is reading any more, so the producer stopped. Not a failure.
	// Cancel is returned when there are no more readers.
	#[error("cancelled")]
	Cancel,

	/// It took too long to open or transmit a stream.
	#[error("timeout")]
	Timeout,

	/// The group is older than the latest group and dropped.
	#[error("old")]
	Old,

	/// An application-chosen close code. Bounded to `u16` and offset past the library's
	/// reserved range (`+ 64`) on the wire by [`SessionError::to_code`] /
	/// [`StreamError::to_code`], so app codes never collide with protocol ones.
	///
	/// The width asymmetry with [`Self::Remote`] is deliberate: `App` is a code *this*
	/// side chooses to send, while `Remote` carries a raw code *received* off the wire
	/// that didn't map to a known variant, which can be any `u32`.
	#[error("app code={0}")]
	App(u16),

	/// The requested broadcast or track does not exist at the peer.
	#[error("not found")]
	NotFound,

	/// A broadcast was requested that is neither announced nor served by a dynamic
	/// router, so there is no route to it.
	#[error("unroutable")]
	Unroutable,

	/// A frame's payload length disagreed with its declared size.
	#[error("wrong frame size")]
	WrongSize,

	/// The peer broke a protocol rule; the session is unusable.
	#[error("protocol violation")]
	ProtocolViolation,

	/// The requested path or operation is not granted, either by the peer's token
	/// or by the scope of the handle it was requested through.
	#[error("unauthorized")]
	Unauthorized,

	/// A valid message arrived in a state where it is not allowed.
	#[error("unexpected message")]
	UnexpectedMessage,

	/// The peer asked for a feature this endpoint does not implement.
	#[error("unsupported")]
	Unsupported,

	/// A message could not be serialized for the negotiated version.
	#[error(transparent)]
	Encode(#[from] coding::EncodeError),

	/// A message carried more parameters than this endpoint accepts.
	#[error("too many parameters")]
	TooManyParameters,

	/// The peer offered an ALPN this endpoint doesn't recognize, so no version could be
	/// negotiated. A connect-time error.
	#[error("unknown ALPN: {0}")]
	UnknownAlpn(String),

	/// The producer was dropped without finishing, so the content is incomplete.
	#[error("dropped")]
	Dropped,

	/// The handle was already closed by this side.
	#[error("closed")]
	Closed,

	/// The reader asked for a frame the group never held: below
	/// [`crate::group::Producer::start_at`], or skipped past a splice. Named from the
	/// consumer's side; distinct from [`Self::GroupTooLarge`], which aborts the whole
	/// group when a write exceeds the cache budget, and from [`Self::Evicted`], which
	/// drops a whole group under the pool's memory pressure.
	#[error("lagged")]
	Lagged,

	/// A frame declared a payload size larger than the receiver accepts.
	#[error("frame too large")]
	FrameTooLarge,

	/// A write would grow the group past its cache budget (byte size or frame count).
	/// The write is refused and the group is aborted, so every reader sees the same
	/// failure rather than a prefix some of them missed.
	#[error("group too large")]
	GroupTooLarge,

	/// A whole-frame write was refused because a frame is already open on the group.
	///
	/// A [`crate::group::Producer`] streaming a frame with `create_frame` blocks the
	/// whole-frame writes on every clone of that producer until it finishes, since
	/// appending around it would reorder the group.
	#[error("frame already open")]
	FrameOpen,

	/// A frame's timestamp doesn't match its track's negotiated timescale: it's
	/// missing on a timed track, present on an untimed track, or carries a
	/// different scale than the track advertised.
	#[error("frame timestamp doesn't match track timescale")]
	TimestampMismatch,

	/// The group was evicted by its own track to pay eviction debt under memory
	/// pressure (see [`cache::Pool`](crate::cache::Pool)). Unlike [`Self::Old`],
	/// the group was still within the publisher's window; it can be re-fetched.
	#[error("evicted")]
	Evicted,

	/// The session is going away (a GOAWAY was received); new subscribe and
	/// announce-interest requests are rejected while existing subscriptions
	/// keep flowing.
	#[error("going away")]
	GoingAway,

	/// The peer did not close the session within the GOAWAY drain deadline.
	///
	/// Sent as the session termination code when the draining side force-closes
	/// after the advertised deadline expires (see [`crate::goaway::Goaway::timeout`]).
	#[error("goaway timeout")]
	GoawayTimeout,

	/// The peer could not parse the track's content.
	#[error("malformed track")]
	MalformedTrack,

	/// The stream was torn down because the session closed. The specific reason travels on
	/// the session close, not here.
	#[error("session closed")]
	SessionClosed,

	/// A session-scoped protocol error, with its registry and verbatim code.
	///
	/// Produced by [`from_transport`](Self::from_transport) for a session close, and by
	/// converting a [`SessionError`]. Local conditions use the specific variants above.
	#[error(transparent)]
	Session(SessionError),

	/// A stream-scoped protocol error, with its registry and verbatim code.
	///
	/// The stream counterpart to [`Self::Session`]. The two registries are disjoint, so
	/// the same integer is a different failure in each.
	#[error(transparent)]
	Stream(StreamError),

	/// An unrecognized request-rejection code (the IETF request registry).
	///
	/// Session and stream unknowns are [`Self::Session`] / [`Self::Stream`], not this.
	#[error("remote error: code={0}")]
	Remote(u32),
}

impl Error {
	/// The session-scoped protocol error, if this was received as one.
	pub fn session(&self) -> Option<&SessionError> {
		match self {
			Self::Session(err) => Some(err),
			_ => None,
		}
	}

	/// The stream-scoped protocol error, if this was received as one.
	pub fn stream(&self) -> Option<&StreamError> {
		match self {
			Self::Stream(err) => Some(err),
			_ => None,
		}
	}

	/// Convert a transport error into an [Error], decoding session close and stream reset
	/// codes through their respective registries.
	///
	/// The two spaces are disjoint, so which one applies depends on what failed: a session
	/// close decodes via [`SessionError::from_code`], a stream reset via
	/// [`StreamError::from_code`]. Reading a stream reset with the session table (or the
	/// reverse) silently mistranslates, since e.g. 0 is "no error" for a session but an
	/// internal error for a stream.
	pub fn from_transport(err: impl web_transport_trait::Error) -> Self {
		if let Some((code, _reason)) = err.session_error() {
			return SessionError::from_code(code).into();
		}

		if let Some(code) = err.stream_error() {
			return StreamError::from_code(code).into();
		}

		Self::Transport(err.to_string())
	}
}

/// Preserve the session registry when carrying a protocol error.
impl From<SessionError> for Error {
	fn from(err: SessionError) -> Self {
		Self::Session(err)
	}
}

/// Preserve the stream registry when carrying a protocol error.
impl From<StreamError> for Error {
	fn from(err: StreamError) -> Self {
		Self::Stream(err)
	}
}

/// Which session code to send for a local error.
///
/// Lossy on purpose: [`Error`] describes what went wrong locally, while the registry is what
/// the peer can act on. Anything stream-scoped reaching a session close is a bug on our side,
/// so it degrades to [`SessionError::Internal`] rather than inventing a code.
impl From<&Error> for SessionError {
	fn from(err: &Error) -> Self {
		match err {
			Error::Session(err) => err.clone(),
			// App codes share the 64+ range in both registries.
			Error::Stream(StreamError::App(app)) => Self::App(*app),
			// A stream-scoped code has no meaning in this registry; don't forward the number.
			Error::Stream(_) => Self::Internal,
			Error::Cancel | Error::Closed | Error::GoingAway | Error::SessionClosed => Self::Cancel,
			Error::Unauthorized => Self::Unauthorized,
			Error::Version | Error::UnknownAlpn(_) => Self::Version,
			Error::TooManyParameters => Self::KeyValueFormatting,
			Error::GoawayTimeout => Self::GoawayTimeout,
			Error::Timeout => Self::Timeout,
			Error::ProtocolViolation
			| Error::UnexpectedMessage
			| Error::UnexpectedStream
			| Error::Duplicate
			| Error::Decode(_)
			| Error::Encode(_)
			| Error::WrongSize
			| Error::BoundsExceeded(_)
			| Error::InvalidPath(_) => Self::ProtocolViolation,
			Error::App(app) => Self::App(*app),
			// A code we did not recognize, so we cannot say which space it came from.
			// Forwarding it into this one risks landing on a value that IS registered here
			// (a session 0x4 would become the stream's GOING_AWAY), and the draft already
			// says an unrecognized code is an unspecified error. Send that instead.
			Error::Remote(_) => Self::Internal,
			_ => Self::Internal,
		}
	}
}

/// Which stream code to send for a local error.
///
/// Session-scoped conditions route through [`StreamError::Session`], which flattens to
/// `SESSION_CLOSED` on the wire.
impl From<&Error> for StreamError {
	fn from(err: &Error) -> Self {
		match err {
			Error::Stream(err) => err.clone(),
			// App codes share the 64+ range in both registries.
			Error::Session(SessionError::App(app)) => Self::App(*app),
			// A session-scoped code has no meaning in this registry; don't forward the number.
			Error::Session(_) => Self::Internal,
			Error::Cancel | Error::Closed => Self::Cancel,
			Error::SessionClosed => Self::Session(SessionError::Cancel),
			Error::Old => Self::Old,
			Error::Evicted => Self::Evicted,
			Error::Lagged => Self::TooFarBehind,
			Error::NotFound => Self::NotFound,
			Error::Unroutable => Self::Unroutable,
			Error::WrongSize => Self::WrongSize,
			Error::FrameTooLarge => Self::FrameTooLarge,
			Error::GroupTooLarge => Self::GroupTooLarge,
			Error::TimestampMismatch => Self::TimestampMismatch,
			Error::Timeout => Self::DeliveryTimeout,
			Error::GoingAway => Self::GoingAway,
			// Our own parse failure is, from the peer's side, a malformed track.
			Error::Decode(_) | Error::BoundsExceeded(_) | Error::InvalidPath(_) | Error::MalformedTrack => {
				Self::MalformedTrack
			}
			Error::App(app) => Self::App(*app),
			// See the SessionError impl: an unregistered code carries no registry, so
			// re-sending the number could mistranslate it.
			Error::Remote(_) => Self::Internal,
			// Session-scoped: the peer learns the detail from the session close.
			// A stream refused on its own is not a session failure, so it does not claim
			// SESSION_CLOSED; there is no stream-scoped PROTOCOL_VIOLATION to send instead.
			Error::UnexpectedStream => Self::Internal,
			Error::Unauthorized
			| Error::Version
			| Error::UnknownAlpn(_)
			| Error::TooManyParameters
			| Error::GoawayTimeout
			| Error::ProtocolViolation
			| Error::UnexpectedMessage => Self::Session(SessionError::from(err)),
			_ => Self::Internal,
		}
	}
}

impl web_transport_trait::Error for Error {
	fn session_error(&self) -> Option<(u32, String)> {
		None
	}
}

/// A [`Result`](std::result::Result) with this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
	use super::*;

	// Both registries are wire contracts now (draft-lcurley-moq-lite, Error Codes), so
	// every code we send must decode back to what we meant.
	#[test]
	fn session_codes_round_trip() {
		// Only the registered codes are a wire contract, so only they round trip.
		let registered = [
			SessionError::Cancel,
			SessionError::Internal,
			SessionError::Unauthorized,
			SessionError::ProtocolViolation,
			SessionError::KeyValueFormatting,
			SessionError::GoawayTimeout,
			SessionError::Timeout,
			SessionError::Version,
			SessionError::App(0),
			SessionError::App(404),
		];
		for err in registered {
			assert_eq!(
				SessionError::from_code(err.to_code()),
				err,
				"{err:?} did not round trip"
			);
		}

		// The moq-transport codes we reuse must keep moq-transport's values.
		assert_eq!(SessionError::Unauthorized.to_code(), 0x2);
		assert_eq!(SessionError::GoawayTimeout.to_code(), 0x10);
		assert_eq!(SessionError::Version.to_code(), 0x15);

		// The reserved 32-47 range, and anything else unregistered, keeps its value instead
		// of being given a meaning. A peer on the old placeholders (0x20-0x22) lands here.
		for code in [0x1f, 0x20, 0x21, 0x22, 0x2f] {
			assert_eq!(SessionError::from_code(code), SessionError::Unknown(code));
		}

		// A disallowed stream fails the session as a protocol violation, and resets just
		// the stream as an internal error rather than claiming the session closed.
		assert_eq!(
			SessionError::from(&Error::UnexpectedStream),
			SessionError::ProtocolViolation
		);
		assert_eq!(StreamError::from(&Error::UnexpectedStream), StreamError::Internal);
	}

	#[test]
	fn stream_codes_round_trip() {
		// Only the registered codes are a wire contract, so only they round trip.
		let registered = [
			StreamError::Internal,
			StreamError::Cancel,
			StreamError::DeliveryTimeout,
			StreamError::GoingAway,
			StreamError::TooFarBehind,
			StreamError::MalformedTrack,
			StreamError::GroupTooLarge,
			StreamError::NotFound,
			StreamError::Old,
			StreamError::Evicted,
			StreamError::Unroutable,
			StreamError::WrongSize,
			StreamError::FrameTooLarge,
			StreamError::TimestampMismatch,
			StreamError::App(7),
		];
		for err in registered {
			assert_eq!(StreamError::from_code(err.to_code()), err, "{err:?} did not round trip");
		}

		// moq-lite's own 48-63 range, pinned to the draft's table. They stay off 0x30-0x31
		// (NO_CAPACITY / CONTROL_TIMEOUT), which other work assigns.
		for (err, code) in [
			(StreamError::GroupTooLarge, 0x32),
			(StreamError::NotFound, 0x33),
			(StreamError::Old, 0x34),
			(StreamError::Evicted, 0x35),
			(StreamError::Unroutable, 0x36),
			(StreamError::WrongSize, 0x37),
			(StreamError::FrameTooLarge, 0x38),
			(StreamError::TimestampMismatch, 0x39),
		] {
			assert_eq!(err.to_code(), code, "{err:?} moved off its assigned code");
		}

		// The reserved 32-47 range carries no meaning, including the values the four
		// codes above used to be sent from: a peer still emitting them is not given one.
		for code in 0x20..0x30 {
			assert_eq!(StreamError::from_code(code), StreamError::Unknown(code));
		}

		// The spaces are disjoint: 0 is a cancellation for a session but an internal
		// error for a stream, and a cancellation is 1 here.
		assert_eq!(StreamError::Cancel.to_code(), 0x1);
		assert_eq!(StreamError::from_code(0x0), StreamError::Internal);
		assert_eq!(SessionError::from_code(0x0), SessionError::Cancel);

		// A session close flattens to SESSION_CLOSED: the specific reason travels on the
		// session close, not on the stream.
		assert_eq!(StreamError::Session(SessionError::Unauthorized).to_code(), 0x3);
		assert_eq!(
			StreamError::from_code(0x3),
			StreamError::Session(SessionError::Internal)
		);
	}

	// A relay decodes a peer's code into `Error` and re-encodes it when it tears down the
	// corresponding downstream stream. That hop must not change what the code means.
	#[test]
	fn relaying_a_code_does_not_change_registries() {
		// SESSION_CLOSED used to decode into a session-space value, which came back out as
		// 0x1: downstream read a routine CANCELLED for an upstream session teardown.
		let relayed = StreamError::from(&Error::from(StreamError::from_code(0x3)));
		assert_eq!(relayed.to_code(), 0x3);

		// Registered stream codes survive the hop unchanged.
		for code in [
			0x0, 0x1, 0x2, 0x4, 0x5, 0x12, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39,
		] {
			let relayed = StreamError::from(&Error::from(StreamError::from_code(code)));
			assert_eq!(relayed.to_code(), code, "stream {code:#x} changed across a relay");
		}

		// So do app codes, in both spaces.
		assert_eq!(
			StreamError::from(&Error::from(StreamError::from_code(64 + 7))).to_code(),
			64 + 7
		);
		assert_eq!(
			SessionError::from(&Error::from(SessionError::from_code(64 + 7))).to_code(),
			64 + 7
		);

		// An unregistered code carries no registry, so it must not be re-sent as a number
		// that means something here: session 0x4 and 0x5 are GOING_AWAY and TOO_FAR_BEHIND
		// on a stream. Downgrade to the unspecified-error code instead.
		for code in [0x4, 0x5, 0x1f] {
			let crossed = StreamError::from(&Error::from(SessionError::from_code(code)));
			assert_eq!(
				crossed,
				StreamError::Internal,
				"session {code:#x} leaked into the stream space"
			);
		}
	}

	// The registry a code is read with depends on what failed, and picking the wrong one
	// silently mistranslates rather than erroring.
	#[test]
	fn from_transport_selects_the_matching_registry() {
		#[derive(Debug, thiserror::Error)]
		#[error("failed")]
		struct Failed {
			session: Option<u32>,
			stream: Option<u32>,
		}

		impl web_transport_trait::Error for Failed {
			fn session_error(&self) -> Option<(u32, String)> {
				self.session.map(|code| (code, "closed".to_string()))
			}
			fn stream_error(&self) -> Option<u32> {
				self.stream
			}
		}

		let session = |code| {
			Error::from_transport(Failed {
				session: Some(code),
				stream: None,
			})
		};
		let stream = |code| {
			Error::from_transport(Failed {
				session: None,
				stream: Some(code),
			})
		};

		// A MoQ-layer auth rejection is now classifiable, because the code is specified.
		assert!(matches!(session(0x2), Error::Session(SessionError::Unauthorized)));
		assert_eq!(session(0x2).session(), Some(&SessionError::Unauthorized));
		assert!(matches!(session(0x0), Error::Session(SessionError::Cancel)));

		// Same integer, different space: 0 ends a session cleanly but fails a stream.
		assert!(matches!(stream(0x1), Error::Stream(StreamError::Cancel)));
		assert_eq!(stream(0x1).stream(), Some(&StreamError::Cancel));
		assert!(matches!(stream(0x0), Error::Stream(StreamError::Internal)));
		assert!(matches!(stream(0x5), Error::Stream(StreamError::TooFarBehind)));
		assert!(matches!(stream(0x32), Error::Stream(StreamError::GroupTooLarge)));

		// Assigned lite codes round-trip to the named error. The reserved 32-47 range
		// stays opaque: the draft forbids reading a meaning out of it.
		assert!(matches!(stream(0x34), Error::Stream(StreamError::Old)));
		assert!(matches!(stream(0x38), Error::Stream(StreamError::FrameTooLarge)));
		assert!(matches!(stream(0x22), Error::Stream(StreamError::Unknown(0x22))));
		assert!(matches!(session(0x22), Error::Session(SessionError::Unknown(0x22))));

		// Neither: the transport itself failed.
		assert!(matches!(
			Error::from_transport(Failed {
				session: None,
				stream: None
			}),
			Error::Transport(_)
		));
	}
}
