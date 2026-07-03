use super::chunk_header::{ChunkHeader, ChunkHeaderFormat};
use crate::rml::chunk_io::ChunkSerializationError;
use crate::rml::messages::{MessagePayload, RtmpMessage};
use crate::rml::time::RtmpTimestamp;
use byteorder::{BigEndian, LittleEndian, WriteBytesExt};
use std::cmp::min;
use std::collections::HashMap;
use std::io::{Cursor, Write};

const INITIAL_MAX_CHUNK_SIZE: u32 = 128;
const MAX_INITIAL_TIMESTAMP: u32 = 16777215;

/// An outbound data packet containing the at least one RTMP chunk with a single RTMP message.
/// The packet can be flagged as droppable because video and audio packets may be allowed to be
/// dropped if there is not enough bandwidth for the current bitrate.  This allows live video
/// to be kept in real time and to prevent getting backed up when redistributing live video when
/// the network conditions don't allow the current bitrate.
#[derive(Debug, PartialEq)]
pub struct Packet {
	pub bytes: Vec<u8>,
	pub can_be_dropped: bool,
}

/// Allows serializing RTMP messages into RTMP chunks.
///
/// Due to the nature of the RTMP chunking protocol, the same serializer should be used
/// for all messages that need to be sent to the same peer.
pub struct ChunkSerializer {
	previous_headers: HashMap<u32, ChunkHeader>,
	max_chunk_size: u32,
}

impl ChunkSerializer {
	/// Creates a new `ChunkSerializer`.
	///
	/// By default (per the RTMP specification) the serializer will break any message into RTMP
	/// chunks with a max size of 128.  To change this amount a call to `set_max_chunk_size()` is
	/// required.
	pub fn new() -> ChunkSerializer {
		ChunkSerializer {
			max_chunk_size: INITIAL_MAX_CHUNK_SIZE,
			previous_headers: HashMap::new(),
		}
	}

	/// Changes the maximum amount of bytes from RTMP messages that can be in a single RTMP chunk.
	///
	/// Changing the maximum chunk size requires notifying the receiver of the change, as it will
	/// affect every chunk you send out from here on out.  Therefore, when this method is called
	/// we automatically serialize a `SetChunkSize` RTMP message to be sent to the peer.  This
	/// packet *must* be sent and cannot be ignored.
	pub fn set_max_chunk_size(
		&mut self,
		new_size: u32,
		time: RtmpTimestamp,
	) -> Result<Packet, ChunkSerializationError> {
		if new_size > 2147483647 {
			return Err(ChunkSerializationError::InvalidMaxChunkSize {
				attempted_chunk_size: new_size,
			});
		}

		let set_chunk_size_message = RtmpMessage::SetChunkSize { size: new_size };
		let message_payload = MessagePayload::from_rtmp_message(set_chunk_size_message, time, 0)?;
		let packet = self.serialize(&message_payload, true, false)?;

		self.max_chunk_size = new_size;
		Ok(packet)
	}

	/// Turns an RTMP message payload into binary data (representing RTMP chunks) that can be
	/// sent over the network.
	///
	/// The RTMP chunk format has a basic form of header compression it utilizes.  If a chunk
	/// is sent with some header information, and the next chunk to be generated has a lot of
	/// similar header information, than the subsequent chunk can ommit some information and flag
	/// itself as requiring information from the previous chunk.
	///
	/// This compression can be bypassed by setting `force_uncompressed` to `true`.  This is
	/// required in certain circumstances, as some encoders or video players require the initial
	/// RTMP messages (after the handshake) to always be type 0 chunks (uncompressed).  The reason
	/// for this is unclear, but in these circumstances the clients or servers will not work
	/// properly without it.
	///
	/// If the message to be serialized is a video or audio data message, and it's not a a/v header,
	/// then it can be safe to set `can_be_dropped` to `true`.  This will mark the packet so that
	/// the network transport mechanisms can make a decision if the packet should be dropped (if
	/// there's not enough bandwidth to keep the stream in real time) or if it should be enqueued
	/// even if backlogged.   Setting this to `true` makes sure that if the packet is dropped that
	/// the receiver will not have deserialization problems on any subsequent RTMP chunks.
	pub fn serialize(
		&mut self,
		message: &MessagePayload,
		force_uncompressed: bool,
		can_be_dropped: bool,
	) -> Result<Packet, ChunkSerializationError> {
		if message.data.len() > 16777215 {
			return Err(ChunkSerializationError::MessageTooLong {
				size: message.data.len() as u32,
			});
		}

		let mut bytes = Cursor::new(Vec::new());

		// Since a message may have a payload greater than one chunk allows, we must
		// split the payload into slices that don't exceed the max chunk length
		let mut slices = Vec::<&[u8]>::new();
		let mut iteration = 0;
		loop {
			let start_index = iteration * self.max_chunk_size as usize;
			if start_index >= message.data.len() {
				break;
			}

			let remaining_length = message.data.len() - start_index;
			let end_index = min(
				start_index + self.max_chunk_size as usize,
				start_index + remaining_length,
			);

			slices.push(&message.data[start_index..end_index]);

			iteration = iteration + 1;
		}

		for (idx, slice) in slices.into_iter().enumerate() {
			self.add_chunk(&mut bytes, force_uncompressed, message, idx > 0, slice, can_be_dropped)?;
		}

		Ok(Packet {
			bytes: bytes.into_inner(),
			can_be_dropped,
		})
	}

