use std::sync::Arc;

use crate::coding;
use web_transport_trait::{MaybeSend, MaybeSync};

/// A trait that is Send+Sync except on WASM.
pub trait SendSyncError: std::error::Error + MaybeSend + MaybeSync {}

impl<T> SendSyncError for T where T: std::error::Error + MaybeSend + MaybeSync {}

/// A list of possible errors that can occur during the session.
#[derive(thiserror::Error, Debug, Clone)]
pub enum Error {
	#[error("transport error: {0}")]
	Transport(Arc<dyn SendSyncError>),

	#[error("decode error: {0}")]
	Decode(#[from] coding::DecodeError),

	// TODO move to a ConnectError
	#[error("unsupported versions: client={0:?} server={1:?}")]
	Version(coding::Versions, coding::Versions),

	/// A required extension was not present
	#[error("extension required: {0}")]
	RequiredExtension(u64),

	/// An unexpected stream type was received
	#[error("unexpected stream type")]
	UnexpectedStream,

	/// Some VarInt was too large and we were too lazy to handle it
	#[error("varint bounds exceeded")]
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
	App(u32),

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
			Self::RequiredExtension(_) => 1,
			Self::Old => 2,
			Self::Timeout => 3,
			Self::Transport(_) => 4,
			Self::Decode(_) => 5,
			Self::Unauthorized => 6,
			Self::Version(..) => 9,
			Self::UnexpectedStream => 10,
			Self::BoundsExceeded(_) => 11,
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
			Self::App(app) => *app + 64,
		}
	}
}

pub type Result<T> = std::result::Result<T, Error>;
