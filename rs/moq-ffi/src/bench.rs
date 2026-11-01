//! The Rust baselines for the binding benchmark (see `bench/README.md`).
//!
//! Every wrapper runs the same loop, so dividing a wrapper's numbers by these isolates
//! what it adds: `rust-ffi` calls the exported API without crossing the FFI, and `rust-net`
//! runs the same rounds on moq-net, below moq-ffi's handles and runtime.

use crate::consumer::MoqTrackConsumer;
use crate::media::MoqFrame;
use crate::origin::{MoqOriginConfig, MoqOriginProducer};
use crate::producer::{MoqBroadcastProducer, MoqTrackInfo, MoqTrackProducer};

use std::future::Future;
use std::hint::black_box;
use std::sync::Arc;
use std::time::{Duration, Instant};

const WARMUP: u64 = 3;

/// Far longer than any honest round, so only a wedged read trips it.
const ROUND_TIMEOUT: Duration = Duration::from_secs(30);

/// Each case's origin cache budget: far above one round's groups, so eviction only
/// reclaims groups every subscriber has read, and memory stays flat over a long run.
const CACHE_BYTES: u64 = 64 << 20;

/// One throughput case: `publishers` tracks, each read by `subscribers` consumers.
struct Case {
	size: usize,
	publishers: usize,
	subscribers: usize,
}

impl Case {
	/// Frame sizes at one publisher and subscriber, then fan-out at 1 KiB.
	fn all() -> Vec<Case> {
		let sizes = [100, 1024, 64 * 1024, 1024 * 1024].map(|size| Case {
			size,
			publishers: 1,
			subscribers: 1,
		});
		let fanout = [(1, 4), (4, 1), (4, 4)].map(|(publishers, subscribers)| Case {
			size: 1024,
			publishers,
			subscribers,
		});
		sizes.into_iter().chain(fanout).collect()
	}

	/// Frames per group, holding a group under moq-net's 32 MB group cache.
	fn frames(&self) -> usize {
		((8 << 20) / self.size).clamp(1, 64)
	}

	fn delivered(&self) -> u64 {
		(self.publishers * self.subscribers * self.frames() * self.size) as u64
	}

	/// A non-uniform payload, so a zero-filled or truncated copy fails the check.
	fn payload(&self) -> Vec<u8> {
		(0..self.size).map(|i| (i % 251) as u8).collect()
	}

	fn name(&self) -> String {
		format!("size={}/pub={}/sub={}", self.size, self.publishers, self.subscribers)
	}
}

fn budget() -> Duration {
	let seconds = std::env::var("MOQ_BENCH_SECONDS").map_or(1.0, |s| s.parse().expect("MOQ_BENCH_SECONDS"));
	Duration::from_secs_f64(seconds)
}

/// Process CPU time, user plus system, across every thread.
fn cpu() -> Duration {
	let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
	// SAFETY: getrusage fills the struct it is handed and returns non-zero only for a bad `who`.
	let usage = unsafe {
		assert_eq!(libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()), 0);
		usage.assume_init()
	};
	let time = |t: libc::timeval| Duration::new(t.tv_sec as u64, t.tv_usec as u32 * 1000);
	time(usage.ru_utime) + time(usage.ru_stime)
}

#[derive(Default)]
struct Report(Vec<serde_json::Value>);

impl Report {
	fn push(&mut self, name: String, unit: &str, value: f64) {
		self.0
			.push(serde_json::json!({ "name": name, "unit": unit, "value": value }));
	}

