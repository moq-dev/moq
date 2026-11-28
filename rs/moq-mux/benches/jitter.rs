//! The jitter buffer's clock steering swept over track count and how many floors the rate
//! window holds.
//!
//! Run with `cargo bench -p moq-mux --bench jitter`.
use std::fmt;
use std::sync::LazyLock;
use std::time::Duration;

use moq_net::Timestamp;
use swole::bench;

#[path = "../src/jitter.rs"]
#[allow(
	dead_code,
	reason = "the bench compiles the private module from source and uses only part of it"
)]
mod jitter;

/// The paused clock the buffer runs on.
static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
	tokio::runtime::Builder::new_current_thread()
		.enable_time()
		.start_paused(true)
		.build()
		.unwrap()
});

/// A track count and a second dimension, named `<tracks>/<n>`.
#[derive(Clone, Copy)]
struct Case {
	tracks: u16,
	n: u64,
}

impl fmt::Display for Case {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}/{}", self.tracks, self.n)
	}
}

/// Every track count against each of `ns`.
fn cases(ns: [u64; 3]) -> Vec<Case> {
	[1u16, 4, 16, 64]
		.into_iter()
		.flat_map(|tracks| ns.map(|n| Case { tracks, n }))
		.collect()
}

/// Every track sends a frame a second as it decodes, for as many two-second steering steps as
/// fill the rate window with `floors`, each step measuring every track heard.
async fn steer(tracks: u16, floors: u64) -> u64 {
	let start = kio::clock::now();
	let mut buffer = jitter::Buffer::new(Duration::from_millis(500));
	buffer.expect(1..=tracks);
	let waiter = kio::Waiter::noop();
	for second in 0..floors * 2 {
		let at = start.saturating_add(Duration::from_secs(second));
		tokio::time::advance(at.saturating_duration_since(kio::clock::now())).await;
		for track in 1..=tracks {
			let arrival = jitter::Arrival {
				arrived: at,
				read: at,
				decode: Timestamp::from_millis(second * 1_000).unwrap().try_into().unwrap(),
				restart: 0,
				sync: true,
				leading: false,
				bytes: 0,
				item: (),
			};
			buffer.push(track, arrival).unwrap();
		}
		while buffer.poll_next(&waiter).is_ready() {}
	}
	buffer.dropped()
}

#[bench(args = cases([10, 100, 300]))]
fn jitter_steering(case: Case) {
	assert_eq!(RUNTIME.block_on(steer(case.tracks, case.n)), 0);
}

/// One fill drains every track's available backlog before polling output.
#[bench(args = cases([16, 64, 256]))]
fn jitter_acquisition(case: Case) {
	let Case { tracks, n: frames } = case;
	RUNTIME.block_on(async {
		let start = kio::clock::now();
		let mut buffer = jitter::Buffer::new(Duration::from_millis(500));
		buffer.expect(1..=tracks);
		for track in 1..=tracks {
			for frame in 0..frames {
				buffer
					.push(
						track,
						jitter::Arrival {
							arrived: start,
							read: start,
							decode: Timestamp::from_millis(frame * 20).unwrap().try_into().unwrap(),
							restart: 0,
							sync: frame % 15 == 0,
							leading: false,
							bytes: 0,
							item: (),
						},
					)
					.unwrap();
			}
		}
		let _ = std::hint::black_box(buffer.poll_next(&kio::Waiter::noop()));
	})
}

/// An independent identity acquires while the old tracks still hold their tail.
#[bench(args = cases([16, 64, 256]))]
fn jitter_handoff(case: Case) {
	let Case { tracks, n: frames } = case;
	RUNTIME.block_on(async {
		let start = kio::clock::now();
		let mut buffer = jitter::Buffer::new(Duration::from_millis(500));
		buffer.expect(1..=tracks);
		for identity in 0..2 {
			if identity == 1 {
				let keys = tracks + 1..=2 * tracks;
				buffer.expect(keys.clone());
				buffer.start(keys.clone(), keys);
			}
			for track in 1..=tracks {
				for frame in 0..frames {
					buffer
						.push(
							identity * tracks + track,
							jitter::Arrival {
								arrived: start,
								read: start,
								decode: Timestamp::from_millis(u64::from(identity) * 10_000 + frame * 20)
									.unwrap()
									.try_into()
									.unwrap(),
								restart: 0,
								sync: frame % 15 == 0,
								leading: false,
								bytes: 0,
								item: (),
							},
						)
						.unwrap();
				}
			}
			let _ = std::hint::black_box(buffer.poll_next(&kio::Waiter::noop()));
		}
		tokio::time::advance(Duration::from_secs(1)).await;
		if let std::task::Poll::Ready(ready) = buffer.poll_pending(&kio::Waiter::noop()) {
			buffer.commit(ready).unwrap();
		}
		assert!(buffer.is_committed());
	})
}

/// Admission reads one time window, independent of the backlog still held by each source.
#[bench(args = cases([64, 256, 1_024]))]
fn jitter_admission(case: Case) -> usize {
	let Case { tracks, n: backlog } = case;
	RUNTIME.block_on(async {
		let start = kio::clock::now();
		let delay = Duration::from_millis(500);
		let mut buffer = jitter::Buffer::new(delay);
		buffer.expect(1..=tracks);
		let frame = |decode| jitter::Arrival {
			arrived: start,
			read: kio::clock::now(),
			decode: Timestamp::from_millis(decode).unwrap().try_into().unwrap(),
			restart: 0,
			sync: true,
			leading: false,
			bytes: 0,
			item: (),
		};
		for track in 1..=tracks {
			buffer.push(track, frame(0)).unwrap();
		}
		tokio::time::advance(delay).await;
		while buffer.poll_next(&kio::Waiter::noop()).is_ready() {}
		let mut read = 0;
		for track in 1..=tracks {
			for next in 1..=backlog {
				if buffer.full(&track, &kio::Waiter::noop()) {
					break;
				}
				buffer.push(track, frame(next * 20)).unwrap();
				read += 1;
			}
		}
		read
	})
}

fn main() {
	swole::main();
}