	fn add_chunk(
		&mut self,
		bytes: &mut Cursor<Vec<u8>>,
		force_uncompressed: bool,
		message: &MessagePayload,
		continued_chunk: bool,
		data_to_write: &[u8],
		can_be_dropped: bool,
	) -> Result<(), ChunkSerializationError> {
		let mut header = ChunkHeader {
			chunk_stream_id: get_csid_for_message_type(message.type_id),
			timestamp: message.timestamp,
			timestamp_field: 0,
			message_type_id: message.type_id,
			message_stream_id: message.message_stream_id,
			message_length: message.data.len() as u32,
			can_be_dropped,
		};

		let header_format = if force_uncompressed {
			ChunkHeaderFormat::Full
		} else {
			match self.previous_headers.get(&header.chunk_stream_id) {
				None => ChunkHeaderFormat::Full,
				Some(ref previous_header) => {
					if continued_chunk {
						//  https://github.com/melpon/rfc/blob/master/rtmp.md#53124-type-3
						//  Continued chunks should use Format Type 3.
						//  Streaming into Twitch was breaking when a payload exceeded the max chunk size
						//  Continued chunks may add extended timestamp, set timestamp field as previous timestamp_field
						header.timestamp_field = previous_header.timestamp_field;
						ChunkHeaderFormat::Empty
					} else if previous_header.can_be_dropped {
						// If the previous packet was able to be dropped, we don't know if it was (or will be)
						// therefore the next packet must be a type 0 chunk as a precaution.  Otherwise
						// we risk the peer not being able to deserialize this packet.
						ChunkHeaderFormat::Full
					} else {
						// TODO: Update to support rtmp time wrap-around
						header.timestamp_field = (header.timestamp - previous_header.timestamp).value;
						get_header_format(&mut header, previous_header)
					}
				}
			}
		};

		if header_format == ChunkHeaderFormat::Full {
			header.timestamp_field = header.timestamp.value;
		}

		add_basic_header(bytes, &header_format, header.chunk_stream_id)?;
		add_initial_timestamp(bytes, &header_format, &header)?;
		add_message_length_and_type_id(bytes, &header_format, header.message_length, header.message_type_id)?;
		add_message_stream_id(bytes, &header_format, header.message_stream_id)?;
		add_extended_timestamp(bytes, &header)?;
		add_message_payload(bytes, data_to_write)?;

		self.previous_headers.insert(header.chunk_stream_id, header);
		Ok(())
	}
}

fn add_basic_header(
	bytes: &mut dyn Write,
	format: &ChunkHeaderFormat,
	csid: u32,
) -> Result<(), ChunkSerializationError> {
	if csid <= 1 || csid >= 65600 {
		panic!(
			"Attempted to serialize an RTMP chunk with a csid of {}, but only csids between 2 and 65600 are allowed",
			csid
		);
	}

	let format_mask = match *format {
		ChunkHeaderFormat::Full => 0b00000000,
		ChunkHeaderFormat::TimeDeltaWithoutMessageStreamId => 0b01000000,
		ChunkHeaderFormat::TimeDeltaOnly => 0b10000000,
		ChunkHeaderFormat::Empty => 0b11000000,
	};

	let mut first_byte = match csid {
		x if x <= 63 => x as u8,
		x if x >= 64 && x <= 319 => 0,
		_ => 1,
	};

	first_byte = first_byte | format_mask;
	bytes.write_u8(first_byte)?;

	// Since get_csid_for_message_type only does csids up to 6, ignore 2 and 3 byte csid formats
	Ok(())
}

fn add_initial_timestamp(
	bytes: &mut Cursor<Vec<u8>>,
	format: &ChunkHeaderFormat,
	header: &ChunkHeader,
) -> Result<(), ChunkSerializationError> {
	if *format == ChunkHeaderFormat::Empty {
		return Ok(());
	}

	let capped_value = min(header.timestamp_field, MAX_INITIAL_TIMESTAMP);
	bytes.write_u24::<BigEndian>(capped_value)?;

	Ok(())
}

