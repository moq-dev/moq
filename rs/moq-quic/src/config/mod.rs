use thiserror::Error;

use crate::LimitsError;

pub mod client;
pub(crate) mod endpoint;
pub mod server;
pub mod transport;

/// Errors in the configuration of an endpoint or a connection
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConfigError {
	/// The flow-control limits are ones a peer would refuse
	#[error("invalid limits: {0}")]
	Limits(#[from] LimitsError),
	/// The idle timeout does not fit a QUIC varint in milliseconds
	#[error("idle timeout out of range")]
	IdleTimeout,
	/// An MTU is outside 1200 to 65527 bytes
	#[error("mtu out of range")]
	Mtu,
	/// The maximum UDP payload size is outside 1200 to 65527 bytes
	#[error("max UDP payload size out of range")]
	MaxUdpPayloadSize,
}
