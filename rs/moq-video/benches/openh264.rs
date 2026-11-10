//! What the codec API costs around the codec: OpenH264 encode and decode of a short
//! clip, through the inline front ends ([`encode::Encoder`], [`decode::Decoder`]) and
//! the threaded [`encode::Sink`] and [`decode::Sink`], swept over a tiny picture, where
//! the codec is cheap and the API shows, and a modest one, where the codec dominates.
//!
//! Every case submits each frame and receives what it readied before the next, then
//! flushes and receives the tail, the way a live caller drives a codec.
//!
//! Run with `cargo bench -p moq-video --features openh264 --bench openh264`. A table comes
//! first: allocations and allocated bytes per frame from an untimed pass, counting the
//! Rust allocations of the codec wrapper and every frame's output, not OpenH264's
//! native ones. Criterion then times each case.
use std::alloc::{GlobalAlloc, Layout as AllocLayout, System};
use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;
use criterion::{BenchmarkId, Criterion, Throughput};
use moq_net::Timestamp;
use moq_video::{Frame, FrameRate, I420, Size, Surface, decode, encode};

/// Frames per case: one group, so every case encodes a single keyframe.
const FRAMES: u64 = 30;
const SIZES: [(u32, u32); 2] = [(64, 64), (640, 360)];

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

/// Which front end drives the codec.
#[derive(Clone, Copy)]
enum Path {
	/// The caller's thread.
	Inline,
	/// The `Sink`'s own codec thread.
	Sink,
}

impl Path {
	fn name(self) -> &'static str {
		match self {
			Self::Inline => "inline",
			Self::Sink => "sink",
		}
	}
}

/// One clip at one size: its raw pictures, and the access units they encode to.
struct Clip {
	size: Size,
	pictures: Vec<Arc<Frame>>,
	units: Vec<(Bytes, Timestamp, bool)>,
	catalog: hang::catalog::VideoConfig,
}

impl Clip {
	fn new(size: Size) -> Self {
		let pictures: Vec<Arc<Frame>> = (0..FRAMES).map(|index| Arc::new(picture(size, index))).collect();
		let config = config(size);
		let catalog = pollster::block_on(config.probe()).expect("probe the encoder");
		let mut encoder = encode::Encoder::new(&config).expect("OpenH264 encoder");
		let mut units = Vec::new();
		for picture in &pictures {
			encoder.encode(picture).unwrap();
			while let Some(encoded) = encoder.receive().unwrap() {
				units.push((encoded.payload, encoded.timestamp, encoded.keyframe));
			}
		}
		encoder.flush().unwrap();
		while let Some(encoded) = encoder.receive().unwrap() {
			units.push((encoded.payload, encoded.timestamp, encoded.keyframe));
		}
		assert_eq!(units.len() as u64, FRAMES, "one access unit per picture");
		Self {
			size,
			pictures,
			units,
			catalog,
		}
	}

	fn name(&self, stage: &str, path: Path) -> String {
		format!("{stage}/{}/{}x{}", path.name(), self.size.width, self.size.height)
	}
}

fn config(size: Size) -> encode::Config {
	let mut config = encode::Config::new(size.width, size.height, FrameRate::new(30, 1).unwrap());
	config.kind = encode::Kind::Software;
	config
}

/// A gradient that moves with `index`, so every picture has new content to encode.
fn picture(size: Size, index: u64) -> Frame {
	let (width, height) = (size.width as usize, size.height as usize);
	let mut data = vec![128u8; width * height * 3 / 2];
	for y in 0..height {
		for x in 0..width {
			data[y * width + x] = ((x + y + index as usize * 4) % 256) as u8;
		}
	}
	let i420 = I420::new(size, data).unwrap();
	Frame::new(Surface::I420(i420), Timestamp::from_micros(index * 33_333).unwrap())
}

