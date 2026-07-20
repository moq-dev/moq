use crate::coding;

/// A list of possible errors that can occur during the session.
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

	/// A required extension was not present
	#[error("extension required")]
	RequiredExtension,

	/// An unexpected stream type was received
	#[error("unexpected stream type")]
	UnexpectedStream,

	/// An integer was too large for the QUIC varint range.
	#[error(transparent)]
	BoundsExceeded(#[from] coding::BoundsExceeded),

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
	/// reserved range (`+ 64`) on the wire by [`Self::to_code`], so app codes never
	/// collide with protocol ones.
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

	/// The peer's token does not grant the requested path or operation.
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

	/// The peer acted against the [`Role`](crate::Role) it advertised at SETUP.
	#[error("invalid role")]
	InvalidRole,

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

	/// The reader fell behind the group's byte budget: the frame it wanted was dropped
	/// to keep the group under its size limit. Named from the consumer's side (nothing is
	/// "full"); distinct from [`Self::Evicted`], which drops a whole group under the
	/// pool's memory pressure.
	#[error("lagged")]
	Lagged,

	/// A frame declared a payload size larger than the receiver accepts.
	#[error("frame too large")]
	FrameTooLarge,

	/// A frame's timestamp doesn't match its track's negotiated timescale: it's
	/// missing on a timed track, present on an untimed track, or carries a
	/// different scale than the track advertised.
	#[error("frame timestamp doesn't match track timescale")]
	TimestampMismatch,

	/// The group was evicted from the cache under memory pressure (see
	/// [`cache::Pool`](crate::cache::Pool)). Unlike [`Self::Old`], the group was
	/// still within the publisher's window; it can be re-fetched.
	#[error("evicted")]
	Evicted,

	/// A remote error received via a stream/session reset code.
	#[error("remote error: code={0}")]
	Remote(u32),
}

impl Error {
	/// An integer code that is sent over the wire.
	pub fn to_code(&self) -> u32 {
		match self {
			Self::Cancel => 0,
			Self::RequiredExtension => 1,
			Self::Old => 2,
			Self::Timeout => 3,
			Self::Transport(_) => 4,
			Self::Decode(_) => 5,
			Self::Unauthorized => 6,
			Self::Version => 9,
			Self::UnexpectedStream => 10,
			Self::BoundsExceeded(_) => 11,
			Self::Duplicate => 12,
			Self::NotFound => 13,
			Self::WrongSize => 14,
			Self::ProtocolViolation => 15,
			Self::UnexpectedMessage => 16,
			Self::Unsupported => 17,
			Self::Encode(_) => 18,
			Self::TooManyParameters => 19,
			Self::InvalidRole => 20,
			Self::UnknownAlpn(_) => 21,
			Self::Dropped => 24,
			Self::Closed => 25,
			Self::Lagged => 26,
			Self::FrameTooLarge => 27,
			// 28 is reserved (was per-frame decompression, removed in draft-05).
			Self::TimestampMismatch => 29,
			Self::Unroutable => 30,
			Self::Evicted => 31,
			Self::App(app) => *app as u32 + 64,
			Self::Remote(code) => *code,
		}
	}

	/// Convert a transport error into an [Error], decoding stream reset codes.
	pub fn from_transport(err: impl web_transport_trait::Error) -> Self {
		if let Some(code) = err.stream_error() {
			return Self::Remote(code);
		}

		Self::Transport(err.to_string())
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

	// The wire codes are a stable contract with every other implementation, so a variant
	// rename (e.g. CacheFull -> Lagged) must not shift them. Pin the load-bearing ones.
	#[test]
	fn to_code_is_stable() {
		assert_eq!(Error::Cancel.to_code(), 0);
		assert_eq!(Error::Version.to_code(), 9);
		assert_eq!(Error::UnknownAlpn(String::new()).to_code(), 21);
		assert_eq!(Error::Lagged.to_code(), 26);
		assert_eq!(Error::Evicted.to_code(), 31);
		// App codes sit past the reserved library range; Remote is the raw received code.
		assert_eq!(Error::App(0).to_code(), 64);
		assert_eq!(Error::App(404).to_code(), 468);
		assert_eq!(Error::Remote(468).to_code(), 468);
	}
}
