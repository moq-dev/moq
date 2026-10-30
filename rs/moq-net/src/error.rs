use crate::coding;

/// A code sent when terminating the session.
///
/// One of the two wire registries specified by moq-lite, which reuse moq-transport's codes
/// unchanged; 64+ are the application's. The stream registry is [`StreamError`] and the two
/// are disjoint, so the same integer means different things in each.
///
/// Shared codes round-trip through [`to_code`](Self::to_code) and
/// [`from_code`](Self::from_code). Protocol drivers translate Lite-only variants at
/// their wire boundary; the shared decoder keeps those numbers [`Unknown`](Self::Unknown).
/// Codes 32 through 47 are reserved and have no assigned meaning.
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

	/// The peer's token has expired. Retrying with refreshed credentials may succeed.
	#[error("authorization token expired")]
	Expired,

	/// The peer broke a protocol rule; the session is unusable.
	#[error("protocol violation")]
	ProtocolViolation,

	/// A key-value pair was malformed or repeated more than allowed.
	#[error("key-value formatting error")]
	KeyValueFormatting,

	/// The peer went past what the session allows, such as more announcements than its
	/// `crate::session::Limits`.
	#[error("too many requests")]
	TooManyRequests,

	/// The peer did not close within the GOAWAY drain deadline.
	#[error("goaway timeout")]
	GoawayTimeout,

	/// A control message took too long.
	#[error("control message timeout")]
	Timeout,

	/// A token registration would exceed the advertised `MAX_AUTH_TOKEN_CACHE_SIZE`, which is
	/// 0 when unadvertised.
	#[error("auth token cache overflow")]
	AuthTokenCacheOverflow,

	/// No version could be negotiated.
	#[error("version negotiation failed")]
	Version,

	/// The peer's token could not be parsed. Retrying with the same token will fail again.
	#[error("malformed auth token")]
	MalformedAuthToken,

	/// A token named an alias that was never registered.
	#[error("unknown auth token alias")]
	UnknownAuthTokenAlias,

	/// The peer left more unacknowledged REQUEST_UPDATEs outstanding on one request stream
	/// than the MAX_REQUEST_UPDATES it was advertised (draft-22 section 9.1.7).
	#[error("too many request updates")]
	TooManyRequestUpdates,

	/// An application-chosen code, offset into the 64+ range on the wire.
	#[error("app code={0}")]
	App(u16),

	/// A code this version does not recognize, kept verbatim.
	#[error("unknown code={0}")]
	Unknown(u32),

	/// The peer cannot support the requested feature; moq-lite only.
	#[error("not supported")]
	Unsupported,

	/// A request ID was reused or violated the negotiated draft’s order or parity rules.
	#[error("invalid request id")]
	InvalidRequestId,

	/// The peer sent a PATH it may not send, such as a server sending one at all.
	#[error("invalid path")]
	InvalidPath,

	/// The peer sent an AUTHORITY it may not send, such as a server sending one at all.
	#[error("invalid authority")]
	InvalidAuthority,
}

impl SessionError {
	/// The shared session code; Lite-only conditions fall back to an internal error.
	pub fn to_code(&self) -> u32 {
		match self {
			Self::Cancel => 0x0,
			Self::Internal => 0x1,
			Self::Unauthorized => 0x2,
			Self::Expired => 0x18,
			Self::ProtocolViolation => 0x3,
			Self::InvalidRequestId => 0x4,
			Self::KeyValueFormatting => 0x6,
			Self::TooManyRequests => 0x7,
			Self::InvalidPath => 0x8,
			Self::GoawayTimeout => 0x10,
			Self::Timeout => 0x11,
			Self::AuthTokenCacheOverflow => 0x13,
			Self::Version => 0x15,
			Self::MalformedAuthToken => 0x16,
			Self::UnknownAuthTokenAlias => 0x17,
			Self::InvalidAuthority => 0x19,
			Self::Unsupported => 0x1,
			Self::TooManyRequestUpdates => 0x1B,
			Self::App(app) => *app as u32 + 64,
			Self::Unknown(code) => *code,
		}
	}

