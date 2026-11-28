//! `Producer::run` recording every track of a broadcast into memory, swept over the number of
//! tracks and the groups each one holds. Throughput is per frame, so a per-event cost that grows
//! with the track table shows up as a slope across the track counts.
//!
//! Run with `cargo bench -p moq-archive --bench producer`.

use std::fmt;
use std::sync::LazyLock;
use std::time::Duration;

use moq_archive::{Producer, Store};
use moq_net::{Timescale, Timestamp, broadcast, group, track};
use object_store::memory::InMemory;
use swole::bench;

const FRAMES_PER_GROUP: u64 = 2;

/// The runtime every call records on; a static because each call moves its broadcast in.
static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
	tokio::runtime::Builder::new_current_thread()
		.enable_time()
		.build()
		.unwrap()
});

/// A broadcast's shape, named `<groups>_groups/<tracks>`.
#[derive(Clone, Copy)]
struct Case {
	tracks: usize,
	groups: u64,
}

impl Case {
	fn frames(self) -> u64 {
		self.tracks as u64 * self.groups * FRAMES_PER_GROUP
	}
}

impl fmt::Display for Case {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}_groups/{}", self.groups, self.tracks)
	}
}

fn cases() -> Vec<Case> {
	[1, 10, 100]
		.into_iter()
		.flat_map(|tracks| [10, 100].map(|groups| Case { tracks, groups }))
		.collect()
}

/// A broadcast of `tracks` tracks, each already holding `groups` one-second groups.
fn setup(case: Case) -> (broadcast::Producer, Vec<track::Producer>) {
	let source = broadcast::Info::new().produce();
	let info = track::Info::default()
		.with_timescale(Timescale::MILLI)
		.with_max_age(Duration::from_secs(3600));
	let tracks = (0..case.tracks)
		.map(|index| {
			let track = source.create_track(format!("track{index}"), info.clone()).unwrap();
			for sequence in 0..case.groups {
				let mut group = track.create_group(group::Info { start: sequence }).unwrap();
				for frame in 0..FRAMES_PER_GROUP {
					let pts = Timestamp::from_millis(sequence * 1_000 + frame * 500).unwrap();
					group.write_frame(pts, vec![0u8; 64]).unwrap();
				}
				group.finish().unwrap();
			}
			track
		})
		.collect();
	(source, tracks)
}

/// Record the broadcast until every track ends, returning it so its drop goes unmeasured.
#[bench(args = cases(), setup = setup, items = Case::frames)]
fn producer(
	(source, tracks): (broadcast::Producer, Vec<track::Producer>),
) -> (broadcast::Producer, Vec<track::Producer>) {
	RUNTIME.block_on(async move {
		let config = moq_archive::producer::Config::default().with_duration(Duration::from_secs(2));
		let store = Store::new(InMemory::new(), "bench");
		let run = tokio::spawn(Producer::new(store, source.consume(), config).run());
		for track in &tracks {
			while track.subscription().is_none() {
				tokio::task::yield_now().await;
			}
			drop(track.clone().finish().unwrap());
		}
		source.close();
		run.await.unwrap().unwrap();
		(source, tracks)
	})
}

fn main() {
	swole::main();
}
