use crate::coding;

/// A list of possible errors that can occur during the session.
#[derive(thiserror::Error, Debug, Clone)]
#[non_exhaustive]
pub enum Error {
	#[error("transport: {0}")]
	Transport(String),

	#[error(transparent)]
	Decode(#[from] coding::DecodeError),

	// TODO move to a ConnectError
	#[error("unsupported versions")]
	Version,

	/// A required extension was not present
	#[error("extension required")]
	RequiredExtension,

	/// An unexpected stream type was received
	#[error("unexpected stream type")]
	UnexpectedStream,

	#[error(transparent)]
	BoundsExceeded(#[from] coding::BoundsExceeded),

	/// A duplicate ID was used
	// The broadcast/track is a duplicate
	#[error("duplicate")]
	Duplicate,

	// Cancel is returned when there are no more readers.
	#[error("cancelled")]
	Cancel,

	/// It took too long to open or transmit a stream.
	#[error("timeout")]
	Timeout,

	/// The group is older than the latest group and dropped.
	#[error("old")]
	Old,

	// The application closes the stream with a code.
	#[error("app code={0}")]
	App(u16),

	#[error("not found")]
	NotFound,

	#[error("wrong frame size")]
	WrongSize,

	#[error("protocol violation")]
	ProtocolViolation,

	#[error("unauthorized")]
	Unauthorized,

	#[error("unexpected message")]
	UnexpectedMessage,

	#[error("unsupported")]
	Unsupported,

	#[error(transparent)]
	Encode(#[from] coding::EncodeError),

	#[error("too many parameters")]
	TooManyParameters,

	#[error("invalid role")]
	InvalidRole,

	#[error("unknown ALPN: {0}")]
	UnknownAlpn(String),

	#[error("dropped")]
	Dropped,

	#[error("closed")]
	Closed,

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

pub type Result<T> = std::result::Result<T, Error>;
