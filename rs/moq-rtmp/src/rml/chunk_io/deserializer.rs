use super::chunk_header::{ChunkHeader, ChunkHeaderFormat};
use crate::rml::chunk_io::ChunkDeserializationError;
use crate::rml::messages::MessagePayload;
use byteorder::{BigEndian, LittleEndian, ReadBytesExt};
use bytes::BytesMut;
use std::cmp::min;
use std::collections::HashMap;
use std::io::Cursor;
use std::mem;

const INITIAL_MAX_CHUNK_SIZE: usize = 128;
const MAX_INITIAL_TIMESTAMP: u32 = 16777215;
// The gateway uses five CSIDs; retain ample room for control and media roles
// without allowing every wire ID to pin header history for the connection.
const MAX_CHUNK_STREAMS: usize = 256;
// Four maximal 24-bit messages fit. Charge declared lengths before allocating,
// so tiny initial chunks cannot pin unaccounted full-message reservations.
const MAX_ASSEMBLY_BYTES: usize = 64 * 1024 * 1024;
const MAX_INPUT_BYTES: usize = 64 * 1024 * 1024;

/// Allows deserializing bytes representing RTMP chunks into RTMP message payloads.
///
/// Due to the nature of the RTMP chunk protocol it is required that every byte going through the
/// wire is sent to the same `ChunkDeserializer` instance, as future chunks can rely on previous
/// chunks, so any chunks missing from the stream may cause deserialization errors.
pub struct ChunkDeserializer {
	max_chunk_size: usize,
	current_header_format: ChunkHeaderFormat,
	current_header: ChunkHeader,
	current_stage: ParseStage,
	assemblies: HashMap<u32, Assembly>,
	assembly_bytes: usize,
	buffer: BytesMut,
	previous_headers: HashMap<u32, ChunkHeader>,
}

struct Assembly {
	payload: MessagePayload,
	data: BytesMut,
	length: usize,
}

enum ParsedValue<T> {
	NotEnoughBytes,
	Value { val: T, next_index: u32 },
}

enum ParseStage {
	Csid,
	InitialTimestamp,
	MessageLength,
	MessageTypeId,
	MessageStreamId,
	MessagePayload,
	ExtendedTimestamp,
}

#[derive(Eq, PartialEq, Debug)]
enum ParseStageResult {
	Success,
	NotEnoughBytes,
}

impl ChunkDeserializer {
	/// Create a new `ChunkDeserializer` with its initial properties.
	///
	/// Per the RTMP specification an initial `ChunkDeserializer` is expecting RTMP chunks with
	/// a max size of 128 bytes.
	pub fn new() -> ChunkDeserializer {
		ChunkDeserializer {
			max_chunk_size: INITIAL_MAX_CHUNK_SIZE,
			current_header_format: ChunkHeaderFormat::Full,
			current_header: ChunkHeader::new(),
			current_stage: ParseStage::Csid,
			buffer: BytesMut::with_capacity(4096),
			previous_headers: HashMap::new(),
			assemblies: HashMap::new(),
			assembly_bytes: 0,
		}
	}

	/// Attempts to read a complete RTMP message from the passed in bytes.
	///
	/// It is normal that one set of bytes will not form a complete RTMP message (or even a
	/// complete RTMP chunk).  Therefore it can be assumed that the deserializer will store all
	/// partial message bytes passed into it and the same bytes should not be passed in repeatedly,
	/// otherwise deserialization errors will most likely occur.
	///
	/// If the bytes that were passed in did not form a complete RTMP message, then the bytes are
	/// added to an internal buffer for storage and `Ok(None)` is returned while it waits for
	/// the next `get_next_message()` call to complete the message.
	///
	/// If the bytes that were passed in formed multiple RTMP messages than only the first message
	/// is deserialized and any subsequent messages are not read until the next `get_next_message()`
	/// call.
	///
	/// This is important because if the peer sends a `SendChunkSize` message (meaning it will
	/// change the maximum size of RTMP chunks it sends) you must process that message and call
	/// the `set_max_chunk_size()` method prior to the next `get_next_message()` call.   Otherwise
	/// if the peer sends a chunk larger than the previous max chunk size the message will not be
	/// deserialized properly (and most likely errors will occur).
	///
	/// It is expected that consumers will call `get_next_message()` in a loop until `None` is
	/// returned.  Since it is important not to keep sending it the same bytes over and over again
	/// an empty slice must be passed in for subsequent calls.
	///
	/// ## Examples
	///
	/// ```ignore
	/// # extern crate bytes;
	/// # extern crate rml_rtmp;
	/// # use bytes::Bytes;
	/// # use rml_rtmp::time::RtmpTimestamp;
	/// # use rml_rtmp::chunk_io::{ChunkSerializer, ChunkDeserializer};
	/// # use rml_rtmp::messages::MessagePayload;
	/// # fn main() {
	/// let input1 = MessagePayload {
	///     timestamp: RtmpTimestamp::new(55),
	///     message_stream_id: 1,
	///     type_id: 15,
	///     data: Bytes::from(vec![1, 2, 3, 4, 5, 6]),
	/// };
	///
	/// let input2 = MessagePayload {
	///     timestamp: RtmpTimestamp::new(65),
	///     message_stream_id: 1,
	///     type_id: 15,
	///     data: Bytes::from(vec![8, 9, 10]),
	/// };
	///
	/// let input3 = MessagePayload {
	///     timestamp: RtmpTimestamp::new(75),
	///     message_stream_id: 1,
	///     type_id: 15,
	///     data: Bytes::from(vec![1, 2, 3]),
	/// };
	///
	/// let mut serializer = ChunkSerializer::new();
	/// let mut packet1 = serializer.serialize(&input1, false, false).unwrap();
	/// let mut packet2 = serializer.serialize(&input2, false, false).unwrap();
	/// let mut packet3 = serializer.serialize(&input3, false, false).unwrap();
	///
	/// let mut all_bytes = Vec::new();
	/// all_bytes.append(&mut packet1.bytes);
	/// all_bytes.append(&mut packet2.bytes);
	/// all_bytes.append(&mut packet3.bytes);
	///
	/// let mut deserializer = ChunkDeserializer::new();
	/// let message1 = deserializer.get_next_message(&all_bytes[..]).unwrap();
	/// let message2 = deserializer.get_next_message(&[]).unwrap();
	/// let message3 = deserializer.get_next_message(&[]).unwrap();
	/// let message4 = deserializer.get_next_message(&[]).unwrap();
	///
	/// assert_eq!(message1, Some(input1));
	/// assert_eq!(message2, Some(input2));
	/// assert_eq!(message3, Some(input3));
	/// assert_eq!(message4, None);
	/// # }
	/// ```
	pub fn get_next_message(&mut self, bytes: &[u8]) -> Result<Option<MessagePayload>, ChunkDeserializationError> {
		let result = self.read_message(bytes);
		if result.is_err() {
			// Errors terminate the connection. Release every other CSID's retained
			// state as well as this chunk, before returning through the session.
			*self = Self::new();
		}
		result
	}

	fn read_message(&mut self, bytes: &[u8]) -> Result<Option<MessagePayload>, ChunkDeserializationError> {
		if bytes.len() > MAX_INPUT_BYTES - self.buffer.len() {
			return Err(ChunkDeserializationError::InputLimit);
		}
		self.buffer.extend_from_slice(bytes);

		loop {
			let mut complete_message = None;
			let result = match self.current_stage {
				ParseStage::Csid => self.form_header()?,
				ParseStage::InitialTimestamp => self.get_initial_timestamp()?,
				ParseStage::MessageLength => self.get_message_length()?,
				ParseStage::MessageTypeId => self.get_message_type_id()?,
				ParseStage::MessageStreamId => self.get_message_stream_id()?,
				ParseStage::ExtendedTimestamp => self.get_extended_timestamp()?,
				ParseStage::MessagePayload => self.get_message_data(&mut complete_message)?,
			};

			if result == ParseStageResult::NotEnoughBytes || complete_message.is_some() {
				return Ok(complete_message);
			}
		}
	}

