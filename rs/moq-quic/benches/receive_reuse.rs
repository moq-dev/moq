//! Receive state across bursts: what a long-lived connection keeps once a burst of streams ends,
//! and what it pays when traffic returns.
//!
//! Every client connection opens `streams` uni streams of `packets` packets each and ends them.
//! `ordered` buffers the whole burst before the server's reader catches up, as a reader under
//! backpressure does; `loss` reads as data arrives while every eighth datagram is lost, so data
//! waits behind holes; `cancel` buffers the burst unread, then the reader stops every stream.
//! The connections then go quiet, resume with one packet per stream, and repeat the burst.
//!
//! The table, printed first, reads a counting allocator, so its heap columns are what the
//! application owns rather than the pages the allocator keeps resident, which the process-wide
//! `rss_kib` shows. `peak_kib` is the whole process at the burst's peak. The `server_` columns
//! are what dropping the server's connections frees: when new, when quiet after one burst, and
//! when quiet after the resume and a repeat burst. `kept_b` is what quiet holds over new, per
//! stream of the burst. The CPU columns time the server; the allocation counts include the
//! clients', which a receive change leaves alone. `teardown_b` is what the case leaks.
//!
//! Criterion then times the server's side of a burst on a connection that already recycled the
//! previous one, so both reuse and regrowth show.
//!
//! Run with `cargo bench -p moq-quic --features fuzz --bench receive_reuse`.

use std::{
	alloc::{GlobalAlloc, Layout, System},
	sync::atomic::{AtomicIsize, AtomicUsize, Ordering},
	time::Duration,
};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group};
use moq_quic::stream::Dir;

#[allow(
	dead_code,
	reason = "a harness shared by several benches, each using only part of it"
)]
mod common;

use common::{Pair, Setup, Sink, ThreadCpu, payload};

/// Stream data per packet, so a stream of `n` packets spans about `n` datagrams.
const PACKET: usize = 1100;

/// Packets written between exchanges, about what one socket receive batch carries.
const BATCH: usize = 64;

/// The most packets one table burst may send across every connection, so the widest sweeps
/// skip the longest streams rather than buffer hundreds of megabytes.
const BURST_PACKETS: usize = 1 << 16;

struct Counter;

/// Bytes allocated and not yet freed.
static LIVE: AtomicIsize = AtomicIsize::new(0);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

#[global_allocator]
static ALLOCATOR: Counter = Counter;

// Counting only. Every call forwards to the system allocator unchanged.
unsafe impl GlobalAlloc for Counter {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
		LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed);
		unsafe { System.alloc(layout) }
	}

	unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
		ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
		LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed);
		unsafe { System.alloc_zeroed(layout) }
	}

	unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
		ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
		LIVE.fetch_add(new_size as isize - layout.size() as isize, Ordering::Relaxed);
		unsafe { System.realloc(ptr, layout, new_size) }
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		LIVE.fetch_sub(layout.size() as isize, Ordering::Relaxed);
		unsafe { System.dealloc(ptr, layout) }
	}
}

fn live() -> isize {
	LIVE.load(Ordering::Relaxed)
}

fn allocations() -> usize {
	ALLOCATIONS.load(Ordering::Relaxed)
}

/// Resident set size in KiB, or zero where `/proc` does not report it.
fn rss_kib() -> u64 {
	let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
	status
		.lines()
		.find_map(|line| line.strip_prefix("VmRSS:"))
		.and_then(|rest| rest.trim().trim_end_matches("kB").trim().parse().ok())
		.unwrap_or(0)
}

/// How the server's application meets a burst.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
	Ordered,
	Loss,
	Cancel,
}

impl Mode {
	const ALL: [Self; 3] = [Self::Ordered, Self::Loss, Self::Cancel];

	fn name(self) -> &'static str {
		match self {
			Self::Ordered => "ordered",
			Self::Loss => "loss",
			Self::Cancel => "cancel",
		}
	}
}

/// Opens `streams` streams of `packets` packets on every client, finishes them, and runs until
/// the server's application has read or stopped them all, so their receive state is recycled.
///
/// Returns the heap the process held with the burst at its peak.
fn burst(pair: &mut Pair, mode: Mode, streams: usize, packets: usize) -> isize {
	pair.lose_every = (mode == Mode::Loss).then_some(8);
	pair.server.sink = match mode {
		Mode::Loss => Sink::Read,
		Mode::Ordered | Mode::Cancel => Sink::Ignore,
	};
	open(pair, streams, packets);
	let peak = match mode {
		Mode::Loss => {
			finish(pair, streams);
			live()
		}
		Mode::Ordered | Mode::Cancel => {
			// Every byte has arrived once the clients forget their acknowledged streams.
			pair.drive_until(|pair| {
				pair.clients
					.iter_mut()
					.all(|client| client.conn().streams().send_streams() == 0)
			});
			let peak = live();
			let start = cpu_time::ThreadTime::now();
			for conn in pair.server.conns.values_mut() {
				while let Some(id) = conn.streams().accept(Dir::Uni) {
					let mut stream = conn.recv_stream(id);
					if mode == Mode::Cancel {
						stream.stop(0u32.into()).unwrap();
						continue;
					}
					let mut chunks = stream.read().unwrap();
					loop {
						match chunks.next(usize::MAX) {
							Ok(Some(chunk)) => pair.server.received.bytes += chunk.bytes.len() as u64,
							Ok(None) => break,
							Err(err) => panic!("read failed: {err}"),
						}
					}
					pair.server.received.finished += 1;
					let _ = chunks.finalize();
				}
			}
			pair.server.busy += start.elapsed();
			peak
		}
	};
	settle(pair);
	peak
}

