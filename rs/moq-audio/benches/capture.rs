//! Shared capture publication cost, swept over sources and independent readers.
//!
//! `capture_publish` writes one buffer to each source; `capture_fanout` also has every reader
//! read it.
//!
//! Run with `cargo bench -p moq-audio --features capture --bench capture`.

use std::fmt;
use std::sync::Arc;

use moq_audio::capture::{Device, Source};
use moq_audio::{Error, Layout, encode};
use moq_net::time::Instant;

// Exercise the actual history implementation without opening hardware or
// exporting a synthetic device API solely for the benchmark.
#[allow(
	dead_code,
	reason = "compile the private capture history to benchmark it without device I/O"
)]
#[path = "../src/capture/fanout.rs"]
mod fanout;

/// How many sources, each with how many readers.
#[derive(Clone, Copy)]
struct Case {
	sources: usize,
	readers: usize,
}

impl fmt::Display for Case {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "sources_{}/{}", self.sources, self.readers)
	}
}

fn cases() -> Vec<Case> {
	[1, 4, 16]
		.into_iter()
		.flat_map(|sources| [1, 4, 16].map(|readers| Case { sources, readers }))
		.collect()
}

fn samples(case: Case) -> usize {
	case.sources * 480
}

/// The sources, their readers, and the 10 ms stereo buffer each call writes.
struct Capture {
	producers: Vec<fanout::Producer>,
	consumers: Vec<Vec<fanout::Consumer>>,
	data: Arc<[f32]>,
	captured: Instant,
	waiter: kio::Waiter,
}

fn capture(case: Case) -> Capture {
	let producers: Vec<_> = (0..case.sources)
		.map(|_| fanout::Producer::new(encode::Input::new(48_000, Layout::Stereo), Source::default(), None))
		.collect();
	let consumers = producers
		.iter()
		.map(|producer| (0..case.readers).map(|_| producer.consume()).collect())
		.collect();
	Capture {
		producers,
		consumers,
		data: vec![0.25; 960].into(),
		captured: Instant::now(),
		waiter: kio::Waiter::noop(),
	}
}

/// Write one buffer to every source, and with `drain`, read it from every reader.
fn publish(capture: &mut Capture, drain: bool) {
	for (producer, readers) in capture.producers.iter().zip(&mut capture.consumers) {
		producer.write(fanout::Buffer {
			data: capture.data.clone(),
			captured: capture.captured,
			gap: false,
		});
		for reader in readers.iter_mut().filter(|_| drain) {
			assert!(matches!(
				std::hint::black_box(reader.poll_read(&capture.waiter)),
				std::task::Poll::Ready(Ok(_))
			));
		}
	}
}

#[swole::bench(args = cases(), setup = capture, items = samples)]
fn capture_publish(capture: &mut Capture) {
	publish(capture, false)
}

#[swole::bench(args = cases(), setup = capture, items = samples)]
fn capture_fanout(capture: &mut Capture) {
	publish(capture, true)
}

fn main() {
	swole::main();
}
