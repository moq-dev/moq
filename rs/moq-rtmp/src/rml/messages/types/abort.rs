use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use bytes::Bytes;
use std::io::Cursor;

use crate::rml::messages::RtmpMessage;
use crate::rml::messages::{MessageDeserializationError, MessageSerializationError};

pub fn serialize(stream_id: u32) -> Result<Bytes, MessageSerializationError> {
	let mut cursor = Cursor::new(Vec::new());
	cursor.write_u32::<BigEndian>(stream_id)?;

	let bytes = Bytes::from(cursor.into_inner());
	Ok(bytes)
}

pub fn deserialize(data: Bytes) -> Result<RtmpMessage, MessageDeserializationError> {
	let mut cursor = Cursor::new(data);

	Ok(RtmpMessage::Abort {
		stream_id: cursor.read_u32::<BigEndian>()?,
	})
}

#[cfg(test)]
mod tests {
	use super::{deserialize, serialize};
	use byteorder::{BigEndian, WriteBytesExt};
	use bytes::Bytes;
	use std::io::Cursor;

	use crate::rml::messages::RtmpMessage;

	#[test]
	fn can_serialize_message() {
		let id = 523;
		let mut cursor = Cursor::new(Vec::new());
		cursor.write_u32::<BigEndian>(id).unwrap();
		let expected = cursor.into_inner();

		let raw_message = serialize(id).unwrap();

		assert_eq!(&raw_message[..], &expected[..]);
	}

	#[test]
	fn can_deserialize_message() {
		let id = 532;
		let expected = RtmpMessage::Abort { stream_id: id };

		let mut cursor = Cursor::new(Vec::new());
		cursor.write_u32::<BigEndian>(id).unwrap();

		let bytes = Bytes::from(cursor.into_inner());
		let result = deserialize(bytes).unwrap();
		assert_eq!(result, expected);
	}
}