	/// Run `round` for the time budget after a warmup, recording throughput and CPU per MB.
	async fn throughput<F, Fut>(&mut self, case: &Case, mut round: F)
	where
		F: FnMut(u64) -> Fut,
		Fut: Future<Output = ()>,
	{
		// The test runs many budgets back to back, so a wedged read is caught per round, not by nextest.
		let mut run = |n| kio::clock::timeout(ROUND_TIMEOUT, round(n));
		for n in 0..WARMUP {
			run(n).await.expect("round wedged");
		}
		let (start, cpu_start, budget) = (Instant::now(), cpu(), budget());
		let mut n = WARMUP;
		while start.elapsed() < budget {
			run(n).await.expect("round wedged");
			n += 1;
		}
		let (wall, cpu) = (start.elapsed(), cpu() - cpu_start);
		let mb = ((n - WARMUP) * case.delivered()) as f64 / 1e6;
		self.push(format!("throughput/{}", case.name()), "MB/s", mb / wall.as_secs_f64());
		self.push(format!("cpu/{}", case.name()), "ms/MB", cpu.as_secs_f64() * 1e3 / mb);
	}

	/// Time `call` over the budget, recording nanoseconds per call.
	fn call(&mut self, name: &str, mut call: impl FnMut()) {
		let (start, budget) = (Instant::now(), budget());
		let mut calls = 0u64;
		while start.elapsed() < budget {
			for _ in 0..1000 {
				call();
			}
			calls += 1000;
		}
		self.push(
			format!("call/{name}"),
			"ns",
			start.elapsed().as_nanos() as f64 / calls as f64,
		);
	}

	/// Write the report to `$MOQ_BENCH_DIR/<runner>.json`, or print it without one.
	fn write(self, runner: &str) {
		let json = serde_json::to_string_pretty(&self.0).unwrap();
		match std::env::var_os("MOQ_BENCH_DIR") {
			Some(dir) => {
				std::fs::create_dir_all(&dir).unwrap();
				std::fs::write(std::path::Path::new(&dir).join(format!("{runner}.json")), json).unwrap();
			}
			None => println!("{runner}: {json}"),
		}
	}
}

fn check(case: &Case, round: u64, frame: usize, want: &[u8], payload: &[u8], timestamp_us: Option<u64>) {
	assert!(payload == want, "corrupt frame {frame} of round {round}");
	assert_eq!(timestamp_us, Some(round * case.frames() as u64 + frame as u64));
}

async fn ffi_round(
	case: &Case,
	payload: &[u8],
	tracks: &[Arc<MoqTrackProducer>],
	consumers: &[Arc<MoqTrackConsumer>],
	n: u64,
) {
	let readers: Vec<_> = consumers
		.iter()
		.map(|consumer| {
			let (consumer, frames) = (consumer.clone(), case.frames());
			tokio::spawn(async move {
				let group = consumer.recv_group().await.unwrap().expect("track ended");
				let mut read = Vec::with_capacity(frames);
				for _ in 0..frames {
					let frame = group.read_frame().await.unwrap().expect("group ended");
					read.push((frame.payload, frame.timestamp_us));
				}
				read
			})
		})
		.collect();
	for track in tracks {
		let group = track.append_group().unwrap();
		for f in 0..case.frames() {
			let frame = MoqFrame {
				payload: payload.to_vec(),
				timestamp_us: Some(n * case.frames() as u64 + f as u64),
			};
			group.write_frame(frame).unwrap();
		}
		group.finish().unwrap();
	}
	for reader in readers {
		for (f, (read, timestamp_us)) in reader.await.unwrap().into_iter().enumerate() {
			check(case, n, f, payload, &read, timestamp_us);
		}
	}
}

