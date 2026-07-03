use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use bytes::Bytes;
use std::io::{Cursor, Write};

use crate::rml::messages::{MessageDeserializationError, MessageSerializationError};
use crate::rml::messages::{RtmpMessage, UserControlEventType};
use crate::rml::time::RtmpTimestamp;

pub fn serialize(
	event_type: UserControlEventType,
	stream_id: Option<u32>,
	buffer_length: Option<u32>,
	timestamp: Option<RtmpTimestamp>,
) -> Result<Bytes, MessageSerializationError> {
	let mut cursor = Cursor::new(Vec::new());
	match event_type {
		UserControlEventType::StreamBegin => write_stream_event(&mut cursor, 0, stream_id)?,
		UserControlEventType::StreamEof => write_stream_event(&mut cursor, 1, stream_id)?,
		UserControlEventType::StreamDry => write_stream_event(&mut cursor, 2, stream_id)?,
		UserControlEventType::SetBufferLength => write_length_event(&mut cursor, 3, stream_id, buffer_length)?,
		UserControlEventType::StreamIsRecorded => write_stream_event(&mut cursor, 4, stream_id)?,
		UserControlEventType::PingRequest => write_timestamp_event(&mut cursor, 6, timestamp)?,
		UserControlEventType::PingResponse => write_timestamp_event(&mut cursor, 7, timestamp)?,
		UserControlEventType::BufferEmpty => write_stream_event(&mut cursor, 31, stream_id)?,
		UserControlEventType::BufferReady => write_stream_event(&mut cursor, 32, stream_id)?,
	};

	let bytes = Bytes::from(cursor.into_inner());
	Ok(bytes)
}

pub fn deserialize(data: Bytes) -> Result<RtmpMessage, MessageDeserializationError> {
	let mut cursor = Cursor::new(data);
	let event_type = match cursor.read_u16::<BigEndian>()? {
		0 => UserControlEventType::StreamBegin,
		1 => UserControlEventType::StreamEof,
		2 => UserControlEventType::StreamDry,
		3 => UserControlEventType::SetBufferLength,
		4 => UserControlEventType::StreamIsRecorded,
		6 => UserControlEventType::PingRequest,
		7 => UserControlEventType::PingResponse,
		31 => UserControlEventType::BufferEmpty,
		32 => UserControlEventType::BufferReady,
		_ => {
			return Err(MessageDeserializationError::InvalidMessageFormat);
		}
	};

	let mut stream_id = None;
	let mut buffer_length = None;
	let mut timestamp = None;

	match event_type {
		UserControlEventType::StreamBegin => stream_id = Some(cursor.read_u32::<BigEndian>()?),
		UserControlEventType::StreamEof => stream_id = Some(cursor.read_u32::<BigEndian>()?),
		UserControlEventType::StreamDry => stream_id = Some(cursor.read_u32::<BigEndian>()?),
		UserControlEventType::StreamIsRecorded => stream_id = Some(cursor.read_u32::<BigEndian>()?),
		UserControlEventType::PingRequest => timestamp = Some(RtmpTimestamp::new(cursor.read_u32::<BigEndian>()?)),
		UserControlEventType::PingResponse => timestamp = Some(RtmpTimestamp::new(cursor.read_u32::<BigEndian>()?)),
		UserControlEventType::SetBufferLength => {
			stream_id = Some(cursor.read_u32::<BigEndian>()?);
			buffer_length = Some(cursor.read_u32::<BigEndian>()?);
		}
		UserControlEventType::BufferEmpty => stream_id = Some(cursor.read_u32::<BigEndian>()?),
		UserControlEventType::BufferReady => stream_id = Some(cursor.read_u32::<BigEndian>()?),
	}

	Ok(RtmpMessage::UserControl {
		event_type,
		stream_id,
		buffer_length,
		timestamp,
	})
}

fn write_stream_event<W: Write>(
	bytes: &mut W,
	event_id: u16,
	stream_id: Option<u32>,
) -> Result<(), MessageSerializationError> {
	debug_assert!(
		stream_id.is_some(),
		"Stream event attempted to be serialized with a None stream id!"
	);

	bytes.write_u16::<BigEndian>(event_id)?;
	match stream_id {
		Some(x) => bytes.write_u32::<BigEndian>(x)?,
		None => bytes.write_u32::<BigEndian>(0)?,
	};

	Ok(())
}

