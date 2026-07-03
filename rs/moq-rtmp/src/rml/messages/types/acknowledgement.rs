use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use bytes::Bytes;
use std::io::Cursor;

use crate::rml::messages::RtmpMessage;
use crate::rml::messages::{MessageDeserializationError, MessageSerializationError};

pub fn serialize(sequence_number: u32) -> Result<Bytes, MessageSerializationError> {
	let mut cursor = Cursor::new(Vec::new());
	cursor.write_u32::<BigEndian>(sequence_number)?;

	let bytes = Bytes::from(cursor.into_inner());
	Ok(bytes)
}

pub fn deserialize(data: Bytes) -> Result<RtmpMessage, MessageDeserializationError> {
	let mut cursor = Cursor::new(data);

	Ok(RtmpMessage::Acknowledgement {
		sequence_number: cursor.read_u32::<BigEndian>()?,
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
		let number = 523;
		let result = serialize(number).unwrap();

		let mut cursor = Cursor::new(Vec::new());
		cursor.write_u32::<BigEndian>(number).unwrap();

		assert_eq!(&cursor.into_inner()[..], &result[..]);
	}

	#[test]
	fn can_deserialize_message() {
		let number = 532;
		let mut cursor = Cursor::new(Vec::new());
		cursor.write_u32::<BigEndian>(number).unwrap();

		let bytes = Bytes::from(cursor.into_inner());
		let result = deserialize(bytes).unwrap();

		let expected = RtmpMessage::Acknowledgement {
			sequence_number: number,
		};
		assert_eq!(expected, result);
	}
}
