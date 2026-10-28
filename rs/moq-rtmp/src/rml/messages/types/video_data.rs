use crate::rml::messages::RtmpMessage;
use crate::rml::messages::{MessageDeserializationError, MessageSerializationError};
use bytes::Bytes;

pub fn serialize(bytes: Bytes) -> Result<Bytes, MessageSerializationError> {
	Ok(bytes)
}

pub fn deserialize(data: Bytes) -> Result<RtmpMessage, MessageDeserializationError> {
	Ok(RtmpMessage::VideoData { data })
}
