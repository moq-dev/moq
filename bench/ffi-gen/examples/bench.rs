//! The Rust floor: py/bench.py's rounds and calls on the slice's API, with no boundary.
//!
//! Built with the `uniffi` feature only because the slice needs one generator's markers
//! to compile; nothing here crosses the FFI.

use std::hint::black_box;
use std::time::{Duration, Instant};

use moq_ffi_gen::*;

const WARMUP: u64 = 3;
const CACHE_BYTES: u64 = 64 << 20;
const SIZES: [usize; 5] = [100, 1024, 16 * 1024, 64 * 1024, 1024 * 1024];

fn budget() -> Duration {
	let seconds = std::env::var("MOQ_BENCH_SECONDS").map_or(1.0, |s| s.parse().expect("MOQ_BENCH_SECONDS"));
	Duration::from_secs_f64(seconds)
}

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

fn frames(size: usize) -> u64 {
	((8 << 20) / size).clamp(1, 64) as u64
}

struct Report(Vec<String>);

impl Report {
	fn push(&mut self, name: String, unit: &str, value: f64) {
		self.0
			.push(format!(r#"{{"name": "{name}", "unit": "{unit}", "value": {value}}}"#));
	}

	async fn throughput(&mut self, size: usize) {
		let origin = MoqOriginProducer::new(CACHE_BYTES);
		let track = origin
			.create_broadcast("bench".into())
			.unwrap()
			.publish_track("0".into())
			.unwrap();
		let consumer = std::sync::Arc::new(track.consume().unwrap());
		let payload: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
		let count = frames(size);

		let step = async |n: u64| {
			let reader = {
				let (consumer, payload) = (consumer.clone(), payload.clone());
				tokio::spawn(async move {
					let group = consumer.recv_group().await.unwrap();
					for f in 0..count {
						let frame = group.read_frame().await.unwrap().expect("group ended");
						assert!(frame.payload == payload, "corrupt frame {f} of round {n}");
						assert_eq!(frame.timestamp_us, Some(n * count + f));
					}
				})
			};
			let group = track.append_group().unwrap();
			for f in 0..count {
				let frame = MoqFrame {
					payload: payload.clone(),
					timestamp_us: Some(n * count + f),
				};
				group.write_frame(frame).unwrap();
			}
			group.finish().unwrap();
			reader.await.unwrap();
		};

		for n in 0..WARMUP {
			step(n).await;
		}
		let (start, cpu_start, budget) = (Instant::now(), cpu(), budget());
		let mut n = WARMUP;
		while start.elapsed() < budget {
			step(n).await;
			n += 1;
		}
		let (wall, used) = (start.elapsed(), cpu() - cpu_start);
		let mb = ((n - WARMUP) * count * size as u64) as f64 / 1e6;
		self.push(format!("throughput/{size}"), "MB/s", mb / wall.as_secs_f64());
		self.push(format!("cpu/{size}"), "ms/MB", used.as_secs_f64() * 1e3 / mb);
	}

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

	async fn call_async<F: Future<Output = ()>>(&mut self, name: &str, mut call: impl FnMut() -> F) {
		let (start, budget) = (Instant::now(), budget());
		let mut calls = 0u64;
		while start.elapsed() < budget {
			for _ in 0..1000 {
				call().await;
			}
			calls += 1000;
		}
		self.push(
			format!("call/{name}"),
			"ns",
			start.elapsed().as_nanos() as f64 / calls as f64,
		);
	}
}

async fn run() {
	let mut report = Report(Vec::new());
	for size in SIZES {
		report.throughput(size).await;
	}

	let origin = MoqOriginProducer::new(CACHE_BYTES);
	let track = origin
		.create_broadcast("calls".into())
		.unwrap()
		.publish_track("calls".into())
		.unwrap();
	let mut group = track.append_group().unwrap();
	let consumer = track.consume().unwrap();
	report.call("noop", || {
		noop();
		black_box(());
	});
	report.call("sequence", || {
		black_box(black_box(&group).sequence());
	});
	report.call("info", || {
		black_box(black_box(&consumer).info());
	});
	let mut writes = 0u64;
	report.call("write_frame/100", || {
		writes += 1;
		if writes.is_multiple_of(1000) {
			group.finish().unwrap();
			group = track.append_group().unwrap();
		}
		let frame = MoqFrame {
			payload: vec![0; 100],
			timestamp_us: Some(0),
		};
		group.write_frame(frame).unwrap();
	});
	report.call_async("noop_async", noop_async).await;
	report.call_async("yield_once", yield_once).await;

	println!(
		"{{\"runner\": \"rust\", \"results\": [\n    {}\n  ]}}",
		report.0.join(",\n    ")
	);
}

fn main() {
	tokio::runtime::Builder::new_current_thread()
		.build()
		.unwrap()
		.block_on(run());
}