/// Traffic returning: one packet per stream on every client, read as it arrives.
fn resume(pair: &mut Pair, streams: usize) {
	pair.lose_every = None;
	pair.server.sink = Sink::Read;
	open(pair, streams, 1);
	finish(pair, streams);
	settle(pair);
}

/// Opens `streams` streams on every client, writes `packets` packets to each a packet per stream
/// at a time, then finishes them.
///
/// The exchange steps every [`BATCH`] packets, so the server takes the burst a receive batch at a
/// time, as a socket driver does, rather than every datagram before its application runs.
fn open(pair: &mut Pair, streams: usize, packets: usize) {
	let data = payload(PACKET);
	let ids: Vec<Vec<_>> = pair
		.clients
		.iter_mut()
		.map(|client| {
			let conn = client.conn();
			(0..streams)
				.map(|_| conn.streams().open(Dir::Uni, Default::default()).expect("stream limit"))
				.collect()
		})
		.collect();
	let mut written = 0;
	for _ in 0..packets {
		for (client, ids) in ids.iter().enumerate() {
			for &id in ids {
				let mut stream = pair.clients[client].conn().send_stream(id);
				assert_eq!(stream.write(&mut data.clone()).unwrap(), PACKET);
				written += 1;
				if written % BATCH == 0 {
					pair.step();
				}
			}
		}
	}
	for (client, ids) in ids.iter().enumerate() {
		for &id in ids {
			pair.clients[client].conn().send_stream(id).finish().unwrap();
		}
	}
}

/// Runs until the server's application has read `streams` more streams per client to the end.
fn finish(pair: &mut Pair, streams: usize) {
	let target = pair.server.received.finished + (pair.clients.len() * streams) as u64;
	pair.drive_until(|pair| pair.server.received.finished >= target);
}

/// Lets the exchange go quiet, checking every burst stream's receive state was recycled.
fn settle(pair: &mut Pair) {
	pair.settle();
	for conn in pair.server.conns.values_mut() {
		assert_eq!(
			conn.streams().remote_open_streams(Dir::Uni),
			0,
			"a burst stream outlived it"
		);
	}
}

/// The allocations and server CPU time `f` takes.
///
/// The count includes the clients' allocations, which a receive change leaves alone.
fn measure(pair: &mut Pair, f: impl FnOnce(&mut Pair)) -> (usize, Duration) {
	pair.server.busy = Duration::ZERO;
	let before = allocations();
	f(pair);
	(allocations() - before, pair.server.busy)
}

/// Heap the server's connections own, found by dropping them, so neither the clients nor the
/// harness's datagram queues count.
fn server_heap(mut pair: Pair) -> isize {
	let before = live();
	drop(std::mem::take(&mut pair.server.conns));
	before - live()
}

fn table() {
	println!(
		"mode conns streams packets peak_kib server_cold_kib server_quiet_kib kept_b resume_allocs resume_cpu_us repeat_allocs repeat_cpu_us server_repeat_kib teardown_b rss_kib"
	);
	let kib = |bytes: isize| bytes / 1024;
	for mode in Mode::ALL {
		for conns in [1, 4] {
			for streams in [16, 256, 1024] {
				for packets in [1, 16, 256, 2048] {
					if conns * streams * packets > BURST_PACKETS {
						continue;
					}
					let base = live();
					let setup = || Setup {
						clients: conns,
						..Default::default()
					};
					let cold = server_heap(Pair::new(setup()));

					let mut pair = Pair::new(setup());
					let peak = burst(&mut pair, mode, streams, packets) - base;
					let quiet = server_heap(pair);

					let mut pair = Pair::new(setup());
					burst(&mut pair, mode, streams, packets);
					let (resume_allocs, resume_cpu) = measure(&mut pair, |pair| resume(pair, streams));
					let (repeat_allocs, repeat_cpu) = measure(&mut pair, |pair| {
						burst(pair, mode, streams, packets);
					});
					let repeat = server_heap(pair);
					let teardown = live() - base;

					let kept = (quiet - cold) / (conns * streams) as isize;
					println!(
						"{} {conns} {streams} {packets} {} {} {} {kept} {resume_allocs} {} {repeat_allocs} {} {} {teardown} {}",
						mode.name(),
						kib(peak),
						kib(cold),
						kib(quiet),
						resume_cpu.as_micros(),
						repeat_cpu.as_micros(),
						kib(repeat),
						rss_kib(),
					);
				}
			}
		}
	}
}

/// The server's side of a burst on one connection that recycled the previous burst's streams.
fn bench_reuse(c: &mut Criterion<ThreadCpu>) {
	let mut group = c.benchmark_group("receive_reuse");
	for mode in Mode::ALL {
		for streams in [16, 256] {
			for packets in [16, 256] {
				let mut pair = None;
				group.throughput(Throughput::Bytes((streams * packets * PACKET) as u64));
				group.bench_function(
					BenchmarkId::new(
						format!("{}/streams_{streams}", mode.name()),
						format!("packets_{packets}"),
					),
					|b| {
						let pair = pair.get_or_insert_with(|| {
							let mut pair = Pair::new(Setup::default());
							burst(&mut pair, mode, streams, packets);
							pair
						});
						b.iter_custom(|iters| {
							let mut busy = Duration::ZERO;
							for _ in 0..iters {
								pair.server.busy = Duration::ZERO;
								burst(pair, mode, streams, packets);
								busy += pair.server.busy;
							}
							busy
						});
					},
				);
			}
		}
	}
	group.finish();
}

criterion_group! {
	name = benches;
	config = common::config();
	targets = bench_reuse
}

fn main() {
	table();
	benches();
	Criterion::default().configure_from_args().final_summary();
}
