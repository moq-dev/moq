//! Cost of reading one tick off a bucket track, swept over broadcasts per tier, tiers, and the share
//! of broadcasts that changed since the last tick.
//!
//! Each tick is one merge-patch delta, applied and materialized the way [`moq_stats::Consumer`]
//! does per yield. The encoded frames are built outside the measured section, and so is the
//! track's own DEFLATE, which moq-net inflates before the reader sees a frame. After the last tick
//! the decoder starts over from the snapshot, outside the measured section.
//!
//! Run with `cargo bench -p moq-stats --bench decode`.

use std::collections::BTreeMap;
use std::fmt;

use moq_json::snapshot::{self, Decoder, Encoder};
use moq_net::stats::Traffic;
use moq_stats::{Bucket, BucketFrame};
use swole::bench;

/// A bucket frame as the producer writes it.
#[derive(serde::Serialize)]
struct Body {
	epoch: String,
	tiers: BTreeMap<String, Bucket>,
}

/// Ticks a decoder reads before it starts over from the snapshot, after one warm-up delta.
const TICKS: u64 = 16;

/// Broadcast `index` at `tick`: every tenth broadcast is live when `changed_percent` is 10, all of
/// them when it is 100. Idle broadcasts keep their counters, so they drop out of the delta.
fn traffic(index: usize, tick: u64, changed_percent: usize) -> Traffic {
	let live = index.is_multiple_of(100 / changed_percent);
	let tick = if live { tick } else { 0 };
	let mut traffic = Traffic::default();
	traffic.announces_started = 1;
	traffic.broadcasts_started = 3;
	traffic.broadcasts_ended = 1;
	traffic.subscriptions_started = 6;
	traffic.subscriptions_ended = 2;
	traffic.bytes = 180_000 * tick + index as u64;
	traffic.frames = 30 * tick;
	traffic.groups = tick / 2;
	traffic
}

/// The frames a bucket's producer would publish over the run, snapshot first.
fn frames(broadcasts: usize, tiers: usize, changed_percent: usize) -> Vec<(Vec<u8>, bool)> {
	let mut encoder = Encoder::<Body>::new(snapshot::Config::default().with_deltas(true));
	let epoch = "01900000-0000-7000-8000-000000000001".to_string();
	(0..TICKS + 2)
		.map(|tick| {
			let tiers = (0..tiers)
				.map(|tier| {
					let mut bucket = Bucket::default();
					for index in 0..broadcasts {
						let path = format!("acme/room-{index:06}/camera");
						let usage = bucket.paths.entry(path).or_default();
						usage.publisher = traffic(index, tick, changed_percent);
					}
					(format!("tier-{tier}"), bucket)
				})
				.collect();
			let frame = Body {
				epoch: epoch.clone(),
				tiers,
			};
			let encoded = encoder.update(&frame).unwrap().expect("every tick changes something");
			let out = (encoded.payload.to_vec(), encoded.keyframe);
			encoded.commit();
			out
		})
		.collect()
}

/// Broadcasts per tier, tiers, and the share that changed, named `<broadcasts>b_<tiers>t_<changed>pct`.
#[derive(Clone, Copy)]
struct Case {
	broadcasts: usize,
	tiers: usize,
	changed: usize,
}

impl fmt::Display for Case {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}b_{}t_{}pct", self.broadcasts, self.tiers, self.changed)
	}
}

fn cases() -> Vec<Case> {
	let mut cases = Vec::new();
	for changed in [10, 100] {
		for tiers in [1, 4] {
			for broadcasts in [1, 16, 256, 4096] {
				cases.push(Case {
					broadcasts,
					tiers,
					changed,
				});
			}
		}
	}
	cases
}

/// A decoder partway through a run of frames.
struct Reader {
	decoder: Decoder<BucketFrame>,
	frames: Vec<(Vec<u8>, bool)>,
	/// The next frame to read.
	next: usize,
}

impl Reader {
	/// Read the snapshot and the warm-up delta, leaving the measured ticks.
	fn rewind(&mut self) {
		let (snapshot, keyframe) = &self.frames[0];
		assert!(*keyframe);
		self.decoder.snapshot(snapshot).unwrap();
		self.decoder.delta(&self.frames[1].0).unwrap();
		self.decoder.decode().unwrap();
		self.next = 2;
	}

	/// Start over from the snapshot once every tick has been read.
	fn ready(&mut self) {
		if self.next == self.frames.len() {
			self.rewind();
		}
	}
}

fn reader(case: Case) -> Reader {
	let mut reader = Reader {
		decoder: Decoder::new(snapshot::consumer::Config::default()),
		frames: frames(case.broadcasts, case.tiers, case.changed),
		next: 0,
	};
	reader.rewind();
	reader
}

#[bench(args = cases(), setup = reader, before = Reader::ready)]
fn tick(reader: &mut Reader) -> Option<BucketFrame> {
	let (payload, keyframe) = &reader.frames[reader.next];
	reader.next += 1;
	// A large, busy group reaches its byte cap and opens a fresh one.
	match keyframe {
		true => reader.decoder.snapshot(payload).unwrap(),
		false => reader.decoder.delta(payload).unwrap(),
	}
	reader.decoder.decode().unwrap()
}

fn main() {
	swole::main();
}
