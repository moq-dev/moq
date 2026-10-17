use std::num::NonZeroU64;

use thiserror::Error;

use crate::{MAX_STREAM_COUNT, VarInt};

/// Flow-control limits an established connection can change, such as a relay widening them
/// once it knows the peer is another relay
///
/// Each limit is `None` to leave it as it is. Each is non-zero because zero streams or a zero
/// window credits nothing and wedges the connection. Build one from [`Limits::default`] and
/// set the fields to change.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Limits {
    /// Max concurrent streams the peer may open, per direction
    pub max_streams: Option<NonZeroU64>,
    /// Connection-wide receive window, in bytes
    pub receive_window: Option<NonZeroU64>,
    /// Cap on unacknowledged outgoing data, in bytes
    pub send_window: Option<NonZeroU64>,
}

impl Limits {
    /// Refuse the limits a peer would reject: a stream count past 2^60, or a receive window
    /// past a QUIC varint
    pub fn validate(&self) -> Result<(), LimitsError> {
        if self
            .max_streams
            .is_some_and(|streams| streams.get() > MAX_STREAM_COUNT)
        {
            return Err(LimitsError::MaxStreams);
        }
        if self
            .receive_window
            .is_some_and(|window| VarInt::from_u64(window.get()).is_err())
        {
            return Err(LimitsError::ReceiveWindow);
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(max_streams: u64, receive_window: u64, send_window: u64) -> Limits {
        Limits {
            max_streams: NonZeroU64::new(max_streams),
            receive_window: NonZeroU64::new(receive_window),
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
    }
}