fn write_length_event<W: Write>(
	bytes: &mut W,
	event_id: u16,
	stream_id: Option<u32>,
	length: Option<u32>,
) -> Result<(), MessageSerializationError> {
	debug_assert!(
		stream_id.is_some(),
		"Buffer length event attempted to be serialized with a None stream id!"
	);
	debug_assert!(
		length.is_some(),
		"Buffer length event attempted to be serialized with a None length value!"
	);

	bytes.write_u16::<BigEndian>(event_id)?;
	match stream_id {
		Some(x) => bytes.write_u32::<BigEndian>(x)?,
		None => bytes.write_u32::<BigEndian>(0)?,
	};

	match length {
		Some(x) => bytes.write_u32::<BigEndian>(x)?,
		None => bytes.write_u32::<BigEndian>(0)?,
	};

	Ok(())
}

fn write_timestamp_event<W: Write>(
	bytes: &mut W,
	event_id: u16,
	timestamp: Option<RtmpTimestamp>,
) -> Result<(), MessageSerializationError> {
	debug_assert!(
		timestamp.is_some(),
		"Timestamp event attempted to be serialized with a None timestamp"
	);

	bytes.write_u16::<BigEndian>(event_id)?;
	match timestamp {
		Some(x) => bytes.write_u32::<BigEndian>(x.value)?,
		None => bytes.write_u32::<BigEndian>(0)?,
	};

	Ok(())
}

#[cfg(test)]
mod tests {
	use super::{deserialize, serialize};
	use byteorder::{BigEndian, WriteBytesExt};
	use bytes::Bytes;
	use std::io::Cursor;

	use crate::rml::messages::{RtmpMessage, UserControlEventType};
	use crate::rml::time::RtmpTimestamp;

	#[test]
	fn can_serialize_stream_begin_message() {
		let stream_id = 555;

		let mut cursor = Cursor::new(Vec::new());
		cursor.write_u16::<BigEndian>(0).unwrap();
		cursor.write_u32::<BigEndian>(stream_id).unwrap();
		let expected = cursor.into_inner();

		let raw_message = serialize(UserControlEventType::StreamBegin, Some(stream_id), None, None).unwrap();

		assert_eq!(&raw_message[..], &expected[..]);
	}

	#[test]
	fn can_serialize_stream_eof_message() {
		let stream_id = 555;

		let mut cursor = Cursor::new(Vec::new());
		cursor.write_u16::<BigEndian>(1).unwrap();
		cursor.write_u32::<BigEndian>(stream_id).unwrap();
		let expected = cursor.into_inner();

		let raw_message = serialize(UserControlEventType::StreamEof, Some(stream_id), None, None).unwrap();

		assert_eq!(&raw_message[..], &expected[..]);
	}

	#[test]
	fn can_serialize_stream_dry_message() {
		let stream_id = 555;

		let mut cursor = Cursor::new(Vec::new());
		cursor.write_u16::<BigEndian>(2).unwrap();
		cursor.write_u32::<BigEndian>(stream_id).unwrap();
		let expected = cursor.into_inner();

		let raw_message = serialize(UserControlEventType::StreamDry, Some(stream_id), None, None).unwrap();

		assert_eq!(&raw_message[..], &expected[..]);
	}

	#[test]
	fn can_serialize_set_buffer_length_message() {
		let stream_id = 555;
		let buffer_length = 666;

		let mut cursor = Cursor::new(Vec::new());
		cursor.write_u16::<BigEndian>(3).unwrap();
		cursor.write_u32::<BigEndian>(stream_id).unwrap();
		cursor.write_u32::<BigEndian>(buffer_length).unwrap();
		let expected = cursor.into_inner();

		let raw_message = serialize(
			UserControlEventType::SetBufferLength,
			Some(stream_id),
			Some(buffer_length),
			None,
		)
		.unwrap();

		assert_eq!(&raw_message[..], &expected[..]);
	}

