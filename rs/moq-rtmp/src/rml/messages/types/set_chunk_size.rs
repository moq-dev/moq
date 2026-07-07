use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use bytes::Bytes;
use std::io::Cursor;

use crate::rml::messages::RtmpMessage;
use crate::rml::messages::{MessageDeserializationError, MessageSerializationError};

const MAX_SIZE: u32 = 0x80000000 - 1;

pub fn serialize(size: u32) -> Result<Bytes, MessageSerializationError> {
	if size > MAX_SIZE {
		return Err(MessageSerializationError::InvalidChunkSize);
	}

	let mut cursor = Cursor::new(Vec::new());
	cursor.write_u32::<BigEndian>(size)?;

	let bytes = Bytes::from(cursor.into_inner());
	Ok(bytes)
}

pub fn deserialize(data: Bytes) -> Result<RtmpMessage, MessageDeserializationError> {
	let mut cursor = Cursor::new(data);
	let size = cursor.read_u32::<BigEndian>()?;

	if size > MAX_SIZE {
		return Err(MessageDeserializationError::InvalidMessageFormat);
	}

	Ok(RtmpMessage::SetChunkSize { size })
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
		let size = 523;

		let mut cursor = Cursor::new(Vec::new());
		cursor.write_u32::<BigEndian>(size).unwrap();
		let expected = cursor.into_inner();

		let raw_message = serialize(size).unwrap();

		assert_eq!(&raw_message[..], &expected[..]);
	}

	#[test]
	fn can_deserialize_message() {
		let size = 532;
		let mut cursor = Cursor::new(Vec::new());
		cursor.write_u32::<BigEndian>(size).unwrap();

		let bytes = Bytes::from(cursor.into_inner());
		let result = deserialize(bytes).unwrap();
		let expected = RtmpMessage::SetChunkSize { size };
		assert_eq!(result, expected);
	}
}