	/// Tells the deserializer that the peer will start sending RTMP chunks with a different
	/// max chunk size.
	///
	/// When an RTMP message is larger than the current max chunk size the serializer
	/// will split the message across multiple RTMP chunks, with each chunk only containing the number
	/// of bytes that fit into the max chunk size value.  Therefore, the sender and the receiver
	/// must be exactly in tune as to what max chunk size they are utilizing.  Any mismatch will
	/// cause errors in the deserialization process, as it will expect split chunks where there
	/// are noone, or encounter a split chunk where it wasn't expecting one.
	///
	/// This method should almost always be called only in reaction to receiving a `SetChunkSize`
	/// message from the other end.
	pub fn set_max_chunk_size(&mut self, new_size: usize) -> Result<(), ChunkDeserializationError> {
		if new_size == 0 || new_size > 2147483647 {
			return Err(ChunkDeserializationError::InvalidMaxChunkSize { chunk_size: new_size });
		}

		self.max_chunk_size = new_size;
		Ok(())
	}

	/// Returns the maximum size of any RTMP chunks that should be received
	pub fn get_max_chunk_size(&self) -> usize {
		self.max_chunk_size
	}

	/// Discard an aborted CSID's incomplete payload while retaining its header history.
	pub(crate) fn abort(&mut self, csid: u32) {
		if let Some(assembly) = self.assemblies.remove(&csid) {
			self.assembly_bytes -= assembly.length;
		}
	}

	fn form_header(&mut self) -> Result<ParseStageResult, ChunkDeserializationError> {
		if self.buffer.len() < 1 {
			return Ok(ParseStageResult::NotEnoughBytes);
		}

		self.current_header_format = get_format(&self.buffer[0]);
		let (csid, next_index) = match get_csid(&self.buffer[..]) {
			ParsedValue::NotEnoughBytes => return Ok(ParseStageResult::NotEnoughBytes),
			ParsedValue::Value { val, next_index } => (val, next_index),
		};

		if !self.previous_headers.contains_key(&csid) && self.previous_headers.len() >= MAX_CHUNK_STREAMS {
			return Err(ChunkDeserializationError::ChunkStreamLimit);
		}

		self.current_header = match self.current_header_format {
			ChunkHeaderFormat::Full => {
				let mut new_header = ChunkHeader::new();
				new_header.chunk_stream_id = csid;
				new_header
			}

			_ => match self.previous_headers.remove(&csid) {
				None => return Err(ChunkDeserializationError::NoPreviousChunkOnStream { csid }),
				Some(header) => header,
			},
		};

		let _ = self.buffer.split_to(next_index as usize);
		self.current_stage = ParseStage::InitialTimestamp;
		Ok(ParseStageResult::Success)
	}

	fn get_initial_timestamp(&mut self) -> Result<ParseStageResult, ChunkDeserializationError> {
		if self.current_header_format == ChunkHeaderFormat::Empty {
			// Some encoders send an empty header after a type 1 header due to a message split
			// across multiple chunks.  We need to be careful *NOT* to apply the delta to each
			// type 3 chunk that's trying to serve a single message, otherwise timestamps will
			// get out of control.
			if !self.assemblies.contains_key(&self.current_header.chunk_stream_id) {
				// Apply the inherited delta once, when this CSID starts a message.
				self.current_header.timestamp = self.current_header.timestamp + self.current_header.timestamp_field;
			}

			self.current_stage = ParseStage::MessageLength;
			return Ok(ParseStageResult::Success);
		}

		if self.buffer.len() < 3 {
			return Ok(ParseStageResult::NotEnoughBytes);
		}

		let timestamp;
		{
			let bytes = self.buffer.split_to(3);
			let mut cursor = Cursor::new(bytes);
			timestamp = cursor.read_u24::<BigEndian>()?;
		}

		if self.current_header_format == ChunkHeaderFormat::Full {
			self.current_header.timestamp.set(timestamp);
		} else {
			// Non full headers are deltas only
			self.current_header.timestamp = self.current_header.timestamp + timestamp;
		}

		//apply the timestamp field
		self.current_header.timestamp_field = timestamp;

		self.current_stage = ParseStage::MessageLength;
		Ok(ParseStageResult::Success)
	}

	fn get_message_length(&mut self) -> Result<ParseStageResult, ChunkDeserializationError> {
		if self.current_header_format == ChunkHeaderFormat::TimeDeltaOnly
			|| self.current_header_format == ChunkHeaderFormat::Empty
		{
			self.current_stage = ParseStage::MessageTypeId;
			return Ok(ParseStageResult::Success);
		}

		if self.buffer.len() < 3 {
			return Ok(ParseStageResult::NotEnoughBytes);
		}

		let length;
		{
			let bytes = self.buffer.split_to(3);
			let mut cursor = Cursor::new(bytes);
			length = cursor.read_u24::<BigEndian>()?;
		}

		self.current_header.message_length = length;
		self.current_stage = ParseStage::MessageTypeId;
		Ok(ParseStageResult::Success)
	}

	fn get_message_type_id(&mut self) -> Result<ParseStageResult, ChunkDeserializationError> {
		if self.current_header_format == ChunkHeaderFormat::TimeDeltaOnly
			|| self.current_header_format == ChunkHeaderFormat::Empty
		{
			self.current_stage = ParseStage::MessageStreamId;
			return Ok(ParseStageResult::Success);
		}

		if self.buffer.len() < 1 {
			return Ok(ParseStageResult::NotEnoughBytes);
		}

		self.current_header.message_type_id = self.buffer[0];
		let _ = self.buffer.split_to(1);
		self.current_stage = ParseStage::MessageStreamId;
		Ok(ParseStageResult::Success)
	}

	fn get_message_stream_id(&mut self) -> Result<ParseStageResult, ChunkDeserializationError> {
		if self.current_header_format != ChunkHeaderFormat::Full {
			self.current_stage = ParseStage::ExtendedTimestamp;
			return Ok(ParseStageResult::Success);
		}

		if self.buffer.len() < 4 {
			return Ok(ParseStageResult::NotEnoughBytes);
		}

		let stream_id;
		{
			let bytes = self.buffer.split_to(4);
			let mut cursor = Cursor::new(bytes);
			stream_id = cursor.read_u32::<LittleEndian>()?;
		}

		self.current_header.message_stream_id = stream_id;
		self.current_stage = ParseStage::ExtendedTimestamp;
		Ok(ParseStageResult::Success)
	}

	fn get_extended_timestamp(&mut self) -> Result<ParseStageResult, ChunkDeserializationError> {
		if self.current_header.timestamp_field < MAX_INITIAL_TIMESTAMP {
			self.current_stage = ParseStage::MessagePayload;
			return Ok(ParseStageResult::Success);
		}

		if self.buffer.len() < 4 {
			return Ok(ParseStageResult::NotEnoughBytes);
		}

		let timestamp;
		{
			let bytes = self.buffer.split_to(4);
			let mut cursor = Cursor::new(bytes);
			timestamp = cursor.read_u32::<BigEndian>()?;
		}

		// A continuation's type 3 timestamp does not advance the message timestamp.
		if self.current_header_format == ChunkHeaderFormat::Full {
			self.current_header.timestamp.set(timestamp);
		} else if !self.assemblies.contains_key(&self.current_header.chunk_stream_id) {
			// Since we already added the MAX_INITIAL_TIMESTAMP to the timestamp, only add the delta difference
			let delta = timestamp
				.checked_sub(MAX_INITIAL_TIMESTAMP)
				.ok_or(ChunkDeserializationError::InvalidExtendedTimestamp { timestamp })?;
			self.current_header.timestamp = self.current_header.timestamp + delta;
		}

		self.current_stage = ParseStage::MessagePayload;
		Ok(ParseStageResult::Success)
	}

