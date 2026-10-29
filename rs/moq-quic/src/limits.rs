use std::num::NonZeroU64;

use thiserror::Error;

use crate::{MAX_STREAM_COUNT, VarInt};

/// Flow-control windows and stream counts, at construction in
/// [`transport::Config::limits`](crate::transport::Config::limits) or on a live connection through
/// [`Connection::set_limits`](crate::connection::Connection::set_limits), such as a relay widening
/// them once it knows the peer is another relay
///
/// Each limit is `None` to take the default at construction, or to leave it as it is on a live
/// connection. Each is non-zero because zero streams or a zero window credits nothing and wedges
/// the connection. Build one from [`Limits::default`] and set the fields to change.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Limits {
	/// Max concurrent streams the peer may open, applied to each direction: this many
	/// bidirectional and this many unidirectional
	pub max_streams: Option<NonZeroU64>,
	/// Connection-wide receive window, in bytes
	pub receive_window: Option<NonZeroU64>,
	/// Receive window of each stream, in bytes
	///
	/// Setting this smaller than `receive_window` helps ensure that a single stream doesn't
	/// monopolize receive buffers. Changed on a live connection, it applies as each stream's
	/// credit is next extended.
	pub stream_receive_window: Option<NonZeroU64>,
	/// Cap on unacknowledged outgoing data, in bytes
	pub send_window: Option<NonZeroU64>,
}

impl Limits {
	/// Refuse the limits a peer would reject: a stream count past 2^60, or a receive window
	/// past a QUIC varint
	pub fn validate(&self) -> Result<(), LimitsError> {
		let varint = |window: Option<NonZeroU64>| window.is_some_and(|window| VarInt::from_u64(window.get()).is_err());
		if self.max_streams.is_some_and(|streams| streams.get() > MAX_STREAM_COUNT) {
			return Err(LimitsError::MaxStreams);
		}
		if varint(self.receive_window) {
			return Err(LimitsError::ReceiveWindow);
		}
		if varint(self.stream_receive_window) {
			return Err(LimitsError::StreamReceiveWindow);
		}
		Ok(())
	}
}

/// Why [`Limits`] cannot be applied
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum LimitsError {
	/// The stream limit is past the 2^60 a peer accepts
	#[error("max_streams must be at most 2^60")]
	MaxStreams,
	/// The receive window does not fit a QUIC varint
	#[error("receive_window must fit a QUIC varint")]
	ReceiveWindow,
	/// The stream receive window does not fit a QUIC varint
	#[error("stream_receive_window must fit a QUIC varint")]
	StreamReceiveWindow,
}

#[cfg(test)]
mod tests {
	use super::*;

	fn limits(max_streams: u64, receive_window: u64, send_window: u64) -> Limits {
		Limits {
			max_streams: NonZeroU64::new(max_streams),
			receive_window: NonZeroU64::new(receive_window),
			stream_receive_window: NonZeroU64::new(receive_window),
			send_window: NonZeroU64::new(send_window),
		}
	}

	#[test]
	fn validate_accepts_the_wire_maximums() {
		assert_eq!(Limits::default().validate(), Ok(()));
		assert_eq!(
			limits(MAX_STREAM_COUNT, VarInt::MAX.into_inner(), u64::MAX).validate(),
			Ok(())
		);
	}

	#[test]
	fn validate_refuses_what_a_peer_would_reject() {
		assert_eq!(
			limits(MAX_STREAM_COUNT + 1, 1, 1).validate(),
			Err(LimitsError::MaxStreams)
		);
		assert_eq!(
			limits(1, VarInt::MAX.into_inner() + 1, 1).validate(),
			Err(LimitsError::ReceiveWindow)
		);
		let stream = Limits {
			stream_receive_window: NonZeroU64::new(VarInt::MAX.into_inner() + 1),
			..Limits::default()
		};
		assert_eq!(stream.validate(), Err(LimitsError::StreamReceiveWindow));
	}
}
