use std::{io, string};
use thiserror::Error;

/// Errors that can occur during the deserialization process
#[derive(Debug, Error)]
pub enum Amf0DeserializationError {
	/// Container nesting exceeds the decoder limit.
	#[error("AMF0 nesting exceeds 64 levels")]
	MaxDepthExceeded,

	/// Every Amf0 value starts with a marker byte describing the type of value that was
	/// encoded.  For example a marker of `0x00` is a number, `0x01` is a string, etc..
	///
	/// This error is encountered when we see a maker value that we do not recognize.
	#[error("Encountered unknown marker: {marker}")]
	UnknownMarker { marker: u8 },

	/// Object properties consist of a name and value pair.  It is expected that every property
	/// has a valid string name, and if the name is empty this error is raised.
	#[error("Unexpected empty object property name")]
	UnexpectedEmptyObjectPropertyName,

	/// This occurs when we are expecting more data but hit the end of the buffer
	/// (e.g. we are reading an object property but there was no property value).
	#[error("Hit end of the byte buffer but was expecting more data")]
	UnexpectedEof,

	/// An I/O Error occurred while reading the data buffer
	#[error("Failed to read byte buffer: {0}")]
	BufferReadError(#[from] io::Error),

	/// Strings in AMF0 are UTF-8 encoded, so if the bytes read are not valid
	/// UTF-8 this error will be raised.
	#[error("Failed to read a utf8 string from the byte buffer: {0}")]
	StringParseError(#[from] string::FromUtf8Error),
}

/// Errors raised during to the serialization process
#[derive(Debug, Error)]
pub enum Amf0SerializationError {
	/// Amf0 strings cannot be more than 65,535 characters, so if a string was provided
	/// with a larger length than this than this error is raised.
	#[error("String length greater than 65,535")]
	NormalStringTooLong,

	/// An I/O error occurred while writing to the output buffer.
	#[error("Failed to write to byte buffer")]
	BufferWriteError(#[from] io::Error),
}