/// Subscribers are not shared handles, so each reader task borrows one for the round and returns it.
async fn net_round(
	case: &Case,
	payload: &[u8],
	mut tracks: Vec<moq_net::track::Producer>,
	consumers: Vec<moq_net::track::Subscriber>,
	n: u64,
) -> Vec<moq_net::track::Subscriber> {
	let readers: Vec<_> = consumers
		.into_iter()
		.map(|mut consumer| {
			let frames = case.frames();
			tokio::spawn(async move {
				let mut group = consumer.recv_group().await.unwrap().expect("track ended");
				let mut read = Vec::with_capacity(frames);
				for _ in 0..frames {
					let frame = group.read_frame().await.unwrap().expect("group ended");
					read.push((frame.payload, frame.timestamp.map(|t| t.as_micros() as u64)));
				}
				(consumer, read)
			})
		})
		.collect();
	for track in &mut tracks {
		let mut group = track.append_group().unwrap();
		for f in 0..case.frames() {
			let timestamp = moq_net::Timestamp::from_micros(n * case.frames() as u64 + f as u64).unwrap();
			// Copy, as a binding's write does, so the baseline does not get zero-copy for free.
			group.write_frame(timestamp, payload.to_vec()).unwrap();
		}
		group.finish().unwrap();
	}
	let mut consumers = Vec::with_capacity(readers.len());
	for reader in readers {
		let (consumer, read) = reader.await.unwrap();
		for (f, (read, timestamp_us)) in read.into_iter().enumerate() {
			check(case, n, f, payload, &read, timestamp_us);
		}
		consumers.push(consumer);
	}
	consumers
}

async fn rust_ffi() {
	let mut report = Report::default();
	for case in Case::all() {
		let origin = MoqOriginProducer::new(MoqOriginConfig {
			cache_capacity_bytes: Some(CACHE_BYTES),
		});
		let broadcast = origin.create_broadcast("bench".into()).unwrap().broadcast();
		let timed = MoqTrackInfo {
			priority: 0,
			max_age: None,
			refresh: None,
			timescale: Some(1_000_000),
			compress: false,
		};
		let tracks: Vec<_> = (0..case.publishers)
			.map(|p| broadcast.publish_track(p.to_string(), Some(timed.clone())).unwrap())
			.collect();
		let consumers: Vec<_> = tracks
			.iter()
			.flat_map(|track| (0..case.subscribers).map(|_| track.consume(None).unwrap()))
			.collect();
		let payload = case.payload();
		report
			.throughput(&case, |n| ffi_round(&case, &payload, &tracks, &consumers, n))
			.await;
	}

	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast.publish_track("calls".into(), None).unwrap();
	let group = track.append_group().unwrap().consume().unwrap();
	report.call("sequence", || {
		black_box(black_box(&group).sequence());
	});
	let consumer = track.consume(None).unwrap();
	report.call("info", || {
		black_box(black_box(&consumer).info());
	});
	report.write("rust-ffi");
}

async fn rust_net() {
	let mut report = Report::default();
	for case in Case::all() {
		let mut config = moq_net::origin::Config::default();
		let cache = moq_net::cache::Config::default()
			.with_capacity(CACHE_BYTES)
			.with_expiry(config.pool.expiry());
		config.pool = moq_net::cache::Pool::new(cache);
		let (origin, driver) = moq_net::origin::Producer::new(config);
		let driver = tokio::spawn(moq_net::time::run(driver));
		let broadcast = origin.create_broadcast("bench").unwrap();
		let info = moq_net::track::Info::default().with_timescale(moq_net::Timescale::MICRO);
		let tracks: Vec<_> = (0..case.publishers)
			.map(|p| broadcast.create_track(p.to_string(), Some(info.clone())).unwrap())
			.collect();
		let consumers = std::cell::RefCell::new(
			tracks
				.iter()
				.flat_map(|track| (0..case.subscribers).map(|_| track.subscribe(None)))
				.collect::<Vec<_>>(),
		);
		let payload = case.payload();
		// Producers are shared handles, so each round appends through its own clones.
		let (case, payload, tracks, consumers) = (&case, &payload, &tracks, &consumers);
		report
			.throughput(case, |n| async move {
				let returned = net_round(case, payload, tracks.clone(), consumers.take(), n).await;
				consumers.replace(returned);
			})
			.await;
		driver.abort();
	}
	report.write("rust-net");
}

#[tokio::test]
#[ignore = "benchmark: see bench/README.md"]
async fn benchmark_ffi() {
	rust_ffi().await;
	rust_net().await;
}
