//! Decode one hot CSID while other CSIDs retain incomplete messages.
//! Sweep the table size independently of message size and chunk count.
//!
//! Run with `cargo bench -p moq-rtmp --bench chunk_interleaving`.

#[path = "../src/rml/mod.rs"]
mod rml;

use std::fmt;

use rml::chunk_io::ChunkDeserializer;
use swole::bench;

fn message(csid: u16, length: usize, chunk: usize) -> Vec<u8> {
	let basic = if csid < 64 {
		vec![csid as u8]
	} else {
		vec![0, (csid - 64) as u8]
	};
	let mut bytes = basic.clone();
	bytes.extend_from_slice(&[0, 0, 1]);
	bytes.extend_from_slice(&(length as u32).to_be_bytes()[1..]);
	bytes.extend_from_slice(&[9, 1, 0, 0, 0]);
	bytes.extend(std::iter::repeat_n(42, length.min(chunk)));
	for start in (chunk..length).step_by(chunk) {
		let mut continuation = basic.clone();
		continuation[0] |= 0xc0;
		bytes.extend_from_slice(&continuation);
		bytes.extend(std::iter::repeat_n(42, chunk.min(length - start)));
	}
	bytes
}

/// Retained CSIDs, chunk size, and hot message length, named `ids_<n>/chunk_<n>/<length>`.
#[derive(Clone, Copy)]
struct Chunks {
	ids: u16,
	chunk: usize,
	length: usize,
}

impl fmt::Display for Chunks {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "ids_{}/chunk_{}/{}", self.ids, self.chunk, self.length)
	}
}

fn chunks() -> Vec<Chunks> {
	let mut cases = Vec::new();
	for ids in [1, 16, 64, 256] {
		for length in [128, 4096, 65536] {
			for chunk in [128, 4096] {
				cases.push(Chunks { ids, chunk, length });
			}
		}
	}
	cases
}

/// A decoder holding `ids - 1` incomplete messages, and the hot message to decode.
struct Interleaved {
	decoder: ChunkDeserializer,
	bytes: Vec<u8>,
	length: usize,
}

fn interleaved(case: Chunks) -> Interleaved {
	let mut decoder = ChunkDeserializer::new();
	decoder.set_max_chunk_size(case.chunk).unwrap();
	for csid in 3..(case.ids + 2) {
		let bytes = message(csid, case.chunk * 2, case.chunk);
		let header = if csid < 64 { 12 } else { 13 };
		assert!(
			decoder
				.get_next_message(&bytes[..header + case.chunk])
				.unwrap()
				.is_none()
		);
	}
	Interleaved {
		decoder,
		bytes: message(2, case.length, case.chunk),
		length: case.length,
	}
}

#[bench(args = chunks(), setup = interleaved, bytes = |case: Chunks| case.length)]
fn rtmp_chunks(input: &mut Interleaved) -> rml::messages::MessagePayload {
	let payload = input.decoder.get_next_message(&input.bytes).unwrap().unwrap();
	assert_eq!(payload.data.len(), input.length);
	assert!(input.decoder.get_next_message(&[]).unwrap().is_none());
	payload
}

mod rtmp_amf0 {
	use swole::bench;

	use super::rml;

	const COUNTS: [usize; 3] = [1024, 65536, 1048576];

	fn nulls_bytes(count: usize) -> Vec<u8> {
		vec![5; count]
	}

	fn properties_bytes(count: usize) -> Vec<u8> {
		let mut bytes = vec![3];
		for _ in 0..count {
			bytes.extend_from_slice(&[0, 1, b'a', 5]);
		}
		bytes.extend_from_slice(&[0, 0, 9]);
		bytes
	}

	#[bench(args = COUNTS, setup = nulls_bytes, bytes = |count| count)]
	fn nulls(bytes: &[u8]) -> usize {
		rml::amf0::Values::decode(bytes).unwrap().len()
	}

	#[bench(args = COUNTS, setup = properties_bytes, bytes = |count| 4 * count + 4)]
	fn properties(bytes: &[u8]) -> usize {
		rml::amf0::Values::decode(bytes).unwrap().len()
	}
}

mod rtmp_connect_fourccs {
	use swole::bench;

	use super::rml;
	use rml::amf0::Amf0Value;
	use rml::chunk_io::ChunkSerializer;
	use rml::messages::RtmpMessage;
	use rml::sessions::{ServerSession, ServerSessionConfig, ServerSessionResult};
	use rml::time::RtmpTimestamp;

	const COUNTS: [usize; 3] = [16, 256, 4096];

	/// A fresh session and a `connect` command whose `field` holds `value`.
	fn connect(field: &str, value: Amf0Value) -> (ServerSession, Vec<u8>) {
		let message = RtmpMessage::Amf0Command {
			command_name: "connect".into(),
			transaction_id: 1.0,
			command_object: Amf0Value::Object(
				[
					("app".into(), Amf0Value::Utf8String("live".into())),
					(field.into(), value),
				]
				.into_iter()
				.collect(),
			),
			additional_arguments: Vec::new(),
		};
		let payload = message.into_message_payload(RtmpTimestamp::new(0), 0).unwrap();
		let packet = ChunkSerializer::new().serialize(&payload, false, false).unwrap();
		let session = ServerSession::new(ServerSessionConfig::new()).unwrap().0;
		(session, packet.bytes)
	}

	fn names(count: usize) -> impl Iterator<Item = String> {
		(0..count).rev().map(|id| format!("{id:04x}"))
	}

	fn array_input(count: usize) -> (ServerSession, Vec<u8>) {
		let value = Amf0Value::StrictArray(names(count).map(Amf0Value::Utf8String).collect());
		connect("fourCcList", value)
	}

	fn object_input(count: usize) -> (ServerSession, Vec<u8>) {
		let value = Amf0Value::Object(
			names(count)
				.map(|name| (name.clone(), Amf0Value::Utf8String(name)))
				.collect(),
		);
		connect("fourCcList", value)
	}

	fn info_map_input(count: usize) -> (ServerSession, Vec<u8>) {
		let value = Amf0Value::Object(names(count).map(|name| (name, Amf0Value::Number(1.0))).collect());
		connect("videoFourCcInfoMap", value)
	}

	/// Handles the command in a fresh session, which drops inside the measurement; the packet
	/// is returned so its free does not count.
	fn handle((mut session, packet): (ServerSession, Vec<u8>)) -> (Vec<ServerSessionResult>, Vec<u8>) {
		(session.handle_input(&packet).unwrap(), packet)
	}

	#[bench(args = COUNTS, setup = array_input, items = |count| count)]
	fn array(input: (ServerSession, Vec<u8>)) -> (Vec<ServerSessionResult>, Vec<u8>) {
		handle(input)
	}

	#[bench(args = COUNTS, setup = object_input, items = |count| count)]
	fn object(input: (ServerSession, Vec<u8>)) -> (Vec<ServerSessionResult>, Vec<u8>) {
		handle(input)
	}

	#[bench(args = COUNTS, setup = info_map_input, items = |count| count)]
	fn info_map(input: (ServerSession, Vec<u8>)) -> (Vec<ServerSessionResult>, Vec<u8>) {
		handle(input)
	}
}

fn main() {
	swole::main();
}
