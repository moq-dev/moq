use crate::rml::messages::RtmpMessage;
use crate::rml::messages::{MessageDeserializationError, MessageSerializationError};
use bytes::Bytes;

pub fn serialize(bytes: Bytes) -> Result<Bytes, MessageSerializationError> {
	Ok(bytes)
}

pub fn deserialize(data: Bytes) -> Result<RtmpMessage, MessageDeserializationError> {
	Ok(RtmpMessage::VideoData { data })
}

#[cfg(test)]
mod tests {
	use super::{deserialize, serialize};
	use crate::rml::messages::RtmpMessage;
	use bytes::Bytes;

	#[test]
	fn can_serialize_message() {
		let expected = vec![1, 2, 3, 4];
		let raw_message = serialize(Bytes::from(vec![1, 2, 3, 4])).unwrap();

		assert_eq!(&raw_message[..], &expected[..]);
	}

	#[test]
	fn can_deserialize_message() {
		let data = Bytes::from(vec![1, 2, 3, 4]);
		let expected = RtmpMessage::VideoData {
			data: Bytes::from(vec![1, 2, 3, 4]),
		};

		let result = deserialize(data).unwrap();
		assert_eq!(result, expected);
	}
}