	/// Decode a shared session code, leaving protocol-specific codes unknown.
	///
	/// Unlike a raw peer code, the registered ones are specified, so decoding them is a
	/// wire contract rather than an assumption. Anything unregistered stays
	/// [`Self::Unknown`], including the reserved 32-47.
	pub fn from_code(code: u32) -> Self {
		match code {
			0x0 => Self::Cancel,
			0x1 => Self::Internal,
			0x2 => Self::Unauthorized,
			0x18 => Self::Expired,
			0x3 => Self::ProtocolViolation,
			0x4 => Self::InvalidRequestId,
			0x6 => Self::KeyValueFormatting,
			0x7 => Self::TooManyRequests,
			0x8 => Self::InvalidPath,
			0x10 => Self::GoawayTimeout,
			0x11 => Self::Timeout,
			0x13 => Self::AuthTokenCacheOverflow,
			0x15 => Self::Version,
			0x16 => Self::MalformedAuthToken,
			0x17 => Self::UnknownAuthTokenAlias,
			0x19 => Self::InvalidAuthority,
			0x1B => Self::TooManyRequestUpdates,
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

	/// A control request took too long to be answered. The stream counterpart to
	/// [`SessionError::Timeout`], which covers the same condition at session scope.
	#[error("control timeout")]
	ControlTimeout,

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

	/// A FETCH reached a datagram, which is never cached. Sent on moq-lite-07
	/// and later; an earlier version sends [`NotFound`](Self::NotFound) instead.
	#[error("not fetchable")]
	NotFetchable,

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

	/// A frame's timestamp doesn't fit its track: missing on a timed track, present on an
	/// untimed one, out of range for the track's timescale, or starting a group earlier
	/// than the group before it.
	#[error("frame timestamp doesn't fit its track")]
	TimestampMismatch,

	/// The grant does not cover this subscription, fetch, or announcement, or no longer
	/// does. Unlike [`SessionError::Unauthorized`], the session stays up.
	#[error("unauthorized")]
	Unauthorized,

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
			Self::ControlTimeout => 0x31,
			Self::GroupTooLarge => 0x32,
			Self::NotFound => 0x33,
			Self::Old => 0x34,
			Self::Evicted => 0x35,
			Self::Unroutable => 0x36,
			Self::WrongSize => 0x37,
			Self::FrameTooLarge => 0x38,
			Self::TimestampMismatch => 0x39,
			Self::NotFetchable => 0x3a,
			Self::Unauthorized => 0x3b,
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
			0x31 => Self::ControlTimeout,
			0x32 => Self::GroupTooLarge,
			0x33 => Self::NotFound,
			0x34 => Self::Old,
			0x35 => Self::Evicted,
			0x36 => Self::Unroutable,
			0x37 => Self::WrongSize,
			0x38 => Self::FrameTooLarge,
			0x39 => Self::TimestampMismatch,
			0x3a => Self::NotFetchable,
			0x3b => Self::Unauthorized,
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
/// value. A stream also reads its session ending cleanly, whichever side closed it, as
/// `Session(SessionError::Cancel)`: to the stream that is the session failing under it,
/// while `Session::closed` reports the clean close itself as [`Self::Cancel`].
/// [`Self::Remote`] is an unrecognized request-rejection code (the IETF
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

	/// A published broadcast or route contains a literal `.` or `..` segment.
	#[error("broadcast path contains a navigation segment")]
	NavigationSegment,

	/// A path was spelled with a leading, trailing, or doubled `/`.
	#[error("path contains an empty segment")]
	EmptySegment,

	/// A path was spelled with a `\` before anything but `\` or `/`.
	#[error(r"path contains a `\` that escapes neither `\` nor `/`")]
	InvalidEscape,

	/// A duplicate ID was used
	// The broadcast/track is a duplicate
	#[error("duplicate")]
	Duplicate,

	/// Nobody is reading any more, so the producer stopped; or the session closed cleanly,
	/// by either side, as `Session::closed` reports it. Not a failure.
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

	/// A FETCH reached a datagram, which is never cached.
	#[error("not fetchable")]
	NotFetchable,

	/// A joining FETCH named a request that is not an active subscription.
	#[error("invalid joining request ID")]
	InvalidJoiningRequestId,

	/// A FETCH range is empty or lies beyond the published objects.
	#[error("invalid fetch range")]
	InvalidRange,

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

	/// The authorization token has expired and can be refreshed.
	#[error("authorization token expired")]
	Expired,

	/// A valid message arrived in a state where it is not allowed.
	#[error("unexpected message")]
	UnexpectedMessage,

	/// A feature was asked for that this endpoint, or the wire to the peer, does not implement.
	#[error("unsupported")]
	Unsupported,

	/// A message could not be serialized for the negotiated version.
	#[error(transparent)]
	Encode(#[from] coding::EncodeError),

	/// The peer went past this session's `crate::session::Limits`, such as too many
	/// announcements.
	#[error("too many requests")]
	TooManyRequests,

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
	/// [`crate::group::Producer::start_at`]. Named from the consumer's side; distinct
	/// from [`Self::GroupTooLarge`], which aborts the whole group when a write exceeds
	/// the cache budget, and from [`Self::Evicted`], which drops a whole group under the
	/// pool's memory pressure.
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

	/// A frame's timestamp doesn't fit its track: it's missing on a timed track, present
	/// on an untimed track, out of range for the track's timescale, or starts a group
	/// earlier than the group before it.
	#[error("frame timestamp doesn't fit its track")]
	TimestampMismatch,

	/// The group was evicted under memory pressure: by its own track to pay eviction debt
	/// (see [`cache::Pool`](crate::cache::Pool)), or by the transport when incomplete
	/// messages spent its receive window. Unlike [`Self::Old`], the group was still within
	/// the publisher's window; it can be re-fetched.
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
	/// after the advertised drain deadline expires.
	#[error("goaway timeout")]
	GoawayTimeout,

	/// The peer could not parse the track's content.
	#[error("malformed track")]
	MalformedTrack,

	/// The track requires a moq-transport property this endpoint does not understand.
	#[error("unsupported extension")]
	UnsupportedExtension,

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
	/// The underlying connection reached its transport idle deadline.
	#[error("transport idle timeout")]
	IdleTimeout,

	/// The transport refused a request because the peer granted no stream credit.
	#[error("no stream credit")]
	NoStreamCredit,
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
	///
	/// Without either code, the transport's [`close_kind`](crate::transport::Error::close_kind)
	/// decides: an idle timeout is [`Error::IdleTimeout`], a local close is
	/// `Error::Session(SessionError::Cancel)`, and anything else is [`Error::Transport`].
	pub fn from_transport(err: impl crate::transport::Error) -> Self {
		if let Some((code, _reason)) = err.session_error() {
			return SessionError::from_code(code).into();
		}

		if let Some(code) = err.stream_error() {
			return StreamError::from_code(code).into();
		}

		match err.close_kind() {
			crate::transport::CloseKind::IdleTimeout => Self::IdleTimeout,
			crate::transport::CloseKind::Evicted => Self::Evicted,
			// A connection ending must not become the model's clean end-of-content.
			crate::transport::CloseKind::Closed => SessionError::Cancel.into(),
			_ => Self::Transport(err.to_string()),
		}
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

/// Lets an entry point taking `impl TryInto<Path, Error: Into<Error>>` accept a `Path` too.
impl From<std::convert::Infallible> for Error {
	fn from(never: std::convert::Infallible) -> Self {
		match never {}
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
			Error::Session(err) | Error::Decode(coding::DecodeError::Session(err)) => err.clone(),
			// App codes share the 64+ range in both registries.
			Error::Stream(StreamError::App(app)) => Self::App(*app),
			// A stream-scoped code has no meaning in this registry; don't forward the number.
			Error::Stream(_) => Self::Internal,
			Error::Cancel | Error::Closed | Error::GoingAway => Self::Cancel,
			Error::Unauthorized => Self::Unauthorized,
			Error::Expired => Self::Expired,
			Error::Unsupported => Self::Unsupported,
			Error::Version | Error::UnknownAlpn(_) => Self::Version,
			Error::TooManyRequests => Self::TooManyRequests,
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
			| Error::InvalidPath(_)
			| Error::NavigationSegment
			| Error::EmptySegment
			| Error::InvalidEscape => Self::ProtocolViolation,
			Error::App(app) => Self::App(*app),
			// A code we did not recognize, so we cannot say which space it came from.
			// Forwarding it into this one risks landing on a value that IS registered here
			// (a session 0x4 would become the stream's GOING_AWAY), and the draft already
			// says an unrecognized code is an unspecified error. Send that instead.
			Error::Remote(_) => Self::Internal,
			// Our own transport failed: there is no code to say so.
			Error::Transport(_) | Error::IdleTimeout | Error::NoStreamCredit => Self::Internal,
			// Stream-scoped conditions have no session code.
			Error::Old
			| Error::Evicted
			| Error::Lagged
			| Error::NotFound
			| Error::NotFetchable
			| Error::InvalidRange
			| Error::InvalidJoiningRequestId
			| Error::Unroutable
			| Error::FrameTooLarge
			| Error::GroupTooLarge
			| Error::TimestampMismatch
			| Error::MalformedTrack
			| Error::UnsupportedExtension => Self::Internal,
			// A producer dropped without an end is a bug here, so it is never given a meaning.
			Error::Dropped => Self::Internal,
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
			// A stream that can still be reset outlived the session that failed, so that was
			// another one: the route this stream was served through. The path lost its route;
			// this session is fine, and the failed session's code means nothing here.
			Error::Session(_) | Error::Transport(_) | Error::IdleTimeout => Self::Unroutable,
			Error::Cancel | Error::Closed => Self::Cancel,
			Error::Decode(coding::DecodeError::Session(err)) => Self::Session(err.clone()),
			Error::Old => Self::Old,
			Error::Evicted => Self::Evicted,
			Error::Lagged => Self::TooFarBehind,
			Error::NotFound => Self::NotFound,
			Error::NotFetchable => Self::NotFetchable,
			Error::Unroutable => Self::Unroutable,
			Error::WrongSize => Self::WrongSize,
			Error::FrameTooLarge => Self::FrameTooLarge,
			Error::GroupTooLarge => Self::GroupTooLarge,
			Error::TimestampMismatch => Self::TimestampMismatch,
			// Losing access ends this stream, not the session. A request can expire on its
			// own (REQUEST_ERROR 0x5), and the stream registry has no expiry code.
			Error::Unauthorized | Error::Expired => Self::Unauthorized,
			Error::Timeout => Self::DeliveryTimeout,
			Error::GoingAway => Self::GoingAway,
			// Our own parse failure is, from the peer's side, a malformed track.
			Error::Decode(_)
			| Error::BoundsExceeded(_)
			| Error::InvalidPath(_)
			| Error::NavigationSegment
			| Error::EmptySegment
			| Error::InvalidEscape
			| Error::MalformedTrack => Self::MalformedTrack,
			Error::App(app) => Self::App(*app),
			// See the SessionError impl: an unregistered code carries no registry, so
			// re-sending the number could mistranslate it.
			Error::Remote(_) => Self::Internal,
			// Session-scoped: the peer learns the detail from the session close.
			// A stream refused on its own is not a session failure, so it does not claim
			// SESSION_CLOSED; there is no stream-scoped PROTOCOL_VIOLATION to send instead.
			Error::UnexpectedStream | Error::NoStreamCredit => Self::Internal,
			Error::Version
			| Error::UnknownAlpn(_)
			| Error::TooManyRequests
			| Error::Duplicate
			| Error::GoawayTimeout
			| Error::ProtocolViolation
			| Error::UnexpectedMessage => Self::Session(SessionError::from(err)),
			// No stream code says so: an unknown stream type, a request only moq-transport
			// can make, or a fault on our own side.
			Error::Unsupported
			| Error::UnsupportedExtension
			| Error::InvalidRange
			| Error::InvalidJoiningRequestId
			| Error::Encode(_) => Self::Internal,
			// A producer dropped without an end is a bug here, so it is never given a meaning.
			Error::Dropped => Self::Internal,
		}
	}
}

impl crate::transport::Error for Error {
	fn close_kind(&self) -> crate::transport::CloseKind {
		match self {
			Self::IdleTimeout => crate::transport::CloseKind::IdleTimeout,
			// Closed also ends model handles and virtual streams, not just connections.
			_ => crate::transport::CloseKind::Transport,
		}
	}
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
	fn lite_unsupported_is_not_in_the_shared_session_registry() {
		assert_eq!(SessionError::from(&Error::Unsupported).to_code(), 0x1);
		assert_eq!(SessionError::from_code(0x30), SessionError::Unknown(0x30));
	}

	#[test]
	fn session_codes_round_trip() {
		// Only the registered codes are a wire contract, so only they round trip, each on
		// the value the drafts assign (moq-transport's, where we reuse its code).
		for (err, spec) in [
			(SessionError::Cancel, 0x0),
			(SessionError::Internal, 0x1),
			(SessionError::Unauthorized, 0x2),
			(SessionError::ProtocolViolation, 0x3),
			(SessionError::InvalidRequestId, 0x4),
			(SessionError::KeyValueFormatting, 0x6),
			(SessionError::TooManyRequests, 0x7),
			(SessionError::InvalidPath, 0x8),
			(SessionError::GoawayTimeout, 0x10),
			(SessionError::Timeout, 0x11),
			(SessionError::AuthTokenCacheOverflow, 0x13),
			(SessionError::Version, 0x15),
			(SessionError::MalformedAuthToken, 0x16),
			(SessionError::UnknownAuthTokenAlias, 0x17),
			(SessionError::Expired, 0x18),
			(SessionError::InvalidAuthority, 0x19),
			(SessionError::TooManyRequestUpdates, 0x1B),
			(SessionError::App(0), 0x40),
			(SessionError::App(404), 0x40 + 404),
		] {
			assert_eq!(err.to_code(), spec, "{err:?} moved off its assigned code");
			assert_eq!(SessionError::from_code(spec), err, "{err:?} did not round trip");
		}

		// The reserved 32-47 range, and anything else unregistered, keeps its value instead
		// of being given a meaning. A peer on the old placeholders (0x20-0x22) lands here.
		for code in [0x1f, 0x20, 0x21, 0x22, 0x2f, 0x30] {
			assert_eq!(SessionError::from_code(code), SessionError::Unknown(code));
		}

		// A disallowed stream fails the session as a protocol violation, and resets just
		// the stream as an internal error rather than claiming the session closed.
		assert_eq!(
			SessionError::from(&Error::UnexpectedStream),
			SessionError::ProtocolViolation
		);
		assert_eq!(StreamError::from(&Error::UnexpectedStream), StreamError::Internal);

		// An expired request (REQUEST_ERROR 0x5) relayed onto a stream ends only that
		// request; SESSION_CLOSED would falsely tell the peer its session is going away.
		assert_eq!(StreamError::from(&Error::Expired), StreamError::Unauthorized);
	}

	#[test]
	fn stream_codes_round_trip() {
		// Only the registered codes are a wire contract, so only they round trip, each on
		// the value the draft assigns.
		for (err, spec) in [
			(StreamError::Internal, 0x0),
			(StreamError::Cancel, 0x1),
			(StreamError::DeliveryTimeout, 0x2),
			(StreamError::GoingAway, 0x4),
			(StreamError::TooFarBehind, 0x5),
			(StreamError::MalformedTrack, 0x12),
			(StreamError::ControlTimeout, 0x31),
			(StreamError::GroupTooLarge, 0x32),
			(StreamError::NotFound, 0x33),
			(StreamError::Old, 0x34),
			(StreamError::Evicted, 0x35),
			(StreamError::Unroutable, 0x36),
			(StreamError::WrongSize, 0x37),
			(StreamError::FrameTooLarge, 0x38),
			(StreamError::TimestampMismatch, 0x39),
			(StreamError::NotFetchable, 0x3a),
			(StreamError::Unauthorized, 0x3b),
			(StreamError::App(7), 0x40 + 7),
		] {
			assert_eq!(err.to_code(), spec, "{err:?} moved off its assigned code");
			assert_eq!(StreamError::from_code(spec), err, "{err:?} did not round trip");
		}

		// The reserved 32-47 range carries no meaning, so a peer emitting it is not given one.
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

		// A datagram reached by a FETCH keeps its own code rather than reading as a plain miss.
		assert_eq!(StreamError::from(&Error::NotFetchable), StreamError::NotFetchable);

		// Registered stream codes survive the hop unchanged.
		for code in [
			0x0, 0x1, 0x2, 0x4, 0x5, 0x12, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x3b,
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

		// An upstream session's close must not be re-sent as a number that means something
		// here: session 0x4 and 0x5 are GOING_AWAY and TOO_FAR_BEHIND on a stream. The
		// downstream session is healthy; only the path lost its route.
		for code in [0x0, 0x4, 0x5, 0x1f] {
			let crossed = StreamError::from(&Error::from(SessionError::from_code(code)));
			assert_eq!(
				crossed,
				StreamError::Unroutable,
				"session {code:#x} leaked into the stream space"
			);
		}
		assert_eq!(
			StreamError::from(&Error::Transport("reset".into())),
			StreamError::Unroutable
		);
		assert_eq!(StreamError::from(&Error::IdleTimeout), StreamError::Unroutable);

		// A producer dropped without an end is a bug, never given a meaning.
		assert_eq!(StreamError::from(&Error::Dropped), StreamError::Internal);
		assert_eq!(SessionError::from(&Error::Dropped), SessionError::Internal);
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

		impl crate::transport::Error for Failed {
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
		assert!(matches!(stream(0x31), Error::Stream(StreamError::ControlTimeout)));
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
