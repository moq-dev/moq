//! Shared protocol types and traits for str0m.

// Avoid rewriting existing code: nested conditions are often more readable than long if-chains.
#![allow(clippy::collapsible_if)]

/// Default target MTU when no MTU is explicitly configured via [`RtcConfig::set_mtu`].
pub const DATAGRAM_MTU_TARGET: usize = 1150;

/// Lower bound for the target MTU.
pub const DATAGRAM_MTU_TARGET_MIN: usize = 576;

/// Upper bound for the target MTU.
pub const DATAGRAM_MTU_TARGET_MAX: usize = 1500;

/// Default warning threshold for MTU when no warning threshold is configured via [`RtcConfig::set_mtu`].
pub const DATAGRAM_MTU_WARN: usize = 1280;

mod bandwidth;
pub use bandwidth::{Bitrate, DataSize};

mod rng;
pub use rng::NonCryptographicRng;

mod pii;
pub use pii::Pii;

mod id;
pub use id::Id;

mod net;
pub use net::{DatagramSend, ParseTcpTypeError, Protocol, TcpType, Transmit};

mod sha1;
pub use sha1::{CryptoSafe, Sha1HmacProvider};

#[cfg(feature = "dtls")]
pub mod crypto;