	#[test]
	fn can_serialize_stream_is_recorded_message() {
		let stream_id = 555;

		let mut cursor = Cursor::new(Vec::new());
		cursor.write_u16::<BigEndian>(4).unwrap();
		cursor.write_u32::<BigEndian>(stream_id).unwrap();
		let expected = cursor.into_inner();

		let raw_message = serialize(UserControlEventType::StreamIsRecorded, Some(stream_id), None, None).unwrap();

		assert_eq!(&raw_message[..], &expected[..]);
	}

	#[test]
	fn can_serialize_ping_request_message() {
		let time = 555;

		let mut cursor = Cursor::new(Vec::new());
		cursor.write_u16::<BigEndian>(6).unwrap();
		cursor.write_u32::<BigEndian>(time).unwrap();
		let expected = cursor.into_inner();

		let raw_message = serialize(
			UserControlEventType::PingRequest,
			None,
			None,
			Some(RtmpTimestamp::new(time)),
		)
		.unwrap();

		assert_eq!(&raw_message[..], &expected[..]);
	}

	#[test]
	fn can_serialize_ping_response_message() {
		let time = 555;

		let mut cursor = Cursor::new(Vec::new());
		cursor.write_u16::<BigEndian>(7).unwrap();
		cursor.write_u32::<BigEndian>(time).unwrap();
		let expected = cursor.into_inner();

		let raw_message = serialize(
			UserControlEventType::PingResponse,
			None,
			None,
			Some(RtmpTimestamp::new(time)),
		)
		.unwrap();

		assert_eq!(&raw_message[..], &expected[..]);
	}

	#[test]
	fn can_serialize_buffer_emtpy_message() {
		let stream_id = 555;

		let mut cursor = Cursor::new(Vec::new());
		cursor.write_u16::<BigEndian>(31).unwrap();
		cursor.write_u32::<BigEndian>(stream_id).unwrap();
		let expected = cursor.into_inner();

		let raw_message = serialize(UserControlEventType::BufferEmpty, Some(stream_id), None, None).unwrap();

		assert_eq!(&raw_message[..], &expected[..]);
	}

	#[test]
	fn can_serialize_buffer_ready_message() {
		let stream_id = 555;

		let mut cursor = Cursor::new(Vec::new());
		cursor.write_u16::<BigEndian>(32).unwrap();
		cursor.write_u32::<BigEndian>(stream_id).unwrap();
		let expected = cursor.into_inner();

		let raw_message = serialize(UserControlEventType::BufferReady, Some(stream_id), None, None).unwrap();

		assert_eq!(&raw_message[..], &expected[..]);
	}

	#[test]
	fn can_deserialize_buffer_empty_message() {
		let stream_id = 555;
		let expected = RtmpMessage::UserControl {
			event_type: UserControlEventType::BufferEmpty,
			stream_id: Some(stream_id),
			buffer_length: None,
			timestamp: None,
		};

		let mut cursor = Cursor::new(Vec::new());
		cursor.write_u16::<BigEndian>(31).unwrap();
		cursor.write_u32::<BigEndian>(stream_id).unwrap();

		let data = Bytes::from(cursor.into_inner());
		let result = deserialize(data).unwrap();
		assert_eq!(result, expected);
	}

	#[test]
	fn can_deserialize_buffer_ready_message() {
		let stream_id = 555;
		let expected = RtmpMessage::UserControl {
			event_type: UserControlEventType::BufferReady,
			stream_id: Some(stream_id),
			buffer_length: None,
			timestamp: None,
		};

		let mut cursor = Cursor::new(Vec::new());
		cursor.write_u16::<BigEndian>(32).unwrap();
		cursor.write_u32::<BigEndian>(stream_id).unwrap();

		let data = Bytes::from(cursor.into_inner());
		let result = deserialize(data).unwrap();
		assert_eq!(result, expected);
	}

	#[test]
	fn can_deserialize_stream_begin_message() {
		let stream_id = 555;
		let expected = RtmpMessage::UserControl {
			event_type: UserControlEventType::StreamBegin,
			stream_id: Some(stream_id),
			buffer_length: None,
			timestamp: None,
		};

		let mut cursor = Cursor::new(Vec::new());
		cursor.write_u16::<BigEndian>(0).unwrap();
		cursor.write_u32::<BigEndian>(stream_id).unwrap();

		let data = Bytes::from(cursor.into_inner());
		let result = deserialize(data).unwrap();
		assert_eq!(result, expected);
	}

