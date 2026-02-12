use crate::coding;

/// A list of possible errors that can occur during the session.
#[derive(thiserror::Error, Debug, Clone)]
#[non_exhaustive]
pub enum Error {
	#[error("transport error")]
	Transport,

	#[error("decode error")]
	Decode,

	// TODO move to a ConnectError
	#[error("unsupported versions")]
	Version,

	/// A required extension was not present
	#[error("extension required")]
	RequiredExtension,

	/// An unexpected stream type was received
	#[error("unexpected stream type")]
	UnexpectedStream,

	/// Some VarInt was too large and we were too lazy to handle it
	#[error("varint bounds exceeded")]
	BoundsExceeded,

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

	#[error("too large")]
	TooLarge,

	#[error("too many parameters")]
	TooManyParameters,

	#[error("invalid role")]
	InvalidRole,

	#[error("unknown ALPN: {0}")]
	UnknownAlpn(String),
}

impl Error {
	/// An integer code that is sent over the wire.
	pub fn to_code(&self) -> u32 {
		match self {
			Self::Cancel => 0,
			Self::RequiredExtension => 1,
			Self::Old => 2,
			Self::Timeout => 3,
			Self::Transport => 4,
			Self::Decode => 5,
			Self::Unauthorized => 6,
			Self::Version => 9,
			Self::UnexpectedStream => 10,
			Self::BoundsExceeded => 11,
			Self::Duplicate => 12,
			Self::NotFound => 13,
			Self::WrongSize => 14,
			Self::ProtocolViolation => 15,
			Self::UnexpectedMessage => 16,
			Self::Unsupported => 17,
			Self::TooLarge => 18,
			Self::TooManyParameters => 19,
			Self::InvalidRole => 20,
			Self::UnknownAlpn(_) => 21,
			Self::App(app) => *app as u32 + 64,
		}
	}

	/// Decode an error from a wire code.
	pub fn from_code(code: u32) -> Self {
		match code {
			0 => Self::Cancel,
			1 => Self::RequiredExtension,
			2 => Self::Old,
			3 => Self::Timeout,
			4 => Self::Transport,
			5 => Self::Decode,
			6 => Self::Unauthorized,
			9 => Self::Version,
			10 => Self::UnexpectedStream,
			11 => Self::BoundsExceeded,
			12 => Self::Duplicate,
			13 => Self::NotFound,
			14 => Self::WrongSize,
			15 => Self::ProtocolViolation,
			16 => Self::UnexpectedMessage,
			17 => Self::Unsupported,
			18 => Self::TooLarge,
			19 => Self::TooManyParameters,
			20 => Self::InvalidRole,
			code if code >= 64 => match u16::try_from(code - 64) {
				Ok(app) => Self::App(app),
				Err(_) => Self::ProtocolViolation,
			},
			_ => Self::ProtocolViolation,
		}
	}

	/// Convert a transport error into an [Error], decoding stream reset codes.
	pub fn from_transport(err: impl web_transport_trait::Error) -> Self {
		if let Some(code) = err.stream_error() {
			return Self::from_code(code);
		}

		tracing::warn!(%err, "transport error");
		Self::Transport
	}
}

impl From<coding::DecodeError> for Error {
	fn from(err: coding::DecodeError) -> Self {
		tracing::warn!(%err, "decode error");
		Error::Decode
	}
}

impl From<coding::BoundsExceeded> for Error {
	fn from(err: coding::BoundsExceeded) -> Self {
		tracing::warn!(%err, "bounds exceeded");
		Error::BoundsExceeded
	}
}

pub type Result<T> = std::result::Result<T, Error>;
