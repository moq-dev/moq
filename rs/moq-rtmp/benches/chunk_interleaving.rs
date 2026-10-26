//! Decode one hot CSID while other CSIDs retain incomplete messages.
//! Sweep the table size independently of message size and chunk count.

#[path = "../src/rml/mod.rs"]
mod rml;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use rml::chunk_io::ChunkDeserializer;
use std::hint::black_box;

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

fn bench(c: &mut Criterion) {
	let mut group = c.benchmark_group("rtmp_chunks");
	for ids in [1, 16, 64, 256] {
		for length in [128, 4096, 65536] {
			for chunk in [128, 4096] {
				let mut decoder = ChunkDeserializer::new();
				decoder.set_max_chunk_size(chunk).unwrap();
				for csid in 3..(ids + 2) {
					let bytes = message(csid, chunk * 2, chunk);
					let header = if csid < 64 { 12 } else { 13 };
					assert!(decoder.get_next_message(&bytes[..header + chunk]).unwrap().is_none());
				}
				let bytes = message(2, length, chunk);
				group.throughput(Throughput::Bytes(length as u64));
				group.bench_function(BenchmarkId::new(format!("ids_{ids}/chunk_{chunk}"), length), |b| {
					b.iter(|| {
						let payload = decoder.get_next_message(black_box(&bytes)).unwrap().unwrap();
						assert_eq!(payload.data.len(), length);
						black_box(payload);
						assert!(decoder.get_next_message(&[]).unwrap().is_none());
					});
				});
			}
		}
	}
	group.finish();
}

fn amf0(c: &mut Criterion) {
	let mut group = c.benchmark_group("rtmp_amf0");
	for count in [1024usize, 65536, 1048576] {
		for (name, value) in [("nulls", vec![5]), ("properties", vec![0, 1, b'a', 5])] {
			let mut bytes = Vec::new();
			if name == "properties" {
				bytes.push(3);
			}
			for _ in 0..count {
				bytes.extend_from_slice(&value);
			}
			if name == "properties" {
				bytes.extend_from_slice(&[0, 0, 9]);
			}
			group.throughput(Throughput::Bytes(bytes.len() as u64));
			group.bench_function(BenchmarkId::new(name, count), |b| {
				b.iter(|| black_box(rml::amf0::Values::decode(black_box(&bytes)).unwrap()));
			});
		}
	}
	group.finish();
}

fn connect(c: &mut Criterion) {
	use rml::amf0::Amf0Value;
	use rml::chunk_io::ChunkSerializer;
	use rml::messages::RtmpMessage;
	use rml::sessions::{ServerSession, ServerSessionConfig};
	use rml::time::RtmpTimestamp;
	let mut group = c.benchmark_group("rtmp_connect_fourccs");
	for count in [16usize, 256, 4096] {
		let properties = [
			("app".into(), Amf0Value::Utf8String("live".into())),
			(
				"fourCcList".into(),
				Amf0Value::StrictArray(
					(0..count)
						.rev()
						.map(|id| Amf0Value::Utf8String(format!("{id:04x}")))
						.collect(),
				),
			),
		]
		.into_iter()
		.collect();
		let message = RtmpMessage::Amf0Command {
			command_name: "connect".into(),
			transaction_id: 1.0,
			command_object: Amf0Value::Object(properties),
			additional_arguments: Vec::new(),
		};
		let payload = message.into_message_payload(RtmpTimestamp::new(0), 0).unwrap();
		let packet = ChunkSerializer::new().serialize(&payload, false, false).unwrap();
		group.throughput(Throughput::Elements(count as u64));
		group.bench_function(BenchmarkId::from_parameter(count), |b| {
			b.iter_batched(
				|| ServerSession::new(ServerSessionConfig::new()).unwrap().0,
				|mut session| black_box(session.handle_input(black_box(&packet.bytes)).unwrap()),
				criterion::BatchSize::SmallInput,
			);
		});
	}
	group.finish();
}

criterion_group!(benches, bench, amf0, connect);
criterion_main!(benches);
