//! The shared clock's update and presentation cost, over queued pictures and independent readers.
//!
//! Run with `cargo bench -p moq-mux --bench sync`.
use std::fmt;
use std::hint::black_box;
use std::time::Duration;

use kio::clock::Instant;
use moq_mux::{container::Kind, sync};
use moq_net::Timestamp;
use swole::bench;

/// Independent readers presenting from a queue of pictures, named `<readers>/<frames>`.
#[derive(Clone, Copy)]
struct Readers {
	readers: u64,
	frames: u64,
}

impl fmt::Display for Readers {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}/{}", self.readers, self.frames)
	}
}

fn readers() -> Vec<Readers> {
	[1, 8, 64]
		.into_iter()
		.flat_map(|readers| [1, 3, 30].map(|frames| Readers { readers, frames }))
		.collect()
}

fn now() -> Instant {
	Instant::ZERO.saturating_add(Duration::from_secs(1))
}

/// A clock with audio and video received, and its readers.
struct Present {
	_clock: sync::Producer,
	_audio: sync::Track,
	_video: sync::Track,
	consumers: Vec<sync::Consumer>,
	timestamps: Vec<Timestamp>,
}

fn present(case: Readers) -> Present {
	let clock = sync::Producer::new(Duration::from_millis(100));
	let audio = clock.track(Kind::Audio);
	let video = clock.track(Kind::Video);
	audio.received(Timestamp::from_millis(1_000).unwrap(), now());
	video.received(Timestamp::from_millis(1_000).unwrap(), now());
	let consumers = (0..case.readers).map(|_| clock.consume()).collect();
	let timestamps = (0..case.frames)
		.map(|index| Timestamp::from_millis(1_000 + index * 20).unwrap())
		.collect();
	Present {
		_clock: clock,
		_audio: audio,
		_video: video,
		consumers,
		timestamps,
	}
}

#[bench(args = readers(), setup = present, items = |case: Readers| case.readers)]
fn sync_present(present: &Present) {
	let later = now().saturating_add(Duration::from_secs(1));
	for consumer in &present.consumers {
		let selected = consumer.present(black_box(present.timestamps.iter().copied()), later);
		assert_eq!(selected, Some(present.timestamps.len() - 1));
	}
}

/// A clock with an audio and a video track.
struct Received {
	_clock: sync::Producer,
	audio: sync::Track,
	_video: sync::Track,
}

fn received() -> Received {
	let clock = sync::Producer::new(Duration::from_millis(100));
	let audio = clock.track(Kind::Audio);
	let video = clock.track(Kind::Video);
	Received {
		_clock: clock,
		audio,
		_video: video,
	}
}

#[bench(setup = received)]
fn sync_received(received: &Received) -> bool {
	received
		.audio
		.received(black_box(Timestamp::from_millis(1_000).unwrap()), now())
}

fn main() {
	swole::main();
}