	#[test]
	fn can_deserialize_stream_eof_message() {
		let stream_id = 555;
		let expected = RtmpMessage::UserControl {
			event_type: UserControlEventType::StreamEof,
			stream_id: Some(stream_id),
			buffer_length: None,
			timestamp: None,
		};

		let mut cursor = Cursor::new(Vec::new());
		cursor.write_u16::<BigEndian>(1).unwrap();
		cursor.write_u32::<BigEndian>(stream_id).unwrap();

		let data = Bytes::from(cursor.into_inner());
		let result = deserialize(data).unwrap();
		assert_eq!(result, expected);
	}

	#[test]
	fn can_deserialize_stream_dry_message() {
		let stream_id = 555;
		let expected = RtmpMessage::UserControl {
			event_type: UserControlEventType::StreamDry,
			stream_id: Some(stream_id),
			buffer_length: None,
			timestamp: None,
		};

		let mut cursor = Cursor::new(Vec::new());
		cursor.write_u16::<BigEndian>(2).unwrap();
		cursor.write_u32::<BigEndian>(stream_id).unwrap();

		let data = Bytes::from(cursor.into_inner());
		let result = deserialize(data).unwrap();
		assert_eq!(result, expected);
	}

	#[test]
	fn can_deserialize_set_buffer_length_message() {
		let stream_id = 555;
		let buffer_length = 666;
		let expected = RtmpMessage::UserControl {
			event_type: UserControlEventType::SetBufferLength,
			stream_id: Some(stream_id),
			buffer_length: Some(buffer_length),
			timestamp: None,
		};

		let mut cursor = Cursor::new(Vec::new());
		cursor.write_u16::<BigEndian>(3).unwrap();
		cursor.write_u32::<BigEndian>(stream_id).unwrap();
		cursor.write_u32::<BigEndian>(buffer_length).unwrap();

		let data = Bytes::from(cursor.into_inner());
		let result = deserialize(data).unwrap();
		assert_eq!(result, expected);
	}

	#[test]
	fn can_deserialize_stream_is_recorded_message() {
		let stream_id = 555;
		let expected = RtmpMessage::UserControl {
			event_type: UserControlEventType::StreamIsRecorded,
			stream_id: Some(stream_id),
			buffer_length: None,
			timestamp: None,
		};

		let mut cursor = Cursor::new(Vec::new());
		cursor.write_u16::<BigEndian>(4).unwrap();
		cursor.write_u32::<BigEndian>(stream_id).unwrap();

		let data = Bytes::from(cursor.into_inner());
		let result = deserialize(data).unwrap();
		assert_eq!(result, expected);
	}

	#[test]
	fn can_deserialize_ping_request_message() {
		let time = 555;
		let expected = RtmpMessage::UserControl {
			event_type: UserControlEventType::PingRequest,
			stream_id: None,
			buffer_length: None,
			timestamp: Some(RtmpTimestamp::new(time)),
		};

		let mut cursor = Cursor::new(Vec::new());
		cursor.write_u16::<BigEndian>(6).unwrap();
		cursor.write_u32::<BigEndian>(time).unwrap();

		let data = Bytes::from(cursor.into_inner());
		let result = deserialize(data).unwrap();
		assert_eq!(result, expected);
	}

	#[test]
	fn can_deserialize_ping_response_message() {
		let time = 555;
		let expected = RtmpMessage::UserControl {
			event_type: UserControlEventType::PingResponse,
			stream_id: None,
			buffer_length: None,
			timestamp: Some(RtmpTimestamp::new(time)),
		};

		let mut cursor = Cursor::new(Vec::new());
		cursor.write_u16::<BigEndian>(7).unwrap();
		cursor.write_u32::<BigEndian>(time).unwrap();

		let data = Bytes::from(cursor.into_inner());
		let result = deserialize(data).unwrap();
		assert_eq!(result, expected);
	}
}
