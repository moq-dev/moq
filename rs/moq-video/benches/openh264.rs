//! What the codec API costs around the codec: OpenH264 encode and decode of a short
//! clip, through the inline front ends ([`encode::Encoder`], [`decode::Decoder`]) and
//! the threaded [`encode::Sink`] and [`decode::Sink`], swept over a tiny picture, where
//! the codec is cheap and the API shows, and a modest one, where the codec dominates.
//!
//! Every case submits each frame and receives what it readied before the next, then
//! flushes and receives the tail, the way a live caller drives a codec.
//!
//! Run with `cargo bench -p moq-video --features openh264 --bench openh264`.

use std::fmt;
use std::hint::black_box;
use std::sync::Arc;

use bytes::Bytes;
use moq_net::Timestamp;
use moq_video::{Frame, FrameRate, I420, Size, Surface, decode, encode};

/// Frames per case: one group, so every case encodes a single keyframe.
const FRAMES: u64 = 30;
const SIZES: [(u32, u32); 2] = [(64, 64), (640, 360)];

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

/// One stage through one front end at one size, named `<stage>/<path>/<width>x<height>`.
#[derive(Clone, Copy)]
struct Case {
	stage: &'static str,
	path: Path,
	size: (u32, u32),
}

impl Case {
	fn clip(self) -> (Clip, Case) {
		(Clip::new(Size::new(self.size.0, self.size.1)), self)
	}
}

impl fmt::Display for Case {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}/{}/{}x{}", self.stage, self.path.name(), self.size.0, self.size.1)
	}
}

fn cases() -> Vec<Case> {
	let mut cases = Vec::new();
	for size in SIZES {
		for stage in ["encode", "decode"] {
			for path in [Path::Inline, Path::Sink] {
				cases.push(Case { stage, path, size });
			}
		}
	}
	cases
}

#[swole::bench(args = cases(), setup = Case::clip, items = |_: Case| FRAMES)]
fn codec((clip, case): &(Clip, Case)) -> u64 {
	let received = match case.stage {
		"encode" => encode(clip, case.path),
		_ => decode(clip, case.path),
	};
	assert_eq!(received, FRAMES, "{case}");
	received
}

fn main() {
	swole::main();
}
