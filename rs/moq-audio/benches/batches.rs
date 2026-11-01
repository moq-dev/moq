//! PCM batching cost through [`encode::Producer`]: one constant duration of audio,
//! delivered in batches from tiny steady writes to a single burst, swept over channels,
//! input rates (with and without resampling) and independent producers.
//!
//! PCM is the codec, so buffering and resampling show instead of codec CPU; two Opus
//! cases put that cost beside a real codec's. Each case builds fresh producers, writes
//! every batch and finishes the track, so the partial tail and resampler flush count too.
//!
//! Run with `cargo bench -p moq-audio --bench batches`. A table comes first: user-space
//! instructions per input sample (Linux `perf_event_open`, `-` where unavailable),
//! allocations, allocated bytes and the slowest write of one counted pass. Criterion
//! then times each case.

use std::alloc::{GlobalAlloc, Layout as AllocLayout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group};
use moq_audio::encode::{Codec, Config, Input, Options, Producer};
use moq_audio::{Frame, Layout};
use moq_net::Timestamp;

/// Every case delivers this much audio, however it is batched.
const SECONDS: usize = 4;
/// The codec rate every case publishes at.
const CODEC_RATE: u32 = 48_000;

struct Counter;

static COUNTING: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED: AtomicU64 = AtomicU64::new(0);

#[global_allocator]
static ALLOCATOR: Counter = Counter;

impl Counter {
	fn count(size: usize) {
		if COUNTING.load(Ordering::Relaxed) {
			ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
			ALLOCATED.fetch_add(size as u64, Ordering::Relaxed);
		}
	}
}

unsafe impl GlobalAlloc for Counter {
	unsafe fn alloc(&self, layout: AllocLayout) -> *mut u8 {
		Self::count(layout.size());
		unsafe { System.alloc(layout) }
	}

	unsafe fn alloc_zeroed(&self, layout: AllocLayout) -> *mut u8 {
		Self::count(layout.size());
		unsafe { System.alloc_zeroed(layout) }
	}

	unsafe fn realloc(&self, ptr: *mut u8, layout: AllocLayout, new_size: usize) -> *mut u8 {
		Self::count(new_size);
		unsafe { System.realloc(ptr, layout, new_size) }
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: AllocLayout) {
		unsafe { System.dealloc(ptr, layout) }
	}
}

/// User-space instructions retired by this thread, for a count that repeats run to run.
#[cfg(target_os = "linux")]
mod instructions {
	use perf_event::events::Hardware;

	/// One counter per core PMU, because a hybrid CPU counts a generic event on one core
	/// type only. Retired instructions are architectural, so the sum is exact wherever the
	/// thread runs.
	pub struct Counter(Vec<perf_event::Counter>);

	impl Counter {
		pub fn new() -> Option<Self> {
			let hybrid: Vec<u64> = ["cpu_core", "cpu_atom"]
				.iter()
				.filter_map(|pmu| std::fs::read_to_string(format!("/sys/devices/{pmu}/type")).ok())
				.map(|kind| kind.trim().parse().ok())
				.collect::<Option<_>>()?;
			// The PMU type goes in the high bits, as `PERF_PMU_TYPE_SHIFT` describes.
			let events = match hybrid.is_empty() {
				true => vec![Hardware::INSTRUCTIONS],
				false => hybrid
					.into_iter()
					.map(|pmu| Hardware(pmu << 32 | Hardware::INSTRUCTIONS.0))
					.collect(),
			};
			let counters = events
				.into_iter()
				.map(|event| perf_event::Builder::new(event).build().ok())
				.collect::<Option<_>>()?;
			Some(Self(counters))
		}

		pub fn start(&mut self) {
			for counter in &mut self.0 {
				counter.reset().unwrap();
				counter.enable().unwrap();
			}
		}

		pub fn stop(&mut self) -> u64 {
			self.0
				.iter_mut()
				.map(|counter| {
					counter.disable().unwrap();
					counter.read().unwrap()
				})
				.sum()
		}
	}
}

#[cfg(not(target_os = "linux"))]
mod instructions {
	pub struct Counter;

	impl Counter {
		pub fn new() -> Option<Self> {
			None
		}
		pub fn start(&mut self) {}
		pub fn stop(&mut self) -> u64 {
			0
		}
	}
}

#[derive(Clone, Copy)]
struct Case {
	codec: Codec,
	input_rate: u32,
	layout: Layout,
	/// Input frames (per channel) in each write; the last write may be shorter.
	batch: usize,
	producers: usize,
}

impl Case {
	fn frames(&self) -> usize {
		SECONDS * self.input_rate as usize
	}

	fn samples(&self) -> u64 {
		(self.frames() * self.layout.channels() as usize * self.producers) as u64
	}

	fn name(&self) -> String {
		let batch = if self.batch >= self.frames() {
			"burst".to_string()
		} else {
			self.batch.to_string()
		};
		format!(
			"{}/{}hz/{}ch/batch_{batch}/producers_{}",
			self.codec,
			self.input_rate,
			self.layout.channels(),
			self.producers
		)
	}