	fn get_message_data(
		&mut self,
		message_to_return: &mut Option<MessagePayload>,
	) -> Result<ParseStageResult, ChunkDeserializationError> {
		let csid = self.current_header.chunk_stream_id;
		let length = self.current_header.message_length as usize;
		if let Some(assembly) = self.assemblies.get(&csid) {
			if assembly.length != length
				|| assembly.payload.type_id != self.current_header.message_type_id
				|| assembly.payload.message_stream_id != self.current_header.message_stream_id
				|| assembly.payload.timestamp != self.current_header.timestamp
			{
				return Err(ChunkDeserializationError::InconsistentMessage { csid });
			}
		} else {
			if length > MAX_ASSEMBLY_BYTES - self.assembly_bytes {
				return Err(ChunkDeserializationError::AssemblyLimit);
			}
			self.assembly_bytes += length;
			self.assemblies.insert(
				csid,
				Assembly {
					payload: MessagePayload {
						timestamp: self.current_header.timestamp,
						type_id: self.current_header.message_type_id,
						message_stream_id: self.current_header.message_stream_id,
						..MessagePayload::new()
					},
					data: BytesMut::with_capacity(length),
					length,
				},
			);
		}

		let assembly = self.assemblies.get_mut(&csid).unwrap();
		let remaining = length
			.checked_sub(assembly.data.len())
			.ok_or(ChunkDeserializationError::InconsistentMessage { csid })?;
		let chunk_length = min(remaining, self.max_chunk_size);
		if self.buffer.len() < chunk_length {
			return Ok(ParseStageResult::NotEnoughBytes);
		}

		let bytes = self.buffer.split_to(chunk_length);
		assembly.data.extend_from_slice(&bytes);
		if assembly.data.len() == length {
			let mut assembly = self.assemblies.remove(&csid).unwrap();
			self.assembly_bytes -= assembly.length;
			assembly.payload.data = assembly.data.freeze();
			*message_to_return = Some(assembly.payload);
		}

		// This completes the current chunk, so cycle the header into the map and start a new one
		let current_header = mem::replace(&mut self.current_header, ChunkHeader::new());
		self.previous_headers
			.insert(current_header.chunk_stream_id, current_header);
		self.current_stage = ParseStage::Csid;
		Ok(ParseStageResult::Success)
	}
}

fn get_format(byte: &u8) -> ChunkHeaderFormat {
	const TYPE_0_MASK: u8 = 0b00000000;
	const TYPE_1_MASK: u8 = 0b01000000;
	const TYPE_2_MASK: u8 = 0b10000000;
	const FORMAT_MASK: u8 = 0b11000000;

	let format_id = *byte & FORMAT_MASK;

	match format_id {
		TYPE_0_MASK => ChunkHeaderFormat::Full,
		TYPE_1_MASK => ChunkHeaderFormat::TimeDeltaWithoutMessageStreamId,
		TYPE_2_MASK => ChunkHeaderFormat::TimeDeltaOnly,
		_ => ChunkHeaderFormat::Empty,
	}
}

