use std::io;
use thiserror::Error;

/// Data for when an error occurs while attempting to deserialize a RTMP chunk
/// An enumeration defining all the possible errors that could occur while deserializing
/// RTMP chunks.
#[derive(Debug, Error)]
pub enum ChunkDeserializationError {
	/// The RTMP chunk format requires that RTMP chunks that are not type 0 utilize information
	/// from the previously received chunk on that same chunk stream id.  This error occurs when a
	/// non-0 chunk is received on a stream that has not received a type 0 chunk yet.
	#[error("Received chunk with non-zero chunk type on csid {csid} prior to receiving a type 0 chunk")]
	NoPreviousChunkOnStream { csid: u32 },

	/// Chunk sizes must fit in 31 bits and be at least one byte.
	#[error("Requested an invalid max chunk size of {chunk_size}. Expected 1 through 2147483647")]
	InvalidMaxChunkSize { chunk_size: usize },

	/// An I/O error occurred while reading the input buffer
	#[error("{0}")]
	Io(#[from] io::Error),
}
