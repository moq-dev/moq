//! Splitting an MPTS across program count and stray PID count. Each stray PID's packets go out
//! once as unclaimed PIDs, which every PID's accounting reads, and once as null packets, the
//! baseline that no accounting reads, so the gap between the two is the accounting's cost.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::time::Duration;

const MPEG_2: crc::Crc<u32> = crc::Crc::<u32>::new(&crc::CRC_32_MPEG_2);
/// 27 MHz ticks per 40 ms interval.
const INTERVAL: u64 = 40 * 27_000;
const INTERVALS: u64 = 50;
const NULL: u16 = 0x1fff;
const STRAY: u16 = 0x1000;

/// A transport stream under construction, continuity counted per PID.
#[derive(Default)]
struct Mux {
	out: Vec<u8>,
	cc: std::collections::HashMap<u16, u8>,
}

impl Mux {
	/// A payload-only packet; `payload` fills it, or the tail is 0xFF.
	fn packet(&mut self, pid: u16, start: bool, payload: &[u8]) {
		let cc = self.cc.entry(pid).or_default();
		let mut packet = vec![0x47, (start as u8) << 6 | (pid >> 8) as u8, pid as u8, 0x10 | *cc];
		*cc = (*cc + 1) & 0x0f;
		packet.extend_from_slice(payload);
		packet.resize(188, 0xff);
		self.out.extend_from_slice(&packet);
	}

	/// A one-packet PSI section of `table_id` with `body`.
	fn section(&mut self, pid: u16, table_id: u8, extension: u16, body: &[u8]) {
		let mut section = vec![table_id, 0xb0, 0, (extension >> 8) as u8, extension as u8, 0xc1, 0, 0];
		section.extend_from_slice(body);
		let len = section.len() - 3 + 4;
		section[1] |= (len >> 8) as u8;
		section[2] = len as u8;
		section.extend_from_slice(&MPEG_2.checksum(&section).to_be_bytes());
		let mut payload = vec![0];
		payload.extend_from_slice(&section);
		self.packet(pid, true, &payload);
	}

	fn pcr(&mut self, pid: u16, ticks: u64) {
		let base = ticks / 300;
		let ext = ticks % 300;
		let pcr = [
			(base >> 25) as u8,
			(base >> 17) as u8,
			(base >> 9) as u8,
			(base >> 1) as u8,
			((base & 1) << 7) as u8 | 0x7e | (ext >> 8) as u8,
			ext as u8,
		];
		// Adaptation only, so the continuity counter holds.
		let cc = self.cc.entry(pid).or_default();
		let mut packet = vec![0x47, (pid >> 8) as u8, pid as u8, 0x20 | *cc, 183, 0x10];
		packet.extend_from_slice(&pcr);
		packet.resize(188, 0xff);
		self.out.extend_from_slice(&packet);
	}

	/// A whole private-data PES of one packet, with no PTS.
	fn pes(&mut self, pid: u16, fill: u8) {
		let mut pes = vec![0, 0, 1, 0xbd, 0, 0, 0x84, 0, 0];
		pes.resize(184, fill);
		let len = pes.len() - 6;
		pes[4] = (len >> 8) as u8;
		pes[5] = len as u8;
		self.packet(pid, true, &pes);
	}
}

/// `programs` programs, each a clock and one private-data stream, alongside `strays` PIDs no
/// table lists, sent as null packets instead when `null`.
fn mpts(programs: u16, strays: u16, null: bool) -> Vec<u8> {
	let mut mux = Mux::default();
	let pat: Vec<u8> = (1..=programs)
		.flat_map(|program| {
			let pmt = 0x20 + program;
			[(program >> 8) as u8, program as u8, 0xe0 | (pmt >> 8) as u8, pmt as u8]
		})
		.collect();
	for interval in 0..INTERVALS {
		mux.section(0, 0x00, 1, &pat);
		for program in 1..=programs {
			let (pmt, clock, data) = (0x20 + program, 0x100 + program, 0x400 + program);
			let mut body = vec![0xe0 | (clock >> 8) as u8, clock as u8, 0xf0, 0];
			body.extend_from_slice(&[0x06, 0xe0 | (data >> 8) as u8, data as u8, 0xf0, 0]);
			mux.section(pmt, 0x02, program, &body);
			mux.pcr(clock, interval * INTERVAL);
			mux.pes(data, interval as u8);
		}
		for stray in 0..strays {
			mux.packet(if null { NULL } else { STRAY + stray }, false, &[0xff; 184]);
		}
	}
	mux.out
}

fn split(c: &mut Criterion) {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap();
	let _guard = runtime.enter();
	let mut group = c.benchmark_group("ts_programs");
	group.measurement_time(Duration::from_secs(1));
	group.warm_up_time(Duration::from_millis(200));
	group.sample_size(10);
	for programs in [1u16, 4, 16] {
		for strays in [1u16, 16, 256] {
			for (lane, null) in [("null", true), ("stray", false)] {
				let data = mpts(programs, strays, null);
				let (origin, driver) = moq_net::origin::Producer::new(Default::default());
				std::mem::forget(driver);
				let mut check = moq_mux::container::ts::Programs::new(origin, "bench", Default::default());
				check.decode(&data).unwrap();
				let counted = check.stats().streams.values().filter(|row| row.refused > 0).count();
				assert_eq!(
					counted,
					if null { 0 } else { strays as usize },
					"{lane} {programs}x{strays}"
				);
				group.throughput(Throughput::Bytes(data.len() as u64));
				let id = BenchmarkId::new(lane, format!("{programs}x{strays}"));
				group.bench_function(id, |b| {
					b.iter_batched(
						|| {
							let (origin, driver) = moq_net::origin::Producer::new(Default::default());
							std::mem::forget(driver);
							moq_mux::container::ts::Programs::new(origin, "bench", Default::default())
						},
						|mut split| {
							split.decode(&data).unwrap();
							split
						},
						criterion::BatchSize::PerIteration,
					);
				});
			}
		}
	}
	group.finish();
}

criterion_group!(benches, split);
criterion_main!(benches);