fn get_csid(buffer: &[u8]) -> ParsedValue<u32> {
	const CSID_MASK: u8 = 0b00111111;

	if buffer.len() < 1 {
		return ParsedValue::NotEnoughBytes;
	}

	match buffer[0] & CSID_MASK {
		0 => {
			if buffer.len() < 2 {
				ParsedValue::NotEnoughBytes
			} else {
				ParsedValue::Value {
					val: buffer[1] as u32 + 64,
					next_index: 2,
				}
			}
		}

		1 => {
			if buffer.len() < 3 {
				ParsedValue::NotEnoughBytes
			} else {
				ParsedValue::Value {
					val: (buffer[2] as u32 * 256) + buffer[1] as u32 + 64,
					next_index: 3,
				}
			}
		}

		x => ParsedValue::Value {
			val: x as u32,
			next_index: 1,
		},
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::rml::time::RtmpTimestamp;
	use byteorder::{BigEndian, LittleEndian, WriteBytesExt};
	use std::io::{Cursor, Write};

	fn interleaved_messages(b_len: usize, chunk_size: usize) {
		let a = form_type_0_chunk(6, 100, 11, 9, &[0xa1; 300], chunk_size);
		let b = form_type_0_chunk(4, 200, 22, 8, &vec![0xb2; b_len], chunk_size);
		let boundary = 12 + chunk_size;
		let bytes = [&a[..boundary], &b, &a[boundary..]].concat();
		let mut decoder = ChunkDeserializer::new();
		decoder.set_max_chunk_size(chunk_size).unwrap();
		let expected_b = MessagePayload {
			timestamp: RtmpTimestamp::new(200),
			type_id: 8,
			message_stream_id: 22,
			data: bytes::Bytes::from(vec![0xb2; b_len]),
		};
		let expected_a = MessagePayload {
			timestamp: RtmpTimestamp::new(100),
			type_id: 9,
			message_stream_id: 11,
			data: bytes::Bytes::from(vec![0xa1; 300]),
		};
		assert_eq!(decoder.get_next_message(&bytes).unwrap(), Some(expected_b));
		assert_eq!(decoder.get_next_message(&[]).unwrap(), Some(expected_a));
		assert_eq!(decoder.get_next_message(&[]).unwrap(), None);
	}

	#[test]
	fn interleaved_shorter_message_does_not_underflow() {
		interleaved_messages(3, 128);
	}

	#[test]
	fn interleaved_longer_message_does_not_mix_payloads() {
		// A's first chunk is shorter than B, which fits in a chunk after a size change.
		let a = form_type_0_chunk(6, 100, 11, 9, &[0xa1; 200], 128);
		let b = form_type_0_chunk(4, 200, 22, 8, &[0xb2; 160], 256);
		let mut decoder = ChunkDeserializer::new();
		assert_eq!(decoder.get_next_message(&a[..140]).unwrap(), None);
		decoder.set_max_chunk_size(256).unwrap();
		let result = decoder.get_next_message(&b).unwrap().unwrap();
		assert_eq!(
			result,
			MessagePayload {
				timestamp: RtmpTimestamp::new(200),
				type_id: 8,
				message_stream_id: 22,
				data: bytes::Bytes::from(vec![0xb2; 160]),
			}
		);
		let result = decoder.get_next_message(&a[140..]).unwrap().unwrap();
		assert_eq!(
			result,
			MessagePayload {
				timestamp: RtmpTimestamp::new(100),
				type_id: 9,
				message_stream_id: 11,
				data: bytes::Bytes::from(vec![0xa1; 200]),
			}
		);
		assert_eq!(decoder.get_next_message(&[]).unwrap(), None);
	}

	fn drain(decoder: &mut ChunkDeserializer, bytes: &[u8], messages: &mut Vec<MessagePayload>) {
		let mut next = decoder.get_next_message(bytes).unwrap();
		while let Some(message) = next {
			messages.push(message);
			next = decoder.get_next_message(&[]).unwrap();
		}
	}

	#[test]
	fn interleaved_compressed_headers_and_fragmented_input_match_independent_streams() {
		let mut wire = Vec::new();
		let mut independent = [ChunkDeserializer::new(), ChunkDeserializer::new()];
		let mut expected = Vec::new();
		for decoder in &mut independent {
			decoder.set_max_chunk_size(2).unwrap();
		}
		for format in 0..4 {
			let mut packets = Vec::new();
			for (index, csid) in [6, 4].into_iter().enumerate() {
				let payload = vec![0xa0 + index as u8 + format; if format == 0 { 4 } else { 6 }];
				let (mut packet, header, extended) = match format {
					0 => (
						form_type_0_chunk(csid, 10 + index as u32, 11 + index as u32, 9, &payload, 2),
						12,
						None,
					),
					1 => (form_type_1_chunk(csid, 16777220, 8, &payload), 12, Some(16777220)),
					2 => (form_type_2_chunk(csid, 10, &payload), 4, None),
					_ => (form_type_3_chunk(csid, &payload, 2, None), 1, None),
				};
				if format == 1 || format == 2 {
					packet.truncate(header + 2);
					packet.extend(form_type_3_chunk(csid, &payload[2..], 2, extended));
				}
				let mut result = Vec::new();
				drain(&mut independent[index], &packet, &mut result);
				assert_eq!(result.len(), 1);
				assert_eq!(result[0].data.as_ref(), payload);
				assert_eq!(result[0].message_stream_id, 11 + index as u32);
				assert_eq!(result[0].type_id, if format == 0 { 9 } else { 8 });
				let timestamp = [10, 16777230, 16777240, 16777250][format as usize] + index as u32;
				assert_eq!(result[0].timestamp, RtmpTimestamp::new(timestamp));
				packets.push((packet, header + 2, result.remove(0)));
			}
			wire.extend_from_slice(&packets[0].0[..packets[0].1]);
			wire.extend_from_slice(&packets[1].0);
			wire.extend_from_slice(&packets[0].0[packets[0].1..]);
			expected.push(packets.remove(1).2);
			expected.push(packets.remove(0).2);
		}
		for fragment in [1, 2, 7, wire.len()] {
			let mut decoder = ChunkDeserializer::new();
			decoder.set_max_chunk_size(2).unwrap();
			let mut result = Vec::new();
			for bytes in wire.chunks(fragment) {
				drain(&mut decoder, bytes, &mut result);
			}
			assert_eq!(result, expected);
			assert_eq!(decoder.get_next_message(&[]).unwrap(), None);
			assert!(decoder.assemblies.is_empty());
			assert_eq!(decoder.assembly_bytes, 0);
		}
	}

	fn assert_released(decoder: &ChunkDeserializer) {
		assert!(decoder.assemblies.is_empty());
		assert!(decoder.previous_headers.is_empty());
		assert!(decoder.buffer.is_empty());
		assert_eq!(decoder.assembly_bytes, 0);
	}

	#[test]
	fn chunk_stream_limit_includes_incomplete_ids_and_releases_connection() {
		let mut decoder = ChunkDeserializer::new();
		decoder.set_max_chunk_size(1).unwrap();
		for csid in 2..2 + MAX_CHUNK_STREAMS as u32 {
			let packet = form_type_0_chunk(csid, 0, 1, 9, &[42; 2], 1);
			let header = if csid < 64 { 12 } else { 13 };
			assert_eq!(decoder.get_next_message(&packet[..header + 1]).unwrap(), None);
		}
		assert_eq!(decoder.previous_headers.len(), MAX_CHUNK_STREAMS);
		assert_eq!(decoder.assemblies.len(), MAX_CHUNK_STREAMS);
		assert_eq!(decoder.assembly_bytes, MAX_CHUNK_STREAMS * 2);
		let packet = form_type_0_chunk(258, 0, 1, 9, &[42], 1);
		assert!(matches!(
			decoder.get_next_message(&packet),
			Err(ChunkDeserializationError::ChunkStreamLimit)
		));
		assert_released(&decoder);
	}

	#[test]
	fn completed_ids_retain_headers_and_reuse_does_not_consume_slots() {
		let mut decoder = ChunkDeserializer::new();
		for csid in 2..2 + MAX_CHUNK_STREAMS as u32 {
			let packet = form_type_0_chunk(csid, 0, 1, 9, &[42], 128);
			assert!(decoder.get_next_message(&packet).unwrap().is_some());
		}
		for _ in 0..100 {
			let packet = form_type_3_chunk(2, &[42], 128, None);
			assert!(decoder.get_next_message(&packet).unwrap().is_some());
			assert_eq!(decoder.assembly_bytes, 0);
			assert!(decoder.assemblies.is_empty());
			assert_eq!(decoder.previous_headers.len(), MAX_CHUNK_STREAMS);
		}
		let packet = form_type_0_chunk(258, 0, 1, 9, &[42], 128);
		assert!(matches!(
			decoder.get_next_message(&packet),
			Err(ChunkDeserializationError::ChunkStreamLimit)
		));
		assert_released(&decoder);
	}

	#[test]
	fn aggregate_declared_lengths_are_charged_before_allocation() {
		let mut decoder = ChunkDeserializer::new();
		decoder.set_max_chunk_size(1).unwrap();
		for csid in 2..6 {
			let mut first = form_type_0_chunk(csid, 0, 1, 9, &[42], 1);
			first[4..7].copy_from_slice(&[0xff; 3]);
			assert_eq!(decoder.get_next_message(&first).unwrap(), None);
		}
		assert_eq!(decoder.assembly_bytes, MAX_ASSEMBLY_BYTES - 4);
		// Completion gives back the whole reservation, while the other IDs remain incomplete.
		let small = form_type_0_chunk(6, 0, 1, 9, &[42; 4], 1);
		assert!(decoder.get_next_message(&small).unwrap().is_some());
		assert_eq!(decoder.assembly_bytes, MAX_ASSEMBLY_BYTES - 4);
		assert_eq!(decoder.get_next_message(&small[..13]).unwrap(), None);
		assert_eq!(decoder.assembly_bytes, MAX_ASSEMBLY_BYTES);
		assert_eq!(
			decoder.assemblies.values().map(|a| a.data.capacity()).sum::<usize>(),
			MAX_ASSEMBLY_BYTES
		);
		let next = form_type_0_chunk(7, 0, 1, 9, &[42], 1);
		assert!(matches!(
			decoder.get_next_message(&next),
			Err(ChunkDeserializationError::AssemblyLimit)
		));
		assert_released(&decoder);
	}

	#[test]
	fn incomplete_message_reservations_are_reclaimed_on_repeated_reuse() {
		let mut decoder = ChunkDeserializer::new();
		let packet = form_type_0_chunk(6, 0, 1, 9, &[42; 200], 128);
		for _ in 0..100 {
			assert_eq!(decoder.get_next_message(&packet[..140]).unwrap(), None);
			assert_eq!(decoder.assembly_bytes, 200);
			assert!(decoder.get_next_message(&packet[140..]).unwrap().is_some());
			assert_eq!(decoder.assembly_bytes, 0);
			assert!(decoder.assemblies.is_empty());
			assert_eq!(decoder.previous_headers.len(), 1);
		}
		// A shared witness becomes unique only after teardown releases the assembly allocation.
		assert_eq!(decoder.get_next_message(&packet[..140]).unwrap(), None);
		let witness = decoder.assemblies.get_mut(&6).unwrap().data.split_to(1).freeze();
		assert!(!witness.is_unique());
		drop(decoder);
		assert!(witness.is_unique());
	}

	#[test]
	fn changed_incomplete_message_header_is_refused_and_releases_other_ids() {
		for (offset, replacement) in [(6, 1), (7, 8), (8, 2), (3, 1)] {
			let mut decoder = ChunkDeserializer::new();
			let packet = form_type_0_chunk(6, 0, 1, 9, &[42; 200], 128);
			assert_eq!(decoder.get_next_message(&packet[..140]).unwrap(), None);
			let other = form_type_0_chunk(4, 0, 1, 8, &[43; 200], 128);
			assert_eq!(decoder.get_next_message(&other[..140]).unwrap(), None);
			let mut changed = packet[..140].to_vec();
			changed[offset] = replacement;
			assert!(matches!(
				decoder.get_next_message(&changed),
				Err(ChunkDeserializationError::InconsistentMessage { csid: 6 })
			));
			assert_released(&decoder);
		}
	}

	#[test]
	fn input_budget_is_checked_before_buffering_and_releases_assemblies() {
		let mut decoder = ChunkDeserializer::new();
		let packet = form_type_0_chunk(6, 0, 1, 9, &[42; 200], 128);
		assert_eq!(decoder.get_next_message(&packet[..140]).unwrap(), None);
		let complete = form_type_0_chunk(4, 0, 1, 99, &[43], 128);
		let mut input = complete;
		input.resize(MAX_INPUT_BYTES, 0);
		assert!(decoder.get_next_message(&input).unwrap().is_some());
		assert_eq!(decoder.assembly_bytes, 200);
		// Appending exactly the consumed bytes fits. The zero-filled suffix starts
		// with a valid empty message on CSID 64, which consumes another 13 bytes.
		assert!(decoder.get_next_message(&[0; 13]).unwrap().is_some());
		assert!(matches!(
			decoder.get_next_message(&[0; 14]),
			Err(ChunkDeserializationError::InputLimit)
		));
		assert_released(&decoder);
	}

	#[test]
	fn extended_timestamp_subtraction_is_validated() {
		let mut decoder = ChunkDeserializer::new();
		let first = form_type_0_chunk(6, 0, 1, 9, &[42], 128);
		assert!(decoder.get_next_message(&first).unwrap().is_some());
		let mut next = form_type_1_chunk(6, MAX_INITIAL_TIMESTAMP + 1, 9, &[43]);
		next[8..12].copy_from_slice(&1u32.to_be_bytes());
		assert!(matches!(
			decoder.get_next_message(&next),
			Err(ChunkDeserializationError::InvalidExtendedTimestamp { timestamp: 1 })
		));
		assert_released(&decoder);
	}

	#[test]
	fn largest_numeric_csid_and_zero_length_messages_are_supported() {
		let mut decoder = ChunkDeserializer::new();
		let mut packet = vec![1, 255, 255];
		packet.extend_from_slice(&[0, 0, 10, 0, 0, 0, 9, 1, 0, 0, 0]);
		let first = decoder.get_next_message(&packet).unwrap().unwrap();
		assert_eq!(first.message_stream_id, 1);
		assert!(first.data.is_empty());
		assert_eq!(decoder.previous_headers.len(), 1);
		assert!(decoder.previous_headers.contains_key(&65599));
		let next = decoder.get_next_message(&[0xc1, 255, 255]).unwrap().unwrap();
		assert_eq!(next.timestamp, RtmpTimestamp::new(20));
		assert!(next.data.is_empty());
		assert!(decoder.assemblies.is_empty());
		assert_eq!(decoder.assembly_bytes, 0);
		assert_eq!(decoder.get_next_message(&[]).unwrap(), None);
	}

	#[test]
	fn abort_releases_only_the_target_assembly_in_both_session_roles() {
		use crate::rml::sessions::{
			ClientSession, ClientSessionConfig, ClientSessionResult, ServerSession, ServerSessionConfig,
			ServerSessionResult,
		};
		let (mut server, _) = ServerSession::new(ServerSessionConfig::new()).unwrap();
		let (mut client, _) = ClientSession::new(ClientSessionConfig::new()).unwrap();
		let a = form_type_0_chunk(6, 100, 11, 99, &[0xa1; 200], 128);
		let b = form_type_0_chunk(4, 200, 22, 99, &[0xb2; 200], 128);
		let abort = form_type_0_chunk(2, 0, 0, 2, &6u32.to_be_bytes(), 128);
		let replacement = form_type_2_chunk(6, 10, &[0xc3; 200]);
		let replacement = [&replacement[..132], &form_type_3_chunk(6, &[0xc3; 72], 128, None)].concat();
		let bytes = [&a[..140], &b[..140], &abort, &replacement, &b[140..]].concat();
		let expected = [
			MessagePayload {
				timestamp: RtmpTimestamp::new(110),
				message_stream_id: 11,
				type_id: 99,
				data: bytes::Bytes::from(vec![0xc3; 200]),
			},
			MessagePayload {
				timestamp: RtmpTimestamp::new(200),
				message_stream_id: 22,
				type_id: 99,
				data: bytes::Bytes::from(vec![0xb2; 200]),
			},
		];
		let server_results = server.handle_input(&bytes).unwrap();
		let client_results = client.handle_input(&bytes).unwrap();
		let server_payloads: Vec<_> = server_results
			.into_iter()
			.map(|result| match result {
				ServerSessionResult::UnhandleableMessageReceived(payload) => payload,
				other => panic!("unexpected result: {other:?}"),
			})
			.collect();
		let client_payloads: Vec<_> = client_results
			.into_iter()
			.map(|result| match result {
				ClientSessionResult::UnhandleableMessageReceived(payload) => payload,
				other => panic!("unexpected result: {other:?}"),
			})
			.collect();
		assert_eq!(server_payloads, expected);
		assert_eq!(client_payloads, expected);
		assert!(server.handle_input(&[]).unwrap().is_empty());
		assert!(client.handle_input(&[]).unwrap().is_empty());
	}

	#[test]
	fn abort_reclaims_reservation_and_keeps_compressed_header_history() {
		let mut decoder = ChunkDeserializer::new();
		for csid in [6, 4] {
			let packet = form_type_0_chunk(csid, 0, 1, 9, &[42; 200], 128);
			assert_eq!(decoder.get_next_message(&packet[..140]).unwrap(), None);
		}
		let witness = decoder.assemblies.get_mut(&6).unwrap().data.split_to(1).freeze();
		assert!(!witness.is_unique());
		decoder.abort(6);
		assert!(witness.is_unique());
		assert_eq!(decoder.assembly_bytes, 200);
		assert_eq!(decoder.assemblies.len(), 1);
		assert!(decoder.assemblies.contains_key(&4));
		assert_eq!(decoder.previous_headers.len(), 2);
		decoder.abort(6);
		decoder.abort(65599);
		assert_eq!(decoder.assembly_bytes, 200);
	}

	#[test]
	fn chunk_size_floor() {
		let mut decoder = ChunkDeserializer::new();
		assert!(matches!(
			decoder.set_max_chunk_size(0),
			Err(ChunkDeserializationError::InvalidMaxChunkSize { chunk_size: 0 })
		));
		assert_eq!(decoder.get_max_chunk_size(), 128);
		decoder.set_max_chunk_size(1).unwrap();
		assert_eq!(decoder.get_max_chunk_size(), 1);
	}

	#[test]
	fn can_read_type_0_chunk_with_small_chunk_stream_id_and_small_timestamp() {
		let csid = 50;
		let timestamp = 25u32;
		let message_stream_id = 5u32;
		let type_id = 3;
		let payload = [1_u8, 2_u8, 3_u8];

		let bytes = form_type_0_chunk(
			csid,
			timestamp,
			message_stream_id,
			type_id,
			&payload,
			INITIAL_MAX_CHUNK_SIZE,
		);
		let mut deserializer = ChunkDeserializer::new();
		let result = deserializer.get_next_message(&bytes).unwrap().unwrap();

		assert_eq!(result.type_id, 3, "Incorrect type id");
		assert_eq!(result.timestamp, RtmpTimestamp::new(timestamp), "Incorrect timestamp");
		assert_eq!(&result.data[..], &payload[..], "Incorrect data");
	}

	#[test]
	fn can_read_type_0_chunk_with_medium_chunk_stream_id_and_small_timestamp() {
		let csid = 500;
		let timestamp = 25u32;
		let message_stream_id = 5u32;
		let type_id = 3;
		let payload = [1_u8, 2_u8, 3_u8];

		let bytes = form_type_0_chunk(
			csid,
			timestamp,
			message_stream_id,
			type_id,
			&payload,
			INITIAL_MAX_CHUNK_SIZE,
		);
		let mut deserializer = ChunkDeserializer::new();
		let result = deserializer.get_next_message(&bytes).unwrap().unwrap();

		assert_eq!(result.type_id, 3, "Incorrect type id");
		assert_eq!(result.timestamp, RtmpTimestamp::new(timestamp), "Incorrect timestamp");
		assert_eq!(&result.data[..], &payload[..], "Incorrect data");
	}

	#[test]
	fn can_read_type_0_chunk_with_large_chunk_stream_id_and_small_timestamp() {
		let csid = 50000;
		let timestamp = 25u32;
		let message_stream_id = 5u32;
		let type_id = 3;
		let payload = [1_u8, 2_u8, 3_u8];

		let bytes = form_type_0_chunk(
			csid,
			timestamp,
			message_stream_id,
			type_id,
			&payload,
			INITIAL_MAX_CHUNK_SIZE,
		);
		let mut deserializer = ChunkDeserializer::new();
		let result = deserializer.get_next_message(&bytes).unwrap().unwrap();

		assert_eq!(result.type_id, 3, "Incorrect type id");
		assert_eq!(result.timestamp, RtmpTimestamp::new(timestamp), "Incorrect timestamp");
		assert_eq!(&result.data[..], &payload[..], "Incorrect data");
	}

	#[test]
	fn can_read_type_0_chunk_with_small_chunk_stream_id_and_large_timestamp() {
		let csid = 50;
		let timestamp = 16777216u32;
		let message_stream_id = 5u32;
		let type_id = 3;
		let payload = [1_u8, 2_u8, 3_u8];

		let bytes = form_type_0_chunk(
			csid,
			timestamp,
			message_stream_id,
			type_id,
			&payload,
			INITIAL_MAX_CHUNK_SIZE,
		);
		let mut deserializer = ChunkDeserializer::new();
		let result = deserializer.get_next_message(&bytes).unwrap().unwrap();

		assert_eq!(result.type_id, 3, "Incorrect type id");
		assert_eq!(result.timestamp, RtmpTimestamp::new(timestamp), "Incorrect timestamp");
		assert_eq!(&result.data[..], &payload[..], "Incorrect data");
	}

	#[test]
	fn can_read_type_0_chunk_with_medium_chunk_stream_id_and_large_timestamp() {
		let csid = 500;
		let timestamp = 16777216u32;
		let message_stream_id = 5u32;
		let type_id = 3;
		let payload = [1_u8, 2_u8, 3_u8];

		let bytes = form_type_0_chunk(
			csid,
			timestamp,
			message_stream_id,
			type_id,
			&payload,
			INITIAL_MAX_CHUNK_SIZE,
		);
		let mut deserializer = ChunkDeserializer::new();
		let result = deserializer.get_next_message(&bytes).unwrap().unwrap();

		assert_eq!(result.type_id, 3, "Incorrect type id");
		assert_eq!(result.timestamp, RtmpTimestamp::new(timestamp), "Incorrect timestamp");
		assert_eq!(&result.data[..], &payload[..], "Incorrect data");
	}

	#[test]
	fn can_read_type_0_chunk_with_large_chunk_stream_id_and_large_timestamp() {
		let csid = 50000;
		let timestamp = 16777216u32;
		let message_stream_id = 5u32;
		let type_id = 3;
		let payload = [1_u8, 2_u8, 3_u8];

		let bytes = form_type_0_chunk(
			csid,
			timestamp,
			message_stream_id,
			type_id,
			&payload,
			INITIAL_MAX_CHUNK_SIZE,
		);
		let mut deserializer = ChunkDeserializer::new();
		let result = deserializer.get_next_message(&bytes).unwrap().unwrap();

		assert_eq!(result.type_id, 3, "Incorrect type id");
		assert_eq!(result.timestamp, RtmpTimestamp::new(timestamp), "Incorrect timestamp");
		assert_eq!(&result.data[..], &payload[..], "Incorrect data");
	}

	#[test]
	fn can_read_type_1_chunk_with_small_chunk_stream_id_and_small_timestamp() {
		let csid = 50;
		let timestamp = 25u32;
		let delta = 10_u32;
		let message_stream_id = 5u32;
		let type_id1 = 3;
		let type_id2 = 4;
		let payload = [1_u8, 2_u8, 3_u8];

		let chunk_0_bytes = form_type_0_chunk(
			csid,
			timestamp,
			message_stream_id,
			type_id1,
			&payload,
			INITIAL_MAX_CHUNK_SIZE,
		);
		let chunk_1_bytes = form_type_1_chunk(csid, delta, type_id2, &payload);
		let mut deserializer = ChunkDeserializer::new();
		let _ = deserializer.get_next_message(&chunk_0_bytes).unwrap().unwrap();
		let result = deserializer.get_next_message(&chunk_1_bytes).unwrap().unwrap();

		assert_eq!(result.type_id, type_id2, "Incorrect type id");
		assert_eq!(
			result.timestamp,
			RtmpTimestamp::new(timestamp + delta),
			"Incorrect timestamp"
		);
		assert_eq!(&result.data[..], &payload[..], "Incorrect data");
	}

	#[test]
	fn can_read_type_2_chunk_with_small_chunk_stream_id_and_small_timestamp() {
		let csid = 50;
		let timestamp = 25u32;
		let delta1 = 10_u32;
		let delta2 = 11_u32;
		let message_stream_id = 5u32;
		let type_id1 = 3;
		let type_id2 = 4;
		let payload = [1_u8, 2_u8, 3_u8];

		let chunk_0_bytes = form_type_0_chunk(
			csid,
			timestamp,
			message_stream_id,
			type_id1,
			&payload,
			INITIAL_MAX_CHUNK_SIZE,
		);
		let chunk_1_bytes = form_type_1_chunk(csid, delta1, type_id2, &payload);
		let chunk_2_bytes = form_type_2_chunk(csid, delta2, &payload);
		let mut deserializer = ChunkDeserializer::new();
		let _ = deserializer.get_next_message(&chunk_0_bytes).unwrap().unwrap();
		let _ = deserializer.get_next_message(&chunk_1_bytes).unwrap().unwrap();
		let result = deserializer.get_next_message(&chunk_2_bytes).unwrap().unwrap();

		assert_eq!(result.type_id, type_id2, "Incorrect type id");
		assert_eq!(
			result.timestamp,
			RtmpTimestamp::new(timestamp + delta1 + delta2),
			"Incorrect timestamp"
		);
		assert_eq!(&result.data[..], &payload[..], "Incorrect data");
	}

	#[test]
	fn can_read_type_2_chunk_with_small_chunk_stream_id_and_large_timestamp() {
		let csid = 50;
		let timestamp = 25u32;
		let delta1 = 10_u32;
		let delta2 = 16777216_u32;
		let message_stream_id = 5u32;
		let type_id1 = 3;
		let type_id2 = 4;
		let payload = [1_u8, 2_u8, 3_u8];

		let chunk_0_bytes = form_type_0_chunk(
			csid,
			timestamp,
			message_stream_id,
			type_id1,
			&payload,
			INITIAL_MAX_CHUNK_SIZE,
		);
		let chunk_1_bytes = form_type_1_chunk(csid, delta1, type_id2, &payload);
		let chunk_2_bytes = form_type_2_chunk(csid, delta2, &payload);
		let mut deserializer = ChunkDeserializer::new();
		let _ = deserializer.get_next_message(&chunk_0_bytes).unwrap().unwrap();
		let _ = deserializer.get_next_message(&chunk_1_bytes).unwrap().unwrap();
		let result = deserializer.get_next_message(&chunk_2_bytes).unwrap().unwrap();

		assert_eq!(result.type_id, type_id2, "Incorrect type id");
		assert_eq!(
			result.timestamp,
			RtmpTimestamp::new(timestamp + delta1 + delta2),
			"Incorrect timestamp"
		);
		assert_eq!(&result.data[..], &payload[..], "Incorrect data");
	}

	#[test]
	fn can_read_type_3_chunk_with_small_chunk_stream_id_and_small_timestamp() {
		let csid = 50;
		let timestamp = 25u32;
		let delta1 = 10_u32;
		let delta2 = 11_u32;
		let message_stream_id = 5u32;
		let type_id1 = 3;
		let type_id2 = 4;
		let payload = [1_u8, 2_u8, 3_u8];

		let chunk_0_bytes = form_type_0_chunk(
			csid,
			timestamp,
			message_stream_id,
			type_id1,
			&payload,
			INITIAL_MAX_CHUNK_SIZE,
		);
		let chunk_1_bytes = form_type_1_chunk(csid, delta1, type_id2, &payload);
		let chunk_2_bytes = form_type_2_chunk(csid, delta2, &payload);
		let chunk_3_bytes = form_type_3_chunk(csid, &payload, INITIAL_MAX_CHUNK_SIZE, None);
		let mut deserializer = ChunkDeserializer::new();
		let _ = deserializer.get_next_message(&chunk_0_bytes).unwrap().unwrap();
		let _ = deserializer.get_next_message(&chunk_1_bytes).unwrap().unwrap();
		let _ = deserializer.get_next_message(&chunk_2_bytes).unwrap().unwrap();
		let result = deserializer.get_next_message(&chunk_3_bytes).unwrap().unwrap();

		assert_eq!(result.type_id, type_id2, "Incorrect type id");
		assert_eq!(
			result.timestamp,
			RtmpTimestamp::new(timestamp + delta1 + delta2 + delta2),
			"Incorrect timestamp"
		);
		assert_eq!(&result.data[..], &payload[..], "Incorrect data");
	}

	#[test]
	fn can_read_type_3_chunk_with_small_chunk_stream_id_and_large_timestamp() {
		let csid = 50;
		let timestamp = 10_u32;
		let delta1 = 10_u32;
		let delta2 = 16777216_u32;
		let message_stream_id = 5u32;
		let type_id1 = 3;
		let type_id2 = 4;
		let payload = [1_u8, 2_u8, 3_u8];

		let chunk_0_bytes = form_type_0_chunk(
			csid,
			timestamp,
			message_stream_id,
			type_id1,
			&payload,
			INITIAL_MAX_CHUNK_SIZE,
		);
		let chunk_1_bytes = form_type_1_chunk(csid, delta1, type_id2, &payload);
		let chunk_2_bytes = form_type_2_chunk(csid, delta2, &payload);
		let chunk_3_bytes = form_type_3_chunk(csid, &payload, INITIAL_MAX_CHUNK_SIZE, Some(delta2));
		let mut deserializer = ChunkDeserializer::new();
		let _ = deserializer.get_next_message(&chunk_0_bytes).unwrap().unwrap();
		let _ = deserializer.get_next_message(&chunk_1_bytes).unwrap().unwrap();
		let _ = deserializer.get_next_message(&chunk_2_bytes).unwrap().unwrap();
		let result = deserializer.get_next_message(&chunk_3_bytes).unwrap().unwrap();

		assert_eq!(result.type_id, type_id2, "Incorrect type id");
		assert_eq!(
			result.timestamp,
			RtmpTimestamp::new(timestamp + delta1 + delta2 + delta2),
			"Incorrect timestamp"
		);
		assert_eq!(&result.data[..], &payload[..], "Incorrect data");
	}

	#[test]
	fn can_read_message_spread_across_multiple_deserialization_calls() {
		let csid = 50;
		let timestamp = 25u32;
		let message_stream_id = 5u32;
		let type_id = 3;
		let payload = [1_u8, 2_u8, 3_u8];

		let all_bytes = form_type_0_chunk(
			csid,
			timestamp,
			message_stream_id,
			type_id,
			&payload,
			INITIAL_MAX_CHUNK_SIZE,
		);
		let (first, second) = all_bytes.split_at(all_bytes.len() / 2);
		let mut deserializer = ChunkDeserializer::new();
		match deserializer.get_next_message(first).unwrap() {
			Some(x) => panic!("Expected None but received {:?}", x),
			None => (),
		};

		let result = deserializer.get_next_message(second).unwrap().unwrap();

		assert_eq!(result.type_id, 3, "Incorrect type id");
		assert_eq!(result.timestamp, RtmpTimestamp::new(timestamp), "Incorrect timestamp");
		assert_eq!(&result.data[..], &payload[..], "Incorrect data");
	}

	#[test]
	fn can_read_message_exceeding_maximum_chunk_size() {
		let csid = 50;
		let timestamp = 25u32;
		let message_stream_id = 5u32;
		let type_id = 3;
		let payload = [100_u8; 500];
		let max_chunk_size = 100;

		let bytes = form_type_0_chunk(csid, timestamp, message_stream_id, type_id, &payload, max_chunk_size);
		let mut deserializer = ChunkDeserializer::new();
		deserializer.set_max_chunk_size(max_chunk_size).unwrap();
		let result = deserializer.get_next_message(&bytes).unwrap().unwrap();

		assert_eq!(result.type_id, 3, "Incorrect type id");
		assert_eq!(result.timestamp, RtmpTimestamp::new(timestamp), "Incorrect timestamp");
		assert_eq!(&result.data[..], &payload[..], "Incorrect data");
	}

	#[test]
	fn error_when_setting_chunk_size_too_large() {
		const CHUNK_SIZE_VALUE: usize = 2147483648;
		let mut deserializer = ChunkDeserializer::new();
		match deserializer.set_max_chunk_size(CHUNK_SIZE_VALUE) {
			Err(ChunkDeserializationError::InvalidMaxChunkSize {
				chunk_size: CHUNK_SIZE_VALUE,
			}) => {} // success
			x => panic!("Unexpected set max chunk size result of {:?}", x),
		}
	}

	#[test]
	fn type_2_chunk_that_exceeds_max_chunk_size_does_not_keep_applying_delta_to_timestamp() {
		// It was noticed that OBS does not totally conform to the RTMP specification.  It will
		// send a type 1 chunk with a time delta for a video packet, but will send the remaining
		// parts of that chunk with a type 3 header (even though the delta should not be applied).
		// this test verifies we can handle that.

		let chunk1 = [
			0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x09, 0x01, 0x00, 0x00, 0x00, 0x01,
		];
		let chunk2 = [
			0x44, 0x00, 0x00, 0x21, 0x00, 0x00, 0x05, 0x09, 0x01, 0x02, 0x03, 0x04, 0xc4, 0x05,
		];

		let mut deserializer = ChunkDeserializer::new();
		deserializer.set_max_chunk_size(4).unwrap();

		let payload1 = deserializer.get_next_message(&chunk1).unwrap().unwrap();
		assert_eq!(payload1.type_id, 0x09, "Incorrect payload 1 type");
		assert_eq!(
			payload1.timestamp,
			RtmpTimestamp::new(0),
			"Incorrect payload 1 timestamp"
		);
		assert_eq!(&payload1.data[..], &[0x01], "Incorrect payload 1 data");

		let payload2 = deserializer.get_next_message(&chunk2).unwrap().unwrap();
		assert_eq!(payload2.type_id, 0x09, "Incorrect payload 2 type");
		assert_eq!(
			payload2.timestamp,
			RtmpTimestamp::new(33),
			"Incorrect payload 2 timestamp"
		);
		assert_eq!(
			&payload2.data[..],
			&[0x01, 0x02, 0x03, 0x04, 0x05],
			"Incorrect payload 2 data"
		);
	}

	#[test]
	fn can_read_type_3_chunk_that_follows_type_0_has_extended_timestamp() {
		let chunk1 = [
			0x06, 0xff, 0xff, 0xff, 0x00, 0x00, 0x07, 0x09, 0x01, 0x00, 0x00, 0x00, 0x01, 0xff, 0xff, 0xff, 0x01, 0x02,
			0x03, 0x04,
		];
		let chunk2 = [0xc6, 0x01, 0xff, 0xff, 0xff, 0x05, 0x06, 0x07];
		let mut deserializer = ChunkDeserializer::new();
		deserializer.set_max_chunk_size(4).unwrap();
		let _ = deserializer.get_next_message(&chunk1).unwrap();
		let payload = deserializer.get_next_message(&chunk2).unwrap().unwrap();
		assert_eq!(payload.type_id, 0x09, "Incorrect payload type");
		assert_eq!(
			payload.timestamp,
			RtmpTimestamp::new(0x1ffffff),
			"Incorrect payload timestamp"
		);
		assert_eq!(
			&payload.data[..],
			&[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07],
			"Incorrect payload data"
		);
	}

	fn form_type_0_chunk(
		csid: u32,
		timestamp: u32,
		message_stream_id: u32,
		type_id: u8,
		payload: &[u8],
		max_chunk_length: usize,
	) -> Vec<u8> {
		let mut cursor = Cursor::new(Vec::new());
		if csid < 64 {
			cursor.write_u8(csid as u8).unwrap();
		} else if csid < 319 {
			cursor.write_u8(0_u8).unwrap();
			cursor.write_u8((csid - 64) as u8).unwrap();
		} else {
			cursor.write_u8(1_u8).unwrap();
			cursor.write_u16::<BigEndian>((csid - 64) as u16).unwrap();
		}

		let standard_timestamp = if timestamp >= 16777215 { 16777215 } else { timestamp };
		cursor.write_u24::<BigEndian>(standard_timestamp).unwrap();
		cursor.write_u24::<BigEndian>(payload.len() as u32).unwrap();
		cursor.write_u8(type_id).unwrap();
		cursor.write_u32::<LittleEndian>(message_stream_id).unwrap();

		let mut option_extended_timestamp = None;
		if timestamp > 16777215 {
			cursor.write_u32::<BigEndian>(timestamp).unwrap();
			option_extended_timestamp = Some(timestamp);
		}

		// If the payload is over max_chunk_length, assume we want to form a split message
		// and therefore need to only write the max chunk amount of the payload in this request
		// and append a type 3 chunk with the rest
		if payload.len() > max_chunk_length {
			cursor.write(&payload[..max_chunk_length]).unwrap();

			let next_chunk = form_type_3_chunk(
				csid,
				&payload[max_chunk_length..],
				max_chunk_length,
				option_extended_timestamp,
			);
			cursor.write(&next_chunk).unwrap();
		} else {
			cursor.write(payload).unwrap();
		}

		cursor.into_inner()
	}

	fn form_type_1_chunk(csid: u32, delta: u32, type_id: u8, payload: &[u8]) -> Vec<u8> {
		let mut cursor = Cursor::new(Vec::new());
		if csid < 64 {
			cursor.write_u8((csid as u8) | 0b01000000).unwrap();
		} else if csid < 319 {
			cursor.write_u8(0_u8 | 0b01000000).unwrap();
			cursor.write_u8((csid - 64) as u8).unwrap();
		} else {
			cursor.write_u8(1_u8 | 0b01000000).unwrap();
			cursor.write_u16::<BigEndian>((csid - 64) as u16).unwrap();
		}

		let standard_timestamp = if delta >= 16777215 { 16777215 } else { delta };
		cursor.write_u24::<BigEndian>(standard_timestamp).unwrap();
		cursor.write_u24::<BigEndian>(payload.len() as u32).unwrap();
		cursor.write_u8(type_id).unwrap();

		if delta > 16777215 {
			cursor.write_u32::<BigEndian>(delta).unwrap();
		}

		cursor.write(payload).unwrap();

		cursor.into_inner()
	}

	fn form_type_2_chunk(csid: u32, delta: u32, payload: &[u8]) -> Vec<u8> {
		let mut cursor = Cursor::new(Vec::new());
		if csid < 64 {
			cursor.write_u8((csid as u8) | 0b10000000).unwrap();
		} else if csid < 319 {
			cursor.write_u8(0_u8 | 0b10000000).unwrap();
			cursor.write_u8((csid - 64) as u8).unwrap();
		} else {
			cursor.write_u8(1_u8 | 0b10000000).unwrap();
			cursor.write_u16::<BigEndian>((csid - 64) as u16).unwrap();
		}

		let standard_timestamp = if delta >= 16777215 { 16777215 } else { delta };
		cursor.write_u24::<BigEndian>(standard_timestamp).unwrap();

		if delta > 16777215 {
			cursor.write_u32::<BigEndian>(delta).unwrap();
		}

		cursor.write(payload).unwrap();

		cursor.into_inner()
	}

	fn form_type_3_chunk(
		csid: u32,
		payload: &[u8],
		max_chunk_length: usize,
		option_extended_timestamp: Option<u32>,
	) -> Vec<u8> {
		let mut cursor = Cursor::new(Vec::new());
		if csid < 64 {
			cursor.write_u8((csid as u8) | 0b11000000).unwrap();
		} else if csid < 319 {
			cursor.write_u8(0_u8 | 0b11000000).unwrap();
			cursor.write_u8((csid - 64) as u8).unwrap();
		} else {
			cursor.write_u8(1_u8 | 0b11000000).unwrap();
			cursor.write_u16::<BigEndian>((csid - 64) as u16).unwrap();
		}

		if option_extended_timestamp != None {
			assert_eq!(
				option_extended_timestamp.unwrap() >= MAX_INITIAL_TIMESTAMP,
				true,
				"timestamp was less than 0xffffff"
			);
			cursor
				.write_u32::<BigEndian>(option_extended_timestamp.unwrap())
				.unwrap();
		}

		// If the payload is over max_chunk_length, assume we want to form a split message
		// and therefore need to only write the max chunk amount of the payload in this request
		// and append a type 3 chunk with the rest
		if payload.len() > max_chunk_length {
			cursor.write(&payload[..max_chunk_length]).unwrap();

			let next_chunk = form_type_3_chunk(
				csid,
				&payload[max_chunk_length..],
				max_chunk_length,
				option_extended_timestamp,
			);
			cursor.write(&next_chunk).unwrap();
		} else {
			cursor.write(payload).unwrap();
		}

		cursor.into_inner()
	}
}
