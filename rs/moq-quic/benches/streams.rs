//! Stream send and scheduling: one client writes many streams to a server that reads them all.
//!
//! Throughput cases open fresh unidirectional streams on a long-lived connection, write and
//! finish them, then exchange packets until the server has read them and the path is idle.
//! The zero-credit case isolates blocked writes and resets across live-writer counts, with
//! connection setup and dropping the remaining state outside the CPU measurement. The
//! reprioritize case moves streams that stay queued, never sending them. The group churn case
//! moves and cancels send groups, one per subscription, while the rest have data queued.

use bytes::Bytes;
use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_quic::stream::Dir;

#[allow(
	dead_code,
	reason = "a harness shared by several benches, each using only part of it"
)]
mod common;

use common::{Pair, Setup, ThreadCpu, payload};

#[allow(dead_code, reason = "the scheduler benchmark exercises only its queued send paths")]
#[path = "../src/connection/streams/schedule.rs"]
mod schedule;

fn bench_schedule(c: &mut Criterion<ThreadCpu>) {
	use schedule::{Next, Schedule, Slot};
	let mut group = c.benchmark_group("schedule");
	for groups in [1, 16, 256] {
		for queued in [1, 16, 256] {
			let mut schedule = Schedule::<usize, ()>::new();
			let ids: Vec<_> = (0..groups).map(|_| schedule.open_group(0)).collect();
			let mut slots: Vec<_> = (0..groups * queued).map(|_| Slot::default()).collect();
			for (id, slot) in slots.iter_mut().enumerate() {
				schedule.join(id, slot, Some(ids[id / queued]), 0);
				schedule.push(id, slot, 0);
			}
			group.bench_function(
				BenchmarkId::new("pop", format!("groups_{groups}_queued_{queued}")),
				|b| {
					b.iter(|| {
						let Some(Next::Stream(id)) = schedule.pop() else {
							panic!("stream remains queued")
						};
						schedule.sent(id, &mut slots[id], 1200, Some(0), true);
					});
				},
			);
			let mut schedule = Schedule::<usize, ()>::new();
			let ids: Vec<_> = (0..groups).map(|_| schedule.open_group(0)).collect();
			for &id in &ids {
				for _ in 0..queued {
					schedule.push_datagram(Some(id), 0, (), 100, usize::MAX);
				}
			}
			let budget = groups * queued * 100;
			let mut next = 0;
			group.bench_function(
				BenchmarkId::new("datagram_push", format!("groups_{groups}_queued_{queued}")),
				|b| {
					b.iter(|| {
						schedule.push_datagram(Some(ids[next % groups]), 0, (), 100, budget);
						next += 1;
					});
				},
			);
		}
	}

	// Moving or retiring some queued groups, as subscription updates and cancels do, while the
	// rest stay queued. Groups are picked from the front of the round robin, its tail, or spread
	// across it, so a cost that grows with the untouched groups shows as a slope.
	let queued_groups = |groups: usize| {
		let mut schedule = Schedule::<usize, ()>::new();
		let ids: Vec<_> = (0..groups).map(|_| schedule.open_group(0)).collect();
		let mut slots: Vec<_> = (0..groups).map(|_| Slot::default()).collect();
		for (id, slot) in slots.iter_mut().enumerate() {
			schedule.join(id, slot, Some(ids[id]), 0);
			schedule.push(id, slot, 0);
		}
		(schedule, ids, slots)
	};
	for (queued, changed) in [(16, 16), (256, 16), (4096, 16), (4096, 256)] {
		for order in ["front", "tail", "mixed"] {
			let picked: Vec<usize> = match order {
				"front" => (0..changed).collect(),
				"tail" => (queued - changed..queued).rev().collect(),
				// An odd stride visits distinct groups of a power-of-two count.
				_ => (0..changed).map(|i| i * 2_654_435_761 % queued).collect(),
			};
			let id = format!("queued_{queued}_changed_{changed}_{order}");
			group.throughput(Throughput::Elements(changed as u64));
			group.bench_function(BenchmarkId::new("group_priority", &id), |b| {
				b.iter_batched(
					|| queued_groups(queued),
					|(mut schedule, ids, slots)| {
						for &index in &picked {
							schedule.set_group_priority(ids[index], 1);
						}
						(schedule, slots)
					},
					BatchSize::LargeInput,
				)
			});
			group.bench_function(BenchmarkId::new("group_retire", &id), |b| {
				b.iter_batched(
					|| queued_groups(queued),
					|(mut schedule, ids, mut slots)| {
						for &index in &picked {
							schedule.close_group(ids[index]);
							schedule.leave(&mut slots[index]);
						}
						(schedule, slots)
					},
					BatchSize::LargeInput,
				)
			});
		}
	}
	group.finish();
}