	/// The batches every producer writes, built outside the measured section.
	fn input(&self) -> Vec<Frame> {
		let channels = self.layout.channels() as usize;
		let samples: Vec<f32> = (0..self.frames() * channels)
			.map(|index| (index as f32 * 0.01).sin() * 0.25)
			.collect();
		samples
			.chunks(self.batch * channels)
			.enumerate()
			.map(|(index, chunk)| {
				let frames = (index * self.batch) as u64;
				let timestamp = Timestamp::from_micros(frames * 1_000_000 / self.input_rate as u64).unwrap();
				let data: Vec<u8> = chunk.iter().flat_map(|sample| sample.to_le_bytes()).collect();
				Frame::new(Bytes::from(data), timestamp)
			})
			.collect()
	}

	/// Fresh producers on one broadcast, kept alive so teardown stays outside the measurement.
	fn producers(&self) -> (moq_net::broadcast::Producer, Vec<Producer>) {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let input = Input::new(self.input_rate, self.layout);
		let mut options = Options::default();
		options.config = Config::from_input(self.codec, &input);
		options.config.sample_rate = CODEC_RATE;
		let producers = (0..self.producers)
			.map(|_| Producer::new(&mut broadcast, catalog.clone(), input.clone(), &options).unwrap())
			.collect();
		(broadcast, producers)
	}
}

/// Write every batch round-robin across the producers, then finish them, returning the slowest write.
fn run(producers: &mut [Producer], input: &[Frame]) -> Duration {
	let mut slowest = Duration::ZERO;
	for frame in input {
		for producer in producers.iter_mut() {
			let start = Instant::now();
			producer.write(black_box(frame)).unwrap();
			slowest = slowest.max(start.elapsed());
		}
	}
	for producer in producers.iter_mut() {
		producer.finish().unwrap();
	}
	slowest
}

fn cases() -> Vec<Case> {
	let case = |codec, input_rate: u32, layout, batch, producers| Case {
		codec,
		input_rate,
		layout,
		batch,
		producers,
	};
	let rates = [48_000, 44_100, 96_000];
	// 32 frames, 10 ms, 100 ms, one second, then all of it in one write.
	let batches = |rate: u32| {
		let rate = rate as usize;
		[32, rate / 100, rate / 10, rate, SECONDS * rate]
	};

	let mut cases = Vec::new();
	for rate in rates {
		for batch in batches(rate) {
			cases.push(case(Codec::Pcm, rate, Layout::Stereo, batch, 1));
		}
	}
	for layout in [Layout::Mono, Layout::FivePointOne] {
		for rate in rates {
			let [_, steady, _, _, burst] = batches(rate);
			for batch in [steady, burst] {
				cases.push(case(Codec::Pcm, rate, layout, batch, 1));
			}
		}
	}
	for rate in [48_000, 44_100] {
		let [_, steady, _, _, burst] = batches(rate);
		for batch in [steady, burst] {
			cases.push(case(Codec::Pcm, rate, Layout::Stereo, batch, 8));
			cases.push(case(Codec::Opus, rate, Layout::Stereo, batch, 1));
		}
	}
	cases
}

fn counts() {
	let mut counter = instructions::Counter::new();
	println!(
		"{:<44} {:>12} {:>10} {:>12} {:>12}",
		"case", "insn/sample", "allocs", "alloc_bytes", "slowest_us"
	);
	for case in cases() {
		let input = case.input();
		let (broadcast, mut producers) = case.producers();
		ALLOCATIONS.store(0, Ordering::Relaxed);
		ALLOCATED.store(0, Ordering::Relaxed);
		COUNTING.store(true, Ordering::Relaxed);
		if let Some(counter) = &mut counter {
			counter.start();
		}
		let slowest = run(&mut producers, &input);
		let instructions = counter.as_mut().map(instructions::Counter::stop);
		COUNTING.store(false, Ordering::Relaxed);
		drop((producers, broadcast));

		let per_sample = match instructions {
			Some(instructions) => format!("{:.1}", instructions as f64 / case.samples() as f64),
			None => "-".to_string(),
		};
		println!(
			"{:<44} {per_sample:>12} {:>10} {:>12} {:>12.1}",
			case.name(),
			ALLOCATIONS.load(Ordering::Relaxed),
			ALLOCATED.load(Ordering::Relaxed),
			slowest.as_secs_f64() * 1e6,
		);
	}
}

fn bench_batches(c: &mut Criterion) {
	let mut group = c.benchmark_group("pcm_batches");
	group.sample_size(10);
	group.warm_up_time(Duration::from_millis(500));
	group.measurement_time(Duration::from_secs(2));
	for case in cases() {
		let input = case.input();
		group.throughput(Throughput::Elements(case.samples()));
		group.bench_function(BenchmarkId::from_parameter(case.name()), |b| {
			b.iter_batched(
				|| case.producers(),
				|(broadcast, mut producers)| {
					run(&mut producers, &input);
					(broadcast, producers)
				},
				BatchSize::PerIteration,
			);
		});
	}
	group.finish();
}

criterion_group!(benches, bench_batches);

fn main() {
	// Nextest lists all targets as potential test binaries.
	if std::env::args().any(|arg| arg == "--list") {
		return;
	}
	counts();
	benches();
	Criterion::default().configure_from_args().final_summary();
}
