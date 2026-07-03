//! Errors that can occur during the handshaking process

use std::io;
use thiserror::Error;

/// Data pertaining to errors that occurred during the handshaking process.
/// Enumeration that represents the various errors that can occur during the handshaking process
#[derive(Error, Debug)]
pub enum HandshakeError {
	/// The RTMP specification requires the first byte in the handshake process to start with a
	/// 3, so this error is encountered if any other value is in the first byte.
	#[error("First byte of the handshake did not start with a 3")]
	BadVersionId,

	/// The RTMP specification requires the 2nd set of 4 bytes to all be zeroes, so this error
	/// is encountered if any of those values are not zeros.
	#[error("Packet 1's 2nd time field was expected to be empty, but wasn't")]
	NonZeroedTimeInPacket1,

	/// This is encountered when the peer did not send the same timestamp in packet #2 that we
	/// sent them in our packet #1.
	#[error("Peer did not send the correct time back")]
	IncorrectPeerTime,

	/// This is encountered when the peer did not send back the same random data in their packet
	/// number 2 that we sent them in our packet number 1.
	#[error("Peer did not send the correct random data back")]
	IncorrectRandomData,

	/// This is encountered if we try to keep progressing on a handshake handler that has already
	/// completed a successful handshake.
	#[error("Attempted to continue handshake process after completing handshake")]
	HandshakeAlreadyCompleted,

	/// The packet 1 we receive may be one of two formats (digest at position 8 or 772).  There
	/// is no known way to know which one to expect, so both are tested for.  This error is returned
	/// when packet 1 does not match either of the messages (and most likely is a bad handshake).
	#[error("No known message format could be determined from the received packet 1")]
	UnknownPacket1Format,

	/// This occurs when the incoming p2 did not either contain an exact copy of the p1 we sent
	/// (old handshake) or the hmac signature did not match (digest handshake).
	#[error("Invalid handshake packet 2 received")]
	InvalidP2Packet,

	/// This occurs when an IO error is encountered while reading the input.
	#[error("{0}")]
	Io(#[from] io::Error),
}