/// How often a `holes` case loses a client datagram: often enough that most windows hold a hole.
const LOSE_EVERY: u64 = 16;

/// Opens `streams` streams across `priorities` levels, writes `writes` writes of `size` bytes to
/// each, and drives until the server has read them all.
fn send(pair: &mut Pair, streams: usize, priorities: usize, writes: usize, size: usize) {
	let data = payload(size);
	let target = pair.server.received.finished + streams as u64;
	let conn = pair.client();
	for index in 0..streams {
		let id = conn.streams().open(Dir::Uni, Default::default()).expect("stream limit");
		let mut stream = conn.send_stream(id);
		// `as _` follows the priority type, which a scheduler rewrite widens.
		stream.set_priority((index % priorities) as _).unwrap();
		for _ in 0..writes {
			assert_eq!(stream.write(&mut data.clone()).unwrap(), size, "write was cut short");
		}
		stream.finish().unwrap();
	}
	pair.drive_until(|pair| pair.server.received.finished >= target);
	pair.settle();
}

/// Stream count against priority levels, with a few packets per stream so they interleave.
fn bench_priority(c: &mut Criterion<ThreadCpu>) {
	const BYTES: usize = 4096;
	let mut group = c.benchmark_group("send_priority");
	for streams in [1, 16, 256, 1024] {
		for priorities in [1, 8, 64] {
			if priorities > streams {
				continue;
			}
			let mut pair = None;
			group.throughput(Throughput::Bytes((streams * BYTES) as u64));
			group.bench_function(
				BenchmarkId::new(format!("streams_{streams}"), format!("priorities_{priorities}")),
				|b| {
					let pair = pair.get_or_insert_with(|| Pair::new(Setup::default()));
					b.iter(|| send(pair, streams, priorities, 1, BYTES))
				},
			);
		}
	}
	group.finish();
}

/// Reprioritize every stream of a queue that is never drained, so each call moves a queued stream.
fn bench_reprioritize(c: &mut Criterion<ThreadCpu>) {
	let mut group = c.benchmark_group("set_priority");
	for queued in [16, 256, 4096] {
		let mut pair = Pair::new(Setup::default());
		let conn = pair.client();
		let ids: Vec<_> = (0..queued)
			.map(|_| {
				let id = conn.streams().open(Dir::Uni, Default::default()).expect("stream limit");
				assert_eq!(conn.send_stream(id).write(&mut &b"x"[..]).unwrap(), 1);
				id
			})
			.collect();
		let mut round = 0;
		group.throughput(Throughput::Elements(queued as u64));
		group.bench_function(BenchmarkId::new("queued", queued), |b| {
			b.iter(|| {
				round += 1;
				for (index, &id) in ids.iter().enumerate() {
					// A priority differing from the last round's, so no call is a no-op.
					let priority = ((index + round) % 64) as _;
					conn.send_stream(id).set_priority(priority).unwrap();
				}
			})
		});
	}
	group.finish();
}

