/*!

This module contains structs for the serialization and deserialization of RTMP chunks, as described
in section 5.3 of the [official RTMP specification](https://www.adobe.com/content/dam/acom/en/devnet/rtmp/pdf/rtmp_specification_1.0.pdf).

The RTMP chunk format is complicated, and heavily relies on information from previously sent and
received chunks.  Due to this it is important that every inbound RTMP chunk is deserialized in the
order it was received, and every outbound RTMP chunk is sent in the order it was received.  It
also means that a new `ChunkSerializer` or `ChunkDeserializer` cannot be introduced mid-stream, as
chances are it will cause errors (either locally or on the connected peer).

Inbound bytes that are part of RTMP chunks are deserialized into `MessagePayload`s.  These are data
structures that contain information about the RTMP message that the chunk contained, such as
timestamp, type id, the message stream id, etc...

Outbound RTMP message payloads are serialized into outbound `Packet`s.  These are a thin wrapper
around the bytes representing the RTMP chunk that was created, but also creates information on if
the packet is allowed to be dropped or not.  Video and Audio data can uaually be marked as able
to be dropped in case bandwidth limitations are encountered between the client and the server.  Any
packet that is *not* marked as being able to be dropped should not be dropped, as that is a pretty
sure way to cause deserialization errors with the peer.

Inbound and outbound binary data relies on the [bytes crate](https://crates.io/crates/bytes) to
provide input and output buffers with minimal allocations.

## Examples

```ignore
# extern crate bytes;
# extern crate rml_rtmp;
# use bytes::Bytes;
# use rml_rtmp::time::RtmpTimestamp;
# use rml_rtmp::chunk_io::{ChunkSerializer, ChunkDeserializer};
# use rml_rtmp::messages::MessagePayload;
# fn main() {
let input1 = MessagePayload {
	timestamp: RtmpTimestamp::new(55),
	message_stream_id: 1,
	type_id: 15,
	data: Bytes::from(vec![1, 2, 3, 4, 5, 6]),
};

let mut serializer = ChunkSerializer::new();
let packet1 = serializer.serialize(&input1, false, false).unwrap();

let mut deserializer = ChunkDeserializer::new();
let output1 = deserializer.get_next_message(&packet1.bytes).unwrap().unwrap();

assert_eq!(output1, input1);
# }

```
*/

mod chunk_header;
mod deserialization_errors;
mod deserializer;
mod serialization_errors;
mod serializer;

pub use self::deserialization_errors::ChunkDeserializationError;
pub use self::deserializer::ChunkDeserializer;
pub use self::serialization_errors::ChunkSerializationError;
pub use self::serializer::{ChunkSerializer, Packet};

#[cfg(test)]
mod tests {
	use super::*;
	use crate::rml::messages::MessagePayload;
	use crate::rml::time::RtmpTimestamp;
	use bytes::Bytes;

	#[test]
	fn can_deserialize_messages_serialized_by_chunk_serializer_struct() {
		let input1 = MessagePayload {
			timestamp: RtmpTimestamp::new(55),
			message_stream_id: 1,
			type_id: 15,
			data: Bytes::from(vec![1, 2, 3, 4, 5, 6]),
		};

		let input2 = MessagePayload {
			timestamp: RtmpTimestamp::new(65),
			message_stream_id: 1,
			type_id: 15,
			data: Bytes::from(vec![8, 9, 10]),
		};

		let input3 = MessagePayload {
			timestamp: RtmpTimestamp::new(75),
			message_stream_id: 1,
			type_id: 15,
			data: Bytes::from(vec![1, 2, 3]),
		};

		let mut serializer = ChunkSerializer::new();
		let packet1 = serializer.serialize(&input1, false, false).unwrap();
		let packet2 = serializer.serialize(&input2, false, false).unwrap();
		let packet3 = serializer.serialize(&input3, false, false).unwrap();

		let mut deserializer = ChunkDeserializer::new();
		let output1 = deserializer.get_next_message(&packet1.bytes).unwrap().unwrap();
		let output2 = deserializer.get_next_message(&packet2.bytes).unwrap().unwrap();
		let output3 = deserializer.get_next_message(&packet3.bytes).unwrap().unwrap();

		assert_eq!(output1, input1, "First message was not deserialized as expected");
		assert_eq!(output2, input2, "Second message was not deserialized as expected");
		assert_eq!(output3, input3, "Third message was not deserialized as expected");
	}

	#[test]
	fn can_deserialize_messages_serialized_with_decreasing_time() {
		let input1 = MessagePayload {
			timestamp: RtmpTimestamp::new(65),
			message_stream_id: 1,
			type_id: 15,
			data: Bytes::from(vec![1, 2, 3, 4, 5, 6]),
		};

		let input2 = MessagePayload {
			timestamp: RtmpTimestamp::new(55),
			message_stream_id: 1,
			type_id: 15,
			data: Bytes::from(vec![8, 9, 10]),
		};

		let input3 = MessagePayload {
			timestamp: RtmpTimestamp::new(45),
			message_stream_id: 1,
			type_id: 15,
			data: Bytes::from(vec![1, 2, 3]),
		};

		let mut serializer = ChunkSerializer::new();
		let packet1 = serializer.serialize(&input1, false, false).unwrap();
		let packet2 = serializer.serialize(&input2, false, false).unwrap();
		let packet3 = serializer.serialize(&input3, false, false).unwrap();

		let mut deserializer = ChunkDeserializer::new();
		let output1 = deserializer.get_next_message(&packet1.bytes).unwrap().unwrap();
		let output2 = deserializer.get_next_message(&packet2.bytes).unwrap().unwrap();
		let output3 = deserializer.get_next_message(&packet3.bytes).unwrap().unwrap();

		assert_eq!(output1, input1, "First message was not deserialized as expected");
		assert_eq!(output2, input2, "Second message was not deserialized as expected");
		assert_eq!(output3, input3, "Third message was not deserialized as expected");
	}
}
