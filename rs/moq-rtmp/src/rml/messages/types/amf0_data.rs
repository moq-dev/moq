use crate::rml::amf0;
use crate::rml::amf0::Amf0Value;
use bytes::Bytes;
use std::io::Cursor;

use crate::rml::messages::RtmpMessage;
use crate::rml::messages::{MessageDeserializationError, MessageSerializationError};

pub fn serialize(values: Vec<Amf0Value>) -> Result<Bytes, MessageSerializationError> {
	let bytes = amf0::serialize(&values)?;

	Ok(Bytes::from(bytes))
}

pub fn deserialize(data: Bytes) -> Result<RtmpMessage, MessageDeserializationError> {
	let mut cursor = Cursor::new(data);
	let values = amf0::deserialize(&mut cursor)?;

	Ok(RtmpMessage::Amf0Data { values })
}

#[cfg(test)]
mod tests {
	use super::{deserialize, serialize};
	use crate::rml::amf0;
	use crate::rml::amf0::Amf0Value;
	use bytes::Bytes;
	use std::io::Cursor;

	use crate::rml::messages::RtmpMessage;

	#[test]
	fn can_serialize_message() {
		let raw_message = serialize(vec![Amf0Value::Boolean(true), Amf0Value::Number(52.0)]).unwrap();

		let mut cursor = Cursor::new(raw_message);
		let result = amf0::deserialize(&mut cursor).unwrap();
		let expected = vec![Amf0Value::Boolean(true), Amf0Value::Number(52.0)];

		assert_eq!(&expected[..], &result[..]);
	}

	#[test]
	fn can_deserialize_message() {
		let values = vec![Amf0Value::Boolean(true), Amf0Value::Number(52.0)];
		let bytes = Bytes::from(amf0::serialize(&values).unwrap());
		let result = deserialize(bytes).unwrap();

		let expected = RtmpMessage::Amf0Data {
			values: vec![Amf0Value::Boolean(true), Amf0Value::Number(52.0)],
		};

		assert_eq!(expected, result);
	}
}