/// Subscription churn on a shared connection: each round writes one small stream in each of
/// `subs` send groups, then, with all of it queued, moves `churn` groups spread across the
/// round robin to another priority and back and cancels `churn` others (reset the stream,
/// close the group, open its replacement), and drives until the server has seen every stream.
fn bench_group_churn(c: &mut Criterion<ThreadCpu>) {
	use moq_quic::send;
	const BYTES: usize = 256;
	let data = payload(BYTES);
	let mut group = c.benchmark_group("group_churn");
	for (subs, churn) in [(16_usize, 4), (256, 16), (4096, 16), (4096, 256)] {
		// The moved and cancelled groups stay distinct, below.
		assert!(2 * churn <= subs && subs.is_power_of_two());
		let mut pair = Pair::new(Setup::default());
		let mut groups: Vec<_> = (0..subs).map(|_| pair.client().send_groups().open(0)).collect();
		let mut round = 0;
		group.throughput(Throughput::Elements(subs as u64));
		group.bench_function(
			BenchmarkId::new(format!("subs_{subs}"), format!("churn_{churn}")),
			|b| {
				b.iter(|| {
					round += 1;
					let finished = pair.server.received.finished;
					let reset = pair.server.received.reset;
					let conn = pair.client();
					let ids: Vec<_> = groups
						.iter()
						.map(|group| {
							let options = send::Options {
								group: Some(group),
								priority: 0,
							};
							let id = conn.streams().open(Dir::Uni, options).expect("stream limit");
							let mut stream = conn.send_stream(id);
							assert_eq!(stream.write(&mut data.clone()).unwrap(), BYTES);
							stream.finish().unwrap();
							id
						})
						.collect();
					// An odd stride permutes a power-of-two count, spreading the touched groups over
					// the round robin and shifting them each round.
					let spread = |i: usize| (round + i * 2_654_435_761) % subs;
					for index in (0..churn).map(spread) {
						conn.send_groups().set_priority(&groups[index], 1);
						conn.send_groups().set_priority(&groups[index], 0);
					}
					for index in (churn..2 * churn).map(spread) {
						conn.send_stream(ids[index]).reset(0u32.into()).unwrap();
						let fresh = conn.send_groups().open(0);
						conn.send_groups().close(std::mem::replace(&mut groups[index], fresh));
					}
					let target = (finished + (subs - churn) as u64, reset + churn as u64);
					pair.drive_until(|pair| {
						let received = &pair.server.received;
						received.finished >= target.0 && received.reset >= target.1
					});
					pair.settle();
				})
			},
		);
	}
	group.finish();
}

/// Writes `total` bytes to one stream in `size`-byte writes, exchanging packets whenever the
/// client's send window is full, and drives until the server has read them all.
fn send_windowed(pair: &mut Pair, total: usize, size: usize) {
	let data = payload(size);
	let target = pair.server.received.finished + 1;
	let id = pair
		.client()
		.streams()
		.open(Dir::Uni, Default::default())
		.expect("stream limit");
	let mut written = 0;
	while written < total {
		// A write the window cuts short resumes mid-chunk, so every chunk boundary stays put.
		let start = written % size;
		match pair.client().send_stream(id).write(&mut data.slice(start..)) {
			Ok(n) => written += n,
			Err(moq_quic::send::Error::Blocked) => {
				pair.drive_until(|pair| pair.client().send_stream(id).write(&mut Bytes::new()).is_ok())
			}
			Err(err) => panic!("{err}"),
		}
	}
	pair.client().send_stream(id).finish().unwrap();
	pair.drive_until(|pair| pair.server.received.finished >= target);
	pair.settle();
}

