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

	/// The connection has exhausted its retained chunk-stream header slots.
	#[error("RTMP connection exceeded 256 chunk stream IDs")]
	ChunkStreamLimit,

	/// The connection has exhausted its aggregate message reservation budget.
	#[error("RTMP connection exceeded 64 MiB of incomplete message payloads")]
	AssemblyLimit,

	/// Undecoded input exceeds the connection's buffer budget.
	#[error("RTMP connection exceeded 64 MiB of undecoded input")]
	InputLimit,

	/// A continuation changed an incomplete message's metadata.
	#[error("RTMP chunk changed an incomplete message on csid {csid}")]
	InconsistentMessage { csid: u32 },

	/// An extended timestamp delta was smaller than its 24-bit marker.
	#[error("RTMP extended timestamp {timestamp} is below 16777215")]
	InvalidExtendedTimestamp { timestamp: u32 },

	/// An I/O error occurred while reading the input buffer
	#[error("{0}")]
	Io(#[from] io::Error),
}
