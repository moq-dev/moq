//! A join judged by the track's refresh ratio, and a reader's refresh judged by its
//! interval, with the demand each raises: swept over tracks and subscribers per track. The
//! judgment reads only the asked track's newest group, so a cost that grows with the other
//! tracks or the track's audience shows up as a slope. The group answering each request is
//! written between calls, outside the measurement.
//!
//! Run with `cargo bench -p moq-net --bench refresh`.

use std::fmt;

use bytes::Bytes;
use moq_net::{broadcast, track};

const TRACKS: [usize; 3] = [1, 100, 1_000];
const SUBSCRIBERS: [usize; 3] = [1, 10, 100];

/// Tracks on the broadcast and subscribers per track, named `<tracks>x/<subscribers>`.
#[derive(Clone, Copy)]
struct Case {
	tracks: usize,
	subscribers: usize,
}

impl fmt::Display for Case {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}x/{}", self.tracks, self.subscribers)
	}
}

fn cases() -> Vec<Case> {
	TRACKS
		.into_iter()
		.flat_map(|tracks| SUBSCRIBERS.map(|subscribers| Case { tracks, subscribers }))
		.collect()
}

/// Append group `key`, whose delta outweighs its first frame.
fn refreshable(track: &track::Producer, key: u64) {
	let start = moq_net::Timestamp::unscaled(key);
	let mut group = track.append_group(start).unwrap();
	group.write_frame(start, Bytes::from_static(b"key")).unwrap();
	group.write_frame(start, Bytes::from_static(b"delta")).unwrap();
	group.finish().unwrap();
}

/// A broadcast of `tracks` tracks with `subscribers` readers each, and the demand of the
/// middle track, which each call asks to refresh.
struct Audience {
	_broadcast: broadcast::Producer,
	producers: Vec<track::Producer>,
	audience: Vec<Vec<track::Subscriber>>,
	demand: track::Demand,
	latest: u64,
}

impl Audience {
	fn new(case: Case, info: track::Info) -> Self {
		let broadcast = broadcast::Info::default().produce();
		let producers: Vec<_> = (0..case.tracks)
			.map(|i| broadcast.create_track(format!("bench{i}"), info.clone()).unwrap())
			.collect();
		let audience = producers
			.iter()
			.map(|track| (0..case.subscribers).map(|_| track.subscribe(None)).collect())
			.collect();
		let demand = producers[case.tracks / 2].demand();
		refreshable(&producers[case.tracks / 2], 0);
		Self {
			_broadcast: broadcast,
			producers,
			audience,
			demand,
			latest: 0,
		}
	}

	fn track(&self) -> &track::Producer {
		&self.producers[self.producers.len() / 2]
	}

	/// Write the group answering the last request.
	fn answer(&mut self) {
		self.latest += 1;
		refreshable(self.track(), self.latest);
	}
}

mod refresh {
	use std::time::Duration;

	use moq_net::track;
	use swole::bench;

	use super::{Audience, Case, cases};

	/// An audience whose track refreshes on every join, and the reader each call joins with.
	struct Joining {
		audience: Audience,
		joined: Option<track::Subscriber>,
	}

	fn joining(case: Case) -> Joining {
		let info = track::Info::default()
			.with_max_age(None)
			.with_refresh(track::Refresh::default().with_ratio(1));
		Joining {
			audience: Audience::new(case, info),
			joined: None,
		}
	}

	/// Drop the joined reader and answer its request, between calls.
	fn rejoin(input: &mut Joining) {
		drop(input.joined.take());
		input.audience.answer();
	}

	#[bench(args = cases(), setup = joining, after = rejoin)]
	fn join(input: &mut Joining) {
		input.joined = Some(input.audience.track().subscribe(None));
		assert!(input.audience.demand.poll_refresh(&kio::Waiter::noop()).is_ready());
	}

	/// An audience whose readers may refresh at any interval, and the requests each call holds.
	struct Requesting {
		audience: Audience,
		held: Vec<track::Refreshing>,
	}

	fn requesting(case: Case) -> Requesting {
		let refresh = track::Refresh::default().with_interval(Duration::ZERO);
		let info = track::Info::default().with_max_age(None).with_refresh(refresh);
		Requesting {
			audience: Audience::new(case, info),
			held: Vec::with_capacity(case.subscribers),
		}
	}

	/// Release the requests and answer them, between calls.
	fn rerequest(input: &mut Requesting) {
		input.held.clear();
		input.audience.answer();
	}

	#[bench(args = cases(), setup = requesting, after = rerequest)]
	fn request(input: &mut Requesting) {
		let asking = &input.audience.audience[input.audience.producers.len() / 2];
		// Every reader asks; all but the first coalesce.
		input.held.extend(asking.iter().map(|reader| reader.refresh()));
		assert!(input.audience.demand.poll_refresh(&kio::Waiter::noop()).is_ready());
	}
}

fn main() {
	swole::main();
}