fn add_message_length_and_type_id(
	bytes: &mut Cursor<Vec<u8>>,
	format: &ChunkHeaderFormat,
	length: u32,
	type_id: u8,
) -> Result<(), ChunkSerializationError> {
	if *format == ChunkHeaderFormat::Empty || *format == ChunkHeaderFormat::TimeDeltaOnly {
		return Ok(());
	}

	bytes.write_u24::<BigEndian>(length)?;
	bytes.write_u8(type_id)?;
	Ok(())
}

fn add_message_stream_id(
	bytes: &mut dyn Write,
	format: &ChunkHeaderFormat,
	stream_id: u32,
) -> Result<(), ChunkSerializationError> {
	if *format != ChunkHeaderFormat::Full {
		return Ok(());
	}

	bytes.write_u32::<LittleEndian>(stream_id)?;
	Ok(())
}

fn add_extended_timestamp(bytes: &mut dyn Write, header: &ChunkHeader) -> Result<(), ChunkSerializationError> {
	if header.timestamp_field < MAX_INITIAL_TIMESTAMP {
		return Ok(());
	}

	bytes.write_u32::<BigEndian>(header.timestamp_field)?;
	Ok(())
}

fn add_message_payload(bytes: &mut dyn Write, data: &[u8]) -> Result<(), ChunkSerializationError> {
	bytes.write_all(data)?;
	Ok(())
}

fn get_csid_for_message_type(message_type_id: u8) -> u32 {
	// Naive resolution, purpose (afaik) is to allow repeated messages
	// to utilize header compression by spreading them across chunk streams
	match message_type_id {
		1 | 2 | 3 | 4 | 5 | 6 => 2,
		18 | 19 => 3,
		9 => 4,
		8 => 5,
		_ => 6,
	}
}

