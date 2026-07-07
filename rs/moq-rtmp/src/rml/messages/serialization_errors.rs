use rml_amf0::Amf0SerializationError;
use thiserror::Error;

use std::io;

/// Error state when serialization errors occur
/// Enumeration that represents the various errors that may occur while trying to
/// serialize a RTMP message into a raw RTMP payload.
#[derive(Debug, Error)]
pub enum MessageSerializationError {
	/// An invalid chunk size value was provided
	#[error("Cannot serialize a SetChunkSize message with a size of 2147483648 or greater")]
	InvalidChunkSize,

	/// The values provided could not be serialized into valid AMF0 encoded data
	#[error("The values provided could not be serialized into valid AMF0 encoded data")]
	Amf0SerializationError(#[from] Amf0SerializationError),

	/// Failed to read the values from the input buffer
	#[error("An IO error occurred while writing the output")]
	Io(#[from] io::Error),
}