/// Encode the clip from a fresh encoder, receiving after every picture.
fn encode(clip: &Clip, path: Path) -> u64 {
	let config = config(clip.size);
	let mut received = 0;
	match path {
		Path::Inline => {
			let mut encoder = encode::Encoder::new(&config).unwrap();
			for picture in &clip.pictures {
				encoder.encode(picture).unwrap();
				while let Some(encoded) = encoder.receive().unwrap() {
					black_box(encoded);
					received += 1;
				}
			}
			encoder.flush().unwrap();
			while let Some(encoded) = encoder.receive().unwrap() {
				black_box(encoded);
				received += 1;
			}
		}
		Path::Sink => pollster::block_on(async {
			let mut sink = encode::Sink::open(&config).await.unwrap();
			for picture in &clip.pictures {
				sink.encode(picture.clone()).await.unwrap();
				while let Some(encoded) = sink.receive().unwrap() {
					black_box(encoded);
					received += 1;
				}
			}
			sink.flush().await.unwrap();
			while let Some(encoded) = sink.receive().unwrap() {
				black_box(encoded);
				received += 1;
			}
		}),
	}
	received
}

/// Decode the clip from a fresh decoder, receiving after every access unit.
fn decode(clip: &Clip, path: Path) -> u64 {
	let mut config = decode::Config::new();
	config.kind = decode::Kind::Software;
	let mut received = 0;
	match path {
		Path::Inline => {
			let mut decoder = decode::Decoder::new(&clip.catalog, &config).unwrap();
			for (payload, timestamp, keyframe) in &clip.units {
				decoder.decode(payload, *timestamp, *keyframe).unwrap();
				while let Some(frame) = decoder.receive().unwrap() {
					black_box(frame);
					received += 1;
				}
			}
			decoder.flush().unwrap();
			while let Some(frame) = decoder.receive().unwrap() {
				black_box(frame);
				received += 1;
			}
		}
		Path::Sink => pollster::block_on(async {
			let mut sink = decode::Sink::open(&clip.catalog, &config).await.unwrap();
			for (payload, timestamp, keyframe) in &clip.units {
				sink.decode(payload.clone(), *timestamp, *keyframe).await.unwrap();
				while let Some(frame) = sink.receive().unwrap() {
					black_box(frame);
					received += 1;
				}
			}
			sink.flush().await.unwrap();
			while let Some(frame) = sink.receive().unwrap() {
				black_box(frame);
				received += 1;
			}
		}),
	}
	received
}

type Stage = fn(&Clip, Path) -> u64;
const STAGES: [(&str, Stage); 2] = [("encode", encode), ("decode", decode)];
const PATHS: [Path; 2] = [Path::Inline, Path::Sink];

/// Print allocations per frame for every case, from one untimed pass each.
fn counts(clips: &[Clip]) {
	println!("{:<28} {:>14} {:>14}", "case", "allocs/frame", "bytes/frame");
	for clip in clips {
		for (stage, run) in STAGES {
			for path in PATHS {
				ALLOCATIONS.store(0, Ordering::Relaxed);
				ALLOCATED.store(0, Ordering::Relaxed);
				COUNTING.store(true, Ordering::Relaxed);
				let received = run(clip, path);
				COUNTING.store(false, Ordering::Relaxed);
				assert_eq!(received, FRAMES, "{}", clip.name(stage, path));
				println!(
					"{:<28} {:>14.2} {:>14.0}",
					clip.name(stage, path),
					ALLOCATIONS.load(Ordering::Relaxed) as f64 / FRAMES as f64,
					ALLOCATED.load(Ordering::Relaxed) as f64 / FRAMES as f64,
				);
			}
		}
	}
}

fn bench_codec(c: &mut Criterion, clips: &[Clip]) {
	let mut group = c.benchmark_group("codec");
	group.sample_size(10);
	group.warm_up_time(Duration::from_millis(500));
	group.measurement_time(Duration::from_secs(3));
	group.throughput(Throughput::Elements(FRAMES));
	for clip in clips {
		for (stage, run) in STAGES {
			for path in PATHS {
				group.bench_function(BenchmarkId::from_parameter(clip.name(stage, path)), |b| {
					b.iter(|| run(clip, path));
				});
			}
		}
	}
	group.finish();
}

fn main() {
	// Nextest lists all targets as potential test binaries.
	if std::env::args().any(|arg| arg == "--list") {
		return;
	}
	let clips: Vec<Clip> = SIZES
		.into_iter()
		.map(|(width, height)| Clip::new(Size::new(width, height)))
		.collect();
	counts(&clips);
	let mut criterion = Criterion::default().configure_from_args();
	bench_codec(&mut criterion, &clips);
	criterion.final_summary();
}
