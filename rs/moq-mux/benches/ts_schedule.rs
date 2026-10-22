//! TS admission and layout across PID count and access units per PID, padded layout across
//! queued tracks and access-unit size, and RTP packetization across rate and unsure share.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use std::time::Duration;

mod export {
	pub const PCR_INTERVAL: Duration = Duration::from_millis(25);
	pub const PSI_INTERVAL: Duration = Duration::from_millis(500);
	use std::time::Duration;
}
#[path = "../src/container/ts/lane.rs"]
#[allow(dead_code)]
mod lane;
#[path = "../src/container/ts/rtp.rs"]
#[allow(dead_code)]
mod rtp;
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
								reach: Duration::from_secs(1),
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

/// Padded layout across queued tracks and access-unit size, each track's first unit with the
/// tables ahead of it, the larger units spanning several slots.
fn tables(c: &mut Criterion) {
	let mut group = c.benchmark_group("ts_schedule_tables");
	group.measurement_time(Duration::from_secs(1));
	group.warm_up_time(Duration::from_millis(200));
	group.sample_size(10);
	let rate = 100_000_000u64;
	for tracks in [1u16, 4, 16] {
		for size in [100usize, 1_000, 4_000] {
			group.bench_function(BenchmarkId::new(tracks.to_string(), size), |b| {
				b.iter(|| {
					let mut schedule = schedule::Schedule::new(Duration::from_secs(2));
					schedule.set_rate(Some(rate));
					schedule.set_clock(1);
					schedule.set_tables(0x1000);
					for pid in 1..=tracks {
						schedule.set_buffer(
							pid,
							schedule::Buffer {
								rate,
								leak: None,
								size: Some(1_000_000),
								transport: 512,
								reach: Duration::from_secs(2),
							},
						);
					}
					// Due one after another, spaced for the multiplex to carry them at about 70 %.
					let spacing = (size as u128 + 2) * 188 * 8 * 1_000_000_000 / u128::from(rate) * 10 / 7;
					for frame in 0..4u128 {
						for pid in 1..=tracks {
							let at = 1_000_000_000 + (frame * u128::from(tracks) + u128::from(pid)) * spacing;
							let mut packets = match frame {
								0 => [packet(0), packet(0x1000)].concat(),
								_ => Vec::new(),
							};
							packets.extend(packet(pid).repeat(size));
							schedule
								.push_frames(
									pid,
									schedule::Timing {
										by: at,
										decode: at + 1_000_000,
									},
									packets,
									frame == 0,
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

/// RTP packetization of a synced multiplex across its rate and the share of each slot's
/// positions the leg is unsure of: none, one in 64, every one.
fn rtp(c: &mut Criterion) {
	let mut group = c.benchmark_group("ts_rtp");
	group.measurement_time(Duration::from_secs(1));
	group.warm_up_time(Duration::from_millis(200));
	group.sample_size(10);
	for rate in [10_000_000u64, 100_000_000, 1_000_000_000] {
		let layout =
			std::sync::Arc::new(lane::Layout::new(rate, 2, 0, &[(0x100, lane::Need::Rest(u64::MAX))]).unwrap());
		let slots: Vec<Vec<u8>> = (0..64).map(|index| packet(0x100).repeat(layout.count(index))).collect();
		for every in [0usize, 64, 1] {
			let unsure: Vec<Vec<usize>> = slots
				.iter()
				.map(|slot| match every {
					0 => Vec::new(),
					every => (0..slot.len() / 188).step_by(every).collect(),
				})
				.collect();
			let mbps = rate / 1_000_000;
			group.bench_function(BenchmarkId::new(format!("{mbps}Mb/s"), every), |b| {
				b.iter(|| {
					let mut packetizer = rtp::Packetizer::new(layout.clone(), 0x1234_5678);
					for (index, (slot, unsure)) in slots.iter().zip(&unsure).enumerate() {
						black_box(packetizer.push(index as u128, slot, unsure));
					}
				});
			});
		}
	}
	group.finish();
}

criterion_group!(benches, admission, tables, rtp);
criterion_main!(benches);
