//! Video assembly across access-unit size and PES fragmentation.
use bytes::Bytes;
use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use mpeg2ts::{
	pes::{PesPacketReader, ReadPesPacket},
	ts::TsPacketReader,
};
use std::{io::Cursor, time::Duration};

fn input(size: usize, payload: usize) -> Vec<u8> {
	let fixture = include_bytes!("../src/container/ts/test_data/poc-avc.ts");
	let mut reader = PesPacketReader::new(TsPacketReader::new(Cursor::new(fixture)));
	// A real SPS/PPS/IDR exercises the timing parser rather than its explicit-PTS
	// fallback for unsupported syntax. Padding stays in the final slice NAL.
	let mut au = reader.read_pes_packet().unwrap().unwrap().data;
	assert!(au.len() <= size);
	au.resize(size, 0xff);
	// The fixture's SDT/PAT/PMT declare one AVC stream on PID 0x100.
	let mut out = include_bytes!("../src/container/ts/test_data/poc-avc.ts")[..188 * 3].to_vec();
	let mut cc = 0;
	for (n, data) in au.chunks(payload).enumerate() {
		let len = data.len() + if n == 0 { 8 } else { 3 };
		let mut pes = vec![
			0,
			0,
			1,
			0xe0,
			(len >> 8) as u8,
			len as u8,
			0x80,
			if n == 0 { 0x80 } else { 0 },
			if n == 0 { 5 } else { 0 },
		];
		if n == 0 {
			pes.extend([0x21, 0, 1, 0, 1]);
		}
		pes.extend(data);
		for (packet, chunk) in pes.chunks(184).enumerate() {
			let mut ts = vec![
				0x47,
				if packet == 0 { 0x41 } else { 1 },
				0,
				if chunk.len() == 184 { 0x10 | cc } else { 0x30 | cc },
			];
			cc = (cc + 1) & 15;
			if chunk.len() < 184 {
				let field = 183 - chunk.len();
				ts.push(field as u8);
				if field > 0 {
					ts.push(0);
					ts.resize(5 + field, 0xff);
				}
			}
			ts.extend(chunk);
			out.extend(ts);
		}
	}
	out
}

fn assembly(c: &mut Criterion) {
	let mut group = c.benchmark_group("ts_video");
	group.measurement_time(Duration::from_secs(1));
	group.warm_up_time(Duration::from_millis(200));
	group.sample_size(10);
	for size in [4_096, 65_536, 1_048_576] {
		group.throughput(Throughput::Bytes(size as u64));
		for payload in [64, 1_024, 65_000] {
			let data = Bytes::from(input(size, payload));
			group.bench_function(BenchmarkId::new(size.to_string(), payload), |b| {
				b.iter_batched(
					|| {
						let mut broadcast = moq_net::broadcast::Info::new().produce();
						let catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
						(
							moq_mux::container::ts::Import::new(broadcast, catalog.reserve()),
							catalog,
						)
					},
					|(mut import, _catalog)| {
						import.decode(&data).unwrap();
						import.finish().unwrap();
						std::hint::black_box(import);
					},
					BatchSize::SmallInput,
				)
			});
		}
	}
	group.finish();
}
criterion_group!(benches, assembly);
criterion_main!(benches);
