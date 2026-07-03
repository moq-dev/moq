use crate::rml::messages::MessageSerializationError;
use thiserror::Error;

use std::io;

/// Data for when an error occurs while attempting to serialize an RTMP message into RTMP chunks.
/// An enumeration defining all the possible errors that could occur while serializing
/// RTMP messages into RTMP chunks.
#[derive(Debug, Error)]
pub enum ChunkSerializationError {
	/// Te RTMP specification states that a message cannot be more than 16,777,215 bytes, even
	/// when split across multiple RTMP chunks.  This error is returned if an RTMP message is passed
	/// in that is larger than this amount.
	#[error("The current message has a length of {size} bytes, which is over the allowed size of 16777215 bytes")]
	MessageTooLong { size: u32 },

	/// The RTMP spec does not allow chunk sizes more than 2,147,483,647 (since it's encoded in only
	/// 31 bytes of the SetChunkSize message), so this error occurs when a chunk size of greater than
	/// this value is attempted to be set
	#[error("An invalid chunk size was specified.  Chunk size must be greater than 0 and less than 2147483647")]
	InvalidMaxChunkSize { attempted_chunk_size: u32 },

	/// An I/O error occurred while writing the output buffer
	#[error("{0}")]
	Io(#[from] io::Error),

	/// Occurs when an error is returned when trying to create a set chunk size message
	#[error("Failed to create SetChunkSize message: {0}")]
	SetChunkSizeMessageCreationFailure(#[from] MessageSerializationError),
}
