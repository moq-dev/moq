use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use bytes::Bytes;
use std::io::Cursor;

use crate::rml::messages::{MessageDeserializationError, MessageSerializationError};
use crate::rml::messages::{PeerBandwidthLimitType, RtmpMessage};

pub fn serialize(limit_type: PeerBandwidthLimitType, size: u32) -> Result<Bytes, MessageSerializationError> {
	let type_id = match limit_type {
		PeerBandwidthLimitType::Hard => 0,
		PeerBandwidthLimitType::Soft => 1,
		PeerBandwidthLimitType::Dynamic => 2,
	};

	let mut cursor = Cursor::new(Vec::new());
	cursor.write_u32::<BigEndian>(size)?;
	cursor.write_u8(type_id)?;

	let bytes = Bytes::from(cursor.into_inner());
	Ok(bytes)
}

pub fn deserialize(data: Bytes) -> Result<RtmpMessage, MessageDeserializationError> {
	let mut cursor = Cursor::new(data);
	let size = cursor.read_u32::<BigEndian>()?;
	let limit_type = match cursor.read_u8()? {
		0 => PeerBandwidthLimitType::Hard,
		1 => PeerBandwidthLimitType::Soft,
		2 => PeerBandwidthLimitType::Dynamic,
		_ => {
			return Err(MessageDeserializationError::InvalidMessageFormat);
		}
	};

	Ok(RtmpMessage::SetPeerBandwidth { size, limit_type })
}

#[cfg(test)]
mod tests {
	use super::{deserialize, serialize};
	use byteorder::{BigEndian, WriteBytesExt};
	use bytes::Bytes;
	use std::io::Cursor;

	use crate::rml::messages::{PeerBandwidthLimitType, RtmpMessage};

	#[test]
	fn can_serialize_message_with_soft_limit_type() {
		let size = 523;

		let mut cursor = Cursor::new(Vec::new());
		cursor.write_u32::<BigEndian>(size).unwrap();
		cursor.write_u8(1).unwrap();
		let expected = cursor.into_inner();

		let raw_message = serialize(PeerBandwidthLimitType::Soft, size).unwrap();
		assert_eq!(&raw_message[..], &expected[..]);
	}

	#[test]
	fn can_serialize_message_with_hard_limit_type() {
		let size = 523;

		let mut cursor = Cursor::new(Vec::new());
		cursor.write_u32::<BigEndian>(size).unwrap();
		cursor.write_u8(0).unwrap();
		let expected = cursor.into_inner();

		let raw_message = serialize(PeerBandwidthLimitType::Hard, size).unwrap();
		assert_eq!(&raw_message[..], &expected[..]);
	}

	#[test]
	fn can_serialize_message_with_dynamic_limit_type() {
		let size = 523;

		let mut cursor = Cursor::new(Vec::new());
		cursor.write_u32::<BigEndian>(size).unwrap();
		cursor.write_u8(2).unwrap();
		let expected = cursor.into_inner();

		let raw_message = serialize(PeerBandwidthLimitType::Dynamic, size).unwrap();
		assert_eq!(&raw_message[..], &expected[..]);
	}

	#[test]
	fn can_deserialize_message_with_hard_limit_type() {
		let size = 523;
		let expected = RtmpMessage::SetPeerBandwidth {
			size,
			limit_type: PeerBandwidthLimitType::Hard,
		};

		let mut cursor = Cursor::new(Vec::new());
		cursor.write_u32::<BigEndian>(size).unwrap();
		cursor.write_u8(0).unwrap();

		let data = Bytes::from(cursor.into_inner());
		let result = deserialize(data).unwrap();
		assert_eq!(result, expected);
	}

	#[test]
	fn can_deserialize_message_with_soft_limit_type() {
		let size = 523;
		let expected = RtmpMessage::SetPeerBandwidth {
			size,
			limit_type: PeerBandwidthLimitType::Soft,
		};

		let mut cursor = Cursor::new(Vec::new());
		cursor.write_u32::<BigEndian>(size).unwrap();
		cursor.write_u8(1).unwrap();

		let data = Bytes::from(cursor.into_inner());
		let result = deserialize(data).unwrap();
		assert_eq!(result, expected);
	}

	#[test]
	fn can_deserialize_message_with_dynamic_limit_type() {
		let size = 523;
		let expected = RtmpMessage::SetPeerBandwidth {
			size,
			limit_type: PeerBandwidthLimitType::Dynamic,
		};

		let mut cursor = Cursor::new(Vec::new());
		cursor.write_u32::<BigEndian>(size).unwrap();
		cursor.write_u8(2).unwrap();

		let data = Bytes::from(cursor.into_inner());
		let result = deserialize(data).unwrap();
		assert_eq!(result, expected);
	}
}
