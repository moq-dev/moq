use crate::rml::messages::RtmpMessage;
use crate::rml::messages::{MessageDeserializationError, MessageSerializationError};
use bytes::Bytes;

pub fn serialize(data: Bytes) -> Result<Bytes, MessageSerializationError> {
	Ok(data)
}

pub fn deserialize(data: Bytes) -> Result<RtmpMessage, MessageDeserializationError> {
	Ok(RtmpMessage::AudioData { data })
}