/// Bytes per write against stream count, writing a fixed 64 KiB to each stream.
///
/// The `window` cases instead write a fixed 256 KiB to one stream through a send window, so the
/// data retained until acknowledged grows with the window while the total stays put. The
/// `holes` cases, at the largest window only to keep the nightly short, lose a datagram now and
/// then, so a hole keeps everything after it retained and its retransmission reads old offsets.
fn bench_write_size(c: &mut Criterion<ThreadCpu>) {
	const BYTES: usize = 64 << 10;
	const TOTAL: usize = 256 << 10;
	let mut group = c.benchmark_group("send_write_size");
	for streams in [1, 64] {
		for size in [64, 1024, 16384] {
			let mut pair = None;
			group.throughput(Throughput::Bytes((streams * BYTES) as u64));
			group.bench_function(
				BenchmarkId::new(format!("streams_{streams}"), format!("bytes_{size}")),
				|b| {
					let pair = pair.get_or_insert_with(|| Pair::new(Setup::default()));
					b.iter(|| send(pair, streams, 1, BYTES / size, size))
				},
			);
		}
	}
	let windows = [(16 << 10, false), (64 << 10, false), (TOTAL, false), (TOTAL, true)];
	for (window, holes) in windows {
		for size in [64, 1024, 16384] {
			let mut pair = None;
			let name = match holes {
				false => format!("window_{}k", window >> 10),
				true => format!("window_{}k_holes", window >> 10),
			};
			group.throughput(Throughput::Bytes(TOTAL as u64));
			group.bench_function(BenchmarkId::new(name, format!("bytes_{size}")), |b| {
				let pair = pair.get_or_insert_with(|| {
					let mut pair = Pair::new(Setup {
						client_send_window: window as u64,
						..Setup::default()
					});
					pair.lose_every = holes.then_some(LOSE_EVERY);
					pair
				});
				b.iter(|| send_windowed(pair, TOTAL, size))
			});
		}
	}
	group.finish();
}

/// Keep `live` writers blocked while `churn` others block and reset, with MAX_DATA spent and never raised.
/// A fresh connection per sample bounds even the unfixed implementation's retained history.
fn bench_connection_blocked(c: &mut Criterion<ThreadCpu>) {
	let mut group = c.benchmark_group("connection_blocked");
	for live in [1, 64, 1024] {
		for churn in [16, 256] {
			group.throughput(Throughput::Elements(churn));
			group.bench_function(
				BenchmarkId::new(format!("live_{live}"), format!("churn_{churn}")),
				|b| {
					b.iter_batched(
						|| {
							let mut pair = Pair::new(Setup {
								server_receive_window: 1u32.into(),
								..Setup::default()
							});
							// A window can't be zero, so one stream spends its only byte.
							let id = pair.client().streams().open(Dir::Uni, Default::default()).unwrap();
							assert_eq!(pair.client().send_stream(id).write(&mut &b"x"[..]), Ok(1));
							for _ in 0..live {
								let id = pair.client().streams().open(Dir::Uni, Default::default()).unwrap();
								assert_eq!(
									pair.client().send_stream(id).write(&mut &b"x"[..]),
									Err(moq_quic::send::Error::Blocked)
								);
							}
							pair
						},
						|mut pair| {
							for _ in 0..churn {
								let id = pair.client().streams().open(Dir::Uni, Default::default()).unwrap();
								let mut stream = pair.client().send_stream(id);
								assert_eq!(stream.write(&mut &b"x"[..]), Err(moq_quic::send::Error::Blocked));
								stream.reset(0u32.into()).unwrap();
							}
							// The peer never grants credit. Packet exchange and dropping the live
							// table are outside the measured insert/write/reset path.
							pair
						},
						BatchSize::PerIteration,
					)
				},
			);
		}
	}
	group.finish();
}

/// Probing against stream count: a client whose controller always lets it probe fills every gap
/// in its sends with duplicates of the data in flight, up to a 1 Gbit/s target, so the cost per
/// unique byte against `off` is what probing adds to a sender.
fn bench_probe(c: &mut Criterion<ThreadCpu>) {
	const BYTES: usize = 64 << 10;
	let mut group = c.benchmark_group("send_probe");
	for streams in [1, 16, 256] {
		for (label, target) in [("off", 0), ("on", 125_000_000)] {
			let mut pair = None;
			group.throughput(Throughput::Bytes((streams * BYTES) as u64));
			group.bench_function(
				BenchmarkId::new(format!("streams_{streams}"), format!("probe_{label}")),
				|b| {
					let pair = pair.get_or_insert_with(|| {
						Pair::new(Setup {
							client_probe_target: target,
							..Setup::default()
						})
					});
					b.iter(|| send(pair, streams, 1, 4, BYTES / 4))
				},
			);
		}
	}
	group.finish();
}

criterion_group! {
	name = benches;
	config = common::config();
	targets = bench_schedule, bench_priority, bench_reprioritize, bench_group_churn, bench_write_size, bench_connection_blocked, bench_probe
}
criterion_main!(benches);
