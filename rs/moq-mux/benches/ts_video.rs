//! Video assembly across access-unit size and PES fragmentation.
//!
//! Run with `cargo bench -p moq-mux --bench ts_video`.
use std::fmt;
use std::io::Cursor;

use bytes::Bytes;
use mpeg2ts::{
	pes::{PesPacketReader, ReadPesPacket},
	ts::TsPacketReader,
};
use swole::bench;

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

/// An access unit of `size` bytes split into PES packets of `payload` bytes, named `<size>/<payload>`.
#[derive(Clone, Copy)]
struct Case {
	size: usize,
	payload: usize,
}

impl fmt::Display for Case {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}/{}", self.size, self.payload)
	}
}

fn cases() -> Vec<Case> {
	[4_096, 65_536, 1_048_576]
		.into_iter()
		.flat_map(|size| [64, 1_024, 65_000].map(|payload| Case { size, payload }))
		.collect()
}

/// The stream, and an importer ready for it that each call takes.
struct Assembly {
	data: Bytes,
	import: Option<(moq_mux::container::ts::Import, moq_mux::catalog::Producer)>,
}

fn assembly(case: Case) -> Assembly {
	Assembly {
		data: Bytes::from(input(case.size, case.payload)),
		import: None,
	}
}

fn importer(assembly: &mut Assembly) {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	assembly.import = Some((
		moq_mux::container::ts::Import::new(broadcast, catalog.reserve()),
		catalog,
	));
}

#[bench(args = cases(), setup = assembly, before = importer, bytes = |case: Case| case.size)]
fn ts_video(assembly: &mut Assembly) {
	let (mut import, _catalog) = assembly.import.take().expect("an importer");
	import.decode(&assembly.data).unwrap();
	let _finished = import.finish().unwrap();
}

fn main() {
	swole::main();
}