fn get_header_format(current_header: &mut ChunkHeader, previous_header: &ChunkHeader) -> ChunkHeaderFormat {
	if current_header.message_stream_id != previous_header.message_stream_id {
		return ChunkHeaderFormat::Full;
	}

	if current_header.message_type_id != previous_header.message_type_id
		|| current_header.message_length != previous_header.message_length
	{
		return ChunkHeaderFormat::TimeDeltaWithoutMessageStreamId;
	}

	if current_header.timestamp_field != previous_header.timestamp_field {
		return ChunkHeaderFormat::TimeDeltaOnly;
	}

	ChunkHeaderFormat::Empty
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::rml::time::RtmpTimestamp;
	use byteorder::{BigEndian, LittleEndian, ReadBytesExt};
	use bytes::Bytes;
	use std::io::{Cursor, Read};

	#[test]
	fn type_0_chunk_for_first_message_with_small_timestamp() {
		let message1 = MessagePayload {
			timestamp: RtmpTimestamp::new(72),
			type_id: 50,
			message_stream_id: 12,
			data: Bytes::from(vec![1_u8, 2_u8, 3_u8, 4_u8]),
		};

		let mut serializer = ChunkSerializer::new();
		let packet = serializer.serialize(&message1, false, false).unwrap();

		let mut cursor = Cursor::new(packet.bytes);
		assert_eq!(cursor.read_u8().unwrap(), 6 | 0b00000000, "Unexpected csid value");
		assert_eq!(
			cursor.read_u24::<BigEndian>().unwrap(),
			72,
			"Unexpected timestamp value"
		);
		assert_eq!(
			cursor.read_u24::<BigEndian>().unwrap(),
			4,
			"Unexpected message length value"
		);
		assert_eq!(cursor.read_u8().unwrap(), 50, "Unexpected type id");
		assert_eq!(
			cursor.read_u32::<LittleEndian>().unwrap(),
			12,
			"Unexpected message stream id"
		);

		let mut payload_bytes = [0_u8; 50];
		let bytes_read = cursor.read(&mut payload_bytes[..]).unwrap();
		assert_eq!(bytes_read, 4, "Unexpected payload bytes read");
		assert_eq!(
			&payload_bytes[..bytes_read],
			&message1.data[..],
			"Unexpected payload contents"
		);
	}

	#[test]
	fn type_0_chunk_for_first_message_with_extended_timestamp() {
		let message1 = MessagePayload {
			timestamp: RtmpTimestamp::new(16777216),
			type_id: 50,
			message_stream_id: 12,
			data: Bytes::from(vec![1_u8, 2_u8, 3_u8, 4_u8]),
		};

		let mut serializer = ChunkSerializer::new();
		let packet = serializer.serialize(&message1, false, false).unwrap();

		let mut cursor = Cursor::new(packet.bytes);
		assert_eq!(cursor.read_u8().unwrap(), 6 | 0b00000000, "Unexpected csid value");
		assert_eq!(
			cursor.read_u24::<BigEndian>().unwrap(),
			16777215,
			"Unexpected timestamp value"
		);
		assert_eq!(
			cursor.read_u24::<BigEndian>().unwrap(),
			4,
			"Unexpected message length value"
		);
		assert_eq!(cursor.read_u8().unwrap(), 50, "Unexpected type id");
		assert_eq!(
			cursor.read_u32::<LittleEndian>().unwrap(),
			12,
			"Unexpected message stream id"
		);
		assert_eq!(
			cursor.read_u32::<BigEndian>().unwrap(),
			16777216,
			"Unexpected extended timestamp"
		);

		let mut payload_bytes = [0_u8; 50];
		let bytes_read = cursor.read(&mut payload_bytes[..]).unwrap();
		assert_eq!(bytes_read, 4, "Unexpected payload bytes read");
		assert_eq!(
			&payload_bytes[..bytes_read],
			[1_u8, 2_u8, 3_u8, 4_u8],
			"Unexpected payload contents"
		);
	}

	#[test]
	fn type_1_chunk_for_second_message_with_same_stream_id_and_different_message_length_and_different_type_id_and_small_timestamp()
	 {
		let message1 = MessagePayload {
			timestamp: RtmpTimestamp::new(72),
			type_id: 50,
			message_stream_id: 12,
			data: Bytes::from(vec![1_u8, 2_u8, 3_u8, 4_u8]),
		};

		let message2 = MessagePayload {
			timestamp: RtmpTimestamp::new(82),
			type_id: 51,
			message_stream_id: 12,
			data: Bytes::from(vec![1_u8, 2_u8, 3_u8]),
		};

		let mut serializer = ChunkSerializer::new();
		let _ = serializer.serialize(&message1, false, false).unwrap();
		let packet = serializer.serialize(&message2, false, false).unwrap();

		let mut cursor = Cursor::new(packet.bytes);
		assert_eq!(cursor.read_u8().unwrap(), 6 | 0b01000000, "Unexpected csid value");
		assert_eq!(
			cursor.read_u24::<BigEndian>().unwrap(),
			10,
			"Unexpected timestamp value"
		);
		assert_eq!(
			cursor.read_u24::<BigEndian>().unwrap(),
			3,
			"Unexpected message length value"
		);
		assert_eq!(cursor.read_u8().unwrap(), 51, "Unexpected type id");

		let mut payload_bytes = [0_u8; 50];
		let bytes_read = cursor.read(&mut payload_bytes[..]).unwrap();
		assert_eq!(bytes_read, 3, "Unexpected payload bytes read");
		assert_eq!(
			&payload_bytes[..bytes_read],
			&[1_u8, 2_u8, 3_u8],
			"Unexpected payload contents"
		);
	}

	#[test]
	fn type_1_chunk_for_second_message_with_same_stream_id_and_different_message_length_and_different_type_id_and_extended_timestamp()
	 {
		let message1 = MessagePayload {
			timestamp: RtmpTimestamp::new(10),
			type_id: 50,
			message_stream_id: 12,
			data: Bytes::from(vec![1_u8, 2_u8, 3_u8, 4_u8]),
		};

		let message2 = MessagePayload {
			timestamp: RtmpTimestamp::new(16777226),
			type_id: 51,
			message_stream_id: 12,
			data: Bytes::from(vec![1_u8, 2_u8, 3_u8]),
		};

		let mut serializer = ChunkSerializer::new();
		let _ = serializer.serialize(&message1, false, false).unwrap();
		let packet = serializer.serialize(&message2, false, false).unwrap();

		let mut cursor = Cursor::new(packet.bytes);
		assert_eq!(cursor.read_u8().unwrap(), 6 | 0b01000000, "Unexpected csid value");
		assert_eq!(
			cursor.read_u24::<BigEndian>().unwrap(),
			16777215,
			"Unexpected timestamp value"
		);
		assert_eq!(
			cursor.read_u24::<BigEndian>().unwrap(),
			3,
			"Unexpected message length value"
		);
		assert_eq!(cursor.read_u8().unwrap(), 51, "Unexpected type id");
		assert_eq!(
			cursor.read_u32::<BigEndian>().unwrap(),
			16777216,
			"Unexpected extended timestamp"
		);

		let mut payload_bytes = [0_u8; 50];
		let bytes_read = cursor.read(&mut payload_bytes[..]).unwrap();
		assert_eq!(bytes_read, 3, "Unexpected payload bytes read");
		assert_eq!(
			&payload_bytes[..bytes_read],
			&[1_u8, 2_u8, 3_u8],
			"Unexpected payload contents"
		);
	}

	#[test]
	fn type_2_chunk_for_second_message_with_same_stream_id_and_same_message_length_and_same_type_id_and_small_timestamp()
	 {
		let message1 = MessagePayload {
			timestamp: RtmpTimestamp::new(72),
			type_id: 50,
			message_stream_id: 12,
			data: Bytes::from(vec![1_u8, 2_u8, 3_u8, 4_u8]),
		};

		let message2 = MessagePayload {
			timestamp: RtmpTimestamp::new(82),
			type_id: 50,
			message_stream_id: 12,
			data: Bytes::from(vec![5_u8, 6_u8, 7_u8, 8_u8]),
		};

		let mut serializer = ChunkSerializer::new();
		let _ = serializer.serialize(&message1, false, false).unwrap();
		let packet = serializer.serialize(&message2, false, false).unwrap();

		let mut cursor = Cursor::new(packet.bytes);
		assert_eq!(cursor.read_u8().unwrap(), 6 | 0b10000000, "Unexpected csid value");
		assert_eq!(
			cursor.read_u24::<BigEndian>().unwrap(),
			10,
			"Unexpected timestamp value"
		);

		let mut payload_bytes = [0_u8; 50];
		let bytes_read = cursor.read(&mut payload_bytes[..]).unwrap();
		assert_eq!(bytes_read, 4, "Unexpected payload bytes read");
		assert_eq!(
			&payload_bytes[..bytes_read],
			&[5_u8, 6_u8, 7_u8, 8_u8],
			"Unexpected payload contents"
		);
	}

	#[test]
	fn type_2_chunk_for_second_message_with_same_stream_id_and_same_message_length_and_same_type_id_and_extended_timestamp()
	 {
		let message1 = MessagePayload {
			timestamp: RtmpTimestamp::new(10),
			type_id: 50,
			message_stream_id: 12,
			data: Bytes::from(vec![1_u8, 2_u8, 3_u8, 4_u8]),
		};

		let message2 = MessagePayload {
			timestamp: RtmpTimestamp::new(16777226),
			type_id: 50,
			message_stream_id: 12,
			data: Bytes::from(vec![5_u8, 6_u8, 7_u8, 8_u8]),
		};

		let mut serializer = ChunkSerializer::new();
		let _ = serializer.serialize(&message1, false, false).unwrap();
		let packet = serializer.serialize(&message2, false, false).unwrap();

		let mut cursor = Cursor::new(packet.bytes);
		assert_eq!(cursor.read_u8().unwrap(), 6 | 0b10000000, "Unexpected csid value");
		assert_eq!(
			cursor.read_u24::<BigEndian>().unwrap(),
			16777215,
			"Unexpected timestamp value"
		);
		assert_eq!(
			cursor.read_u32::<BigEndian>().unwrap(),
			16777216,
			"Unexpected extended timestamp"
		);

		let mut payload_bytes = [0_u8; 50];
		let bytes_read = cursor.read(&mut payload_bytes[..]).unwrap();
		assert_eq!(bytes_read, 4, "Unexpected payload bytes read");
		assert_eq!(
			&payload_bytes[..bytes_read],
			&[5_u8, 6_u8, 7_u8, 8_u8],
			"Unexpected payload contents"
		);
	}

	#[test]
	fn type_3_chunk_for_third_message_with_all_matching_details() {
		let message1 = MessagePayload {
			timestamp: RtmpTimestamp::new(72),
			type_id: 50,
			message_stream_id: 12,
			data: Bytes::from(vec![1_u8, 2_u8, 3_u8, 4_u8]),
		};

		let message2 = MessagePayload {
			timestamp: RtmpTimestamp::new(82),
			type_id: 50,
			message_stream_id: 12,
			data: Bytes::from(vec![5_u8, 6_u8, 7_u8, 8_u8]),
		};

		let message3 = MessagePayload {
			timestamp: RtmpTimestamp::new(92),
			type_id: 50,
			message_stream_id: 12,
			data: Bytes::from(vec![9_u8, 10_u8, 11_u8, 12_u8]),
		};

		let mut serializer = ChunkSerializer::new();
		let _ = serializer.serialize(&message1, false, false).unwrap();
		let _ = serializer.serialize(&message2, false, false).unwrap();
		let packet = serializer.serialize(&message3, false, false).unwrap();

		let mut cursor = Cursor::new(packet.bytes);
		assert_eq!(cursor.read_u8().unwrap(), 6 | 0b11000000, "Unexpected csid value");

		let mut payload_bytes = [0_u8; 50];
		let bytes_read = cursor.read(&mut payload_bytes[..]).unwrap();
		assert_eq!(bytes_read, 4, "Unexpected payload bytes read");
		assert_eq!(
			&payload_bytes[..bytes_read],
			&[9_u8, 10_u8, 11_u8, 12_u8],
			"Unexpected payload contents"
		);
	}

	#[test]
	fn type_0_chunks_used_when_new_message_on_different_csid_serialized() {
		let message1 = MessagePayload {
			timestamp: RtmpTimestamp::new(72),
			type_id: 50,
			message_stream_id: 12,
			data: Bytes::from(vec![1_u8, 2_u8, 3_u8, 4_u8]),
		};

		let message2 = MessagePayload {
			timestamp: RtmpTimestamp::new(82),
			type_id: 1,
			message_stream_id: 12,
			data: Bytes::from(vec![6_u8, 7_u8, 8_u8, 9_u8]),
		};

		let mut serializer = ChunkSerializer::new();
		let _ = serializer.serialize(&message1, false, false).unwrap();
		let packet = serializer.serialize(&message2, false, false).unwrap();

		let mut cursor = Cursor::new(packet.bytes);
		assert_eq!(cursor.read_u8().unwrap(), 2 | 0b00000000, "Unexpected csid value");
		assert_eq!(
			cursor.read_u24::<BigEndian>().unwrap(),
			82,
			"Unexpected timestamp value"
		);
		assert_eq!(
			cursor.read_u24::<BigEndian>().unwrap(),
			4,
			"Unexpected message length value"
		);
		assert_eq!(cursor.read_u8().unwrap(), 1, "Unexpected type id");
		assert_eq!(
			cursor.read_u32::<LittleEndian>().unwrap(),
			12,
			"Unexpected message stream id"
		);

		let mut payload_bytes = [0_u8; 50];
		let bytes_read = cursor.read(&mut payload_bytes[..]).unwrap();
		assert_eq!(bytes_read, 4, "Unexpected payload bytes read");
		assert_eq!(
			&payload_bytes[..bytes_read],
			&[6_u8, 7_u8, 8_u8, 9_u8],
			"Unexpected payload contents"
		);
	}

	#[test]
	fn type_0_chunk_for_second_message_when_forcing_uncompressed() {
		let message1 = MessagePayload {
			timestamp: RtmpTimestamp::new(72),
			type_id: 50,
			message_stream_id: 12,
			data: Bytes::from(vec![1_u8, 2_u8, 3_u8, 4_u8]),
		};

		let message2 = MessagePayload {
			timestamp: RtmpTimestamp::new(82),
			type_id: 50,
			message_stream_id: 12,
			data: Bytes::from(vec![5_u8, 6_u8, 7_u8, 8_u8]),
		};

		let mut serializer = ChunkSerializer::new();
		let _ = serializer.serialize(&message1, false, false).unwrap();
		let packet = serializer.serialize(&message2, true, false).unwrap();

		let mut cursor = Cursor::new(packet.bytes);
		assert_eq!(cursor.read_u8().unwrap(), 6 | 0b00000000, "Unexpected csid value");
		assert_eq!(
			cursor.read_u24::<BigEndian>().unwrap(),
			82,
			"Unexpected timestamp value"
		);
		assert_eq!(
			cursor.read_u24::<BigEndian>().unwrap(),
			4,
			"Unexpected message length value"
		);
		assert_eq!(cursor.read_u8().unwrap(), 50, "Unexpected type id");
		assert_eq!(
			cursor.read_u32::<LittleEndian>().unwrap(),
			12,
			"Unexpected message stream id"
		);

		let mut payload_bytes = [0_u8; 50];
		let bytes_read = cursor.read(&mut payload_bytes[..]).unwrap();
		assert_eq!(bytes_read, 4, "Unexpected payload bytes read");
		assert_eq!(
			&payload_bytes[..bytes_read],
			&[5_u8, 6_u8, 7_u8, 8_u8],
			"Unexpected payload contents"
		);
	}

	#[test]
	fn message_split_when_payload_exceeds_max_chunk_size() {
		let mut payload = Vec::new();
		payload.extend_from_slice(&[11_u8; 75]);
		payload.extend_from_slice(&[22_u8; 25]);

		let message1 = MessagePayload {
			timestamp: RtmpTimestamp::new(72),
			type_id: 50,
			message_stream_id: 12,
			data: Bytes::from(payload.clone()),
		};

		let message2 = MessagePayload {
			timestamp: RtmpTimestamp::new(73),
			type_id: 50,
			message_stream_id: 12,
			data: Bytes::from(payload),
		};

		let mut serializer = ChunkSerializer::new();
		serializer.set_max_chunk_size(75, RtmpTimestamp::new(0)).unwrap();

		let packet = serializer.serialize(&message1, false, false).unwrap();

		let mut cursor = Cursor::new(packet.bytes);
		assert_eq!(cursor.read_u8().unwrap(), 6 | 0b00000000, "Unexpected csid value");
		assert_eq!(
			cursor.read_u24::<BigEndian>().unwrap(),
			72,
			"Unexpected timestamp value"
		);
		assert_eq!(
			cursor.read_u24::<BigEndian>().unwrap(),
			100,
			"Unexpected message length value"
		);
		assert_eq!(cursor.read_u8().unwrap(), 50, "Unexpected type id");
		assert_eq!(
			cursor.read_u32::<LittleEndian>().unwrap(),
			12,
			"Unexpected message stream id"
		);

		let mut payload_bytes = [0_u8; 75];
		let bytes_read = cursor.read(&mut payload_bytes[..]).unwrap();
		assert_eq!(bytes_read, 75, "Unexpected payload bytes read");
		assert_eq!(
			&payload_bytes[..bytes_read],
			&([11_u8; 75])[..],
			"Unexpected payload contents"
		);

		assert_eq!(cursor.read_u8().unwrap(), 6 | 0b11000000, "Unexpected 2nd csid value");
		let bytes_read = cursor.read(&mut payload_bytes[..]).unwrap();
		assert_eq!(bytes_read, 25, "Unexpected 2nd payload bytes read");
		assert_eq!(
			&payload_bytes[..bytes_read],
			&([22_u8; 25])[..],
			"Unexpected 2nd payload contents"
		);

		let packet = serializer.serialize(&message2, false, false).unwrap();
		let mut cursor = Cursor::new(packet.bytes);
		assert_eq!(cursor.read_u8().unwrap(), 6 | 0b10000000, "Unexpected csid value");
		assert_eq!(cursor.read_u24::<BigEndian>().unwrap(), 1, "Unexpected timestamp value");

		let mut payload_bytes = [0_u8; 75];
		let bytes_read = cursor.read(&mut payload_bytes[..]).unwrap();
		assert_eq!(bytes_read, 75, "Unexpected payload bytes read");
		assert_eq!(
			&payload_bytes[..bytes_read],
			&([11_u8; 75])[..],
			"Unexpected payload contents"
		);

		assert_eq!(cursor.read_u8().unwrap(), 6 | 0b11000000, "Unexpected chunk format");
		let bytes_read = cursor.read(&mut payload_bytes[..]).unwrap();
		assert_eq!(bytes_read, 25, "Unexpected 2nd payload bytes read");
		assert_eq!(
			&payload_bytes[..bytes_read],
			&([22_u8; 25])[..],
			"Unexpected 2nd payload contents"
		);
	}

	#[test]
	fn message_split_extended_timestamp() {
		let mut payload = Vec::new();
		payload.extend_from_slice(&[11_u8; 75]);
		payload.extend_from_slice(&[22_u8; 25]);

		let timestamp_value = MAX_INITIAL_TIMESTAMP + 1;
		let message1 = MessagePayload {
			timestamp: RtmpTimestamp::new(timestamp_value),
			type_id: 50,
			message_stream_id: 12,
			data: Bytes::from(payload.clone()),
		};

		let mut serializer = ChunkSerializer::new();
		serializer.set_max_chunk_size(75, RtmpTimestamp::new(0)).unwrap();

		let packet = serializer.serialize(&message1, false, false).unwrap();

		let mut cursor = Cursor::new(packet.bytes);
		assert_eq!(cursor.read_u8().unwrap(), 6 | 0b00000000, "Unexpected csid value");
		assert_eq!(
			cursor.read_u24::<BigEndian>().unwrap(),
			MAX_INITIAL_TIMESTAMP,
			"Unexpected timestamp value"
		);
		assert_eq!(
			cursor.read_u24::<BigEndian>().unwrap(),
			100,
			"Unexpected message length value"
		);
		assert_eq!(cursor.read_u8().unwrap(), 50, "Unexpected type id");
		assert_eq!(
			cursor.read_u32::<LittleEndian>().unwrap(),
			12,
			"Unexpected message stream id"
		);
		assert_eq!(
			cursor.read_u32::<BigEndian>().unwrap(),
			timestamp_value,
			"Unexpected extended timestamp value"
		);

		let mut payload_bytes = [0_u8; 75];
		let bytes_read = cursor.read(&mut payload_bytes[..]).unwrap();
		assert_eq!(bytes_read, 75, "Unexpected payload bytes read");
		assert_eq!(
			&payload_bytes[..bytes_read],
			&([11_u8; 75])[..],
			"Unexpected payload contents"
		);

		assert_eq!(cursor.read_u8().unwrap(), 6 | 0b11000000, "Unexpected 2nd csid value");
		assert_eq!(
			cursor.read_u32::<BigEndian>().unwrap(),
			timestamp_value,
			"Unexpected extended timestamp value on second chunk"
		);
		let bytes_read = cursor.read(&mut payload_bytes[..]).unwrap();
		assert_eq!(bytes_read, 25, "Unexpected 2nd payload bytes read");
		assert_eq!(
			&payload_bytes[..bytes_read],
			&([22_u8; 25])[..],
			"Unexpected 2nd payload contents"
		);
	}

	#[test]
	fn changing_size_returns_set_chunk_size_outbound_message() {
		let mut serializer = ChunkSerializer::new();
		let packet = serializer.set_max_chunk_size(75, RtmpTimestamp::new(152)).unwrap();

		let mut cursor = Cursor::new(packet.bytes);
		assert_eq!(cursor.read_u8().unwrap(), 2 | 0b00000000, "Unexpected csid value");
		assert_eq!(cursor.read_u24::<BigEndian>().unwrap(), 152, "Unexpected timestamp");
		assert_eq!(
			cursor.read_u24::<BigEndian>().unwrap(),
			4,
			"Unexpected message length value"
		);
		assert_eq!(cursor.read_u8().unwrap(), 1, "Unexpected type id");
		assert_eq!(
			cursor.read_u32::<LittleEndian>().unwrap(),
			0,
			"Unexpected message stream id"
		);
		assert_eq!(cursor.read_u32::<BigEndian>().unwrap(), 75, "Unexpected chunk size");
	}

	#[test]
	fn type_0_chunk_comes_after_droppable_packet() {
		let message1 = MessagePayload {
			timestamp: RtmpTimestamp::new(72),
			type_id: 50,
			message_stream_id: 12,
			data: Bytes::from(vec![1_u8, 2_u8, 3_u8, 4_u8]),
		};

		let message2 = MessagePayload {
			timestamp: RtmpTimestamp::new(82),
			type_id: 50,
			message_stream_id: 12,
			data: Bytes::from(vec![1_u8, 2_u8, 3_u8, 4_u8]),
		};

		let mut serializer = ChunkSerializer::new();
		let packet1 = serializer.serialize(&message1, false, true).unwrap();

		let mut cursor = Cursor::new(packet1.bytes);
		assert_eq!(cursor.read_u8().unwrap(), 6 | 0b00000000, "Unexpected csid value");
		assert_eq!(
			cursor.read_u24::<BigEndian>().unwrap(),
			72,
			"Unexpected timestamp value"
		);
		assert_eq!(
			cursor.read_u24::<BigEndian>().unwrap(),
			4,
			"Unexpected message length value"
		);
		assert_eq!(cursor.read_u8().unwrap(), 50, "Unexpected type id");
		assert_eq!(
			cursor.read_u32::<LittleEndian>().unwrap(),
			12,
			"Unexpected message stream id"
		);
		assert_eq!(
			packet1.can_be_dropped, true,
			"First packet was expected to be droppable"
		);

		let mut payload_bytes = [0_u8; 50];
		let bytes_read = cursor.read(&mut payload_bytes[..]).unwrap();
		assert_eq!(bytes_read, 4, "Unexpected payload bytes read");
		assert_eq!(
			&payload_bytes[..bytes_read],
			&message1.data[..],
			"Unexpected payload contents"
		);

		let packet2 = serializer.serialize(&message2, false, false).unwrap();
		let mut cursor = Cursor::new(packet2.bytes);
		assert_eq!(cursor.read_u8().unwrap(), 6 | 0b00000000, "Unexpected 2nd csid value");
		assert_eq!(
			cursor.read_u24::<BigEndian>().unwrap(),
			82,
			"Unexpected 2nd timestamp value"
		);
		assert_eq!(
			cursor.read_u24::<BigEndian>().unwrap(),
			4,
			"Unexpected 2nd message length value"
		);
		assert_eq!(cursor.read_u8().unwrap(), 50, "Unexpected 2nd type id");
		assert_eq!(
			cursor.read_u32::<LittleEndian>().unwrap(),
			12,
			"Unexpected 2nd message stream id"
		);
		assert_eq!(
			packet2.can_be_dropped, false,
			"Second packet was not expected to be droppable"
		);

		let mut payload_bytes = [0_u8; 50];
		let bytes_read = cursor.read(&mut payload_bytes[..]).unwrap();
		assert_eq!(bytes_read, 4, "Unexpected payload bytes read");
		assert_eq!(
			&payload_bytes[..bytes_read],
			&message1.data[..],
			"Unexpected payload contents"
		);
	}
}
