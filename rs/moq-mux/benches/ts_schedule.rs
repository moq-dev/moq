//! TS admission and layout across PID count and access units per PID.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use std::time::Duration;

mod export {
	pub const PCR_INTERVAL: Duration = Duration::from_millis(25);
	use std::time::Duration;
}
#[path = "../src/container/ts/schedule.rs"]
#[allow(dead_code)]
mod schedule;

fn packet(pid: u16) -> [u8; 188] {
	let mut packet = [0xff; 188];
	packet[..4].copy_from_slice(&[0x47, (pid >> 8) as u8, pid as u8, 0x10]);
	packet
}

fn admission(c: &mut Criterion) {
	let mut group = c.benchmark_group("ts_schedule");
	group.measurement_time(Duration::from_secs(1));
	group.warm_up_time(Duration::from_millis(200));
	group.sample_size(10);
	for pids in [1u16, 4, 16, 64] {
		for frames in [1u128, 16, 64] {
			group.bench_function(BenchmarkId::new(pids.to_string(), frames), |b| {
				b.iter(|| {
					let mut schedule = schedule::Schedule::new(Duration::from_millis(200));
					schedule.set_rate(Some(100_000_000));
					schedule.set_clock(1);
					for pid in 1..=pids {
						schedule.set_buffer(
							pid,
							schedule::Buffer {
								rate: 2_000_000,
								leak: None,
								size: Some(1_000_000),
								transport: 512,
							},
						);
					}
					for frame in 0..frames {
						for pid in 1..=pids {
							let at = (999 + frame * 40) * 1_000_000;
							schedule
								.push_frames(
									pid,
									schedule::Timing {
										by: at,
										decode: at + 1_000_000,
									},
									packet(pid).repeat(10),
									false,
									Vec::new(),
								)
								.unwrap();
						}
					}
					while let Some(slot) = schedule.next(None).unwrap() {
						black_box(slot.layout(&packet(1), 100, &packet(0x1fff)));
					}
				});
			});
		}
	}
	group.finish();
}
criterion_group!(benches, admission);
criterion_main!(benches);
