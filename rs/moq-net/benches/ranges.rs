//! Per-poll cost over the number of ranges in one subscription.
//!
//! A lite-07 SUBSCRIBE fits about 32K ranges in its 64 KiB message, so a walk over every
//! range on each poll lets a peer multiply what one subscription costs by that much. Each
//! group here holds the ranges fixed and repeats one steady-state step, swept over the range
//! count, so such a walk shows up as a slope over it:
//!
//! - `ranges_time_end`: ranges whose time ends lie past the live edge, so none settles. A
//!   group lands, starting before every end, and the reader drains it.
//! - `ranges_time_cap`: one time end past the live edge beside fixed ranges far ahead of
//!   it. A group lands yet to start, which caps the end, then starts inside it, which lifts
//!   the cap, so the window moves twice per group.
//! - `ranges_fills_out`: ranges below where the feed starts, asked of a handler that never
//!   answers, beside one live range. A live group lands and the reader drains it while every
//!   fill is still out.
//! - `ranges_sparse`: a feed numbered sparsely, as a moq-transport publisher may, with the
//!   reader's ranges in the gaps between its groups. A group lands past them, and the reader
//!   polls with nothing in range.
//! - `ranges_time_gaps`: time ranges each ending between two groups of a sparsely numbered
//!   history, so each end waits on the missing sequences of a gap of its own, beside one time
//!   end past the live edge whose cap moves twice per group, as in `ranges_time_cap`.
//! - `ranges_datagram`: timed ranges none of which admits the datagram that lands, so the
//!   reader judges it against all of them.
//! - `ranges_frames`: a lite-07 publisher's frame bounds, each range starting and ending
//!   partway through its own group, looked up for one group as a served group or datagram
//!   does.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench ranges`.

use std::hint::black_box;
use std::task::Poll;
use std::time::Duration;

use bytes::Bytes;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_net::{Timestamp, broadcast, group, track};

/// Ranges in the one subscription, up to about what a lite-07 SUBSCRIBE fits.
const RANGES: [u64; 4] = [1, 64, 1024, 16_384];

/// A budget the run never outlasts, so nothing goes stale.
const BUDGET: Duration = Duration::from_secs(3600);

/// Where the live feed starts, in milliseconds, past every time a range names.
const LIVE: u64 = 1 << 32;

fn timed() -> track::Info {
	track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

fn millis(millis: u64) -> Timestamp {
	Timestamp::from_millis(millis).unwrap()
}

/// Write a one-frame group at `sequence`, presented at `at` milliseconds.
fn land(track: &track::Producer, sequence: u64, at: u64) {
	let mut group = track.create_group(group::Info { sequence }).unwrap();
	group.write_frame(millis(at), Bytes::from_static(b"x")).unwrap();
	group.finish().unwrap();
}

/// Hand out everything the reader has now, returning how many groups.
fn drain(reader: &mut track::Subscriber) -> usize {
	let waiter = kio::Waiter::noop();
	let mut handed = 0;
	while let Poll::Ready(Ok(Some(group))) = reader.poll_recv_group(&waiter) {
		black_box(group);
		handed += 1;
	}
	handed
}

/// One reader whose time ends lie past the live edge; see `ranges_time_end`.
pub(crate) struct TimeEnds {
	_broadcast: broadcast::Producer,
	track: track::Producer,
	reader: track::Subscriber,
	next: u64,
}

impl TimeEnds {
	pub(crate) fn new(ranges: u64) -> Self {
		let broadcast = broadcast::Info::default().produce();
		let track = broadcast.create_track("bench", timed()).unwrap();
		land(&track, 0, 0);
		// Each range starts and ends far past the live edge, at a time of its own.
		let subscription = track::Subscription::default()
			.with_max_delay(BUDGET)
			.with_ranges((0..ranges).map(|i| track::Range {
				start: track::Anchor::Time(millis(LIVE + i * 2)),
				end: Some(track::Anchor::Time(millis(LIVE + i * 2 + 1))),
			}));
		let mut reader = track.subscribe(subscription);
		drain(&mut reader);
		Self {
			_broadcast: broadcast,
			track,
			reader,
			next: 1,
		}
	}

	pub(crate) fn step(&mut self) {
		land(&self.track, self.next, self.next);
		self.next += 1;
		assert_eq!(drain(&mut self.reader), 1, "every end is past the group");
	}
}

/// One reader whose time end moves its cap beside fixed ranges; see `ranges_time_cap`.
pub(crate) struct TimeCap {
	_broadcast: broadcast::Producer,
	track: track::Producer,
	reader: track::Subscriber,
	next: u64,
}

impl TimeCap {
	pub(crate) fn new(ranges: u64) -> Self {
		let broadcast = broadcast::Info::default().produce();
		let track = broadcast.create_track("bench", timed()).unwrap();
		land(&track, 0, 0);
		// The time end, then disjoint groups far past anything the run lands.
		let subscription = track::Subscription::default().with_max_delay(BUDGET).with_ranges(
			std::iter::once(track::Range {
				start: track::Anchor::Time(millis(0)),
				end: Some(track::Anchor::Time(millis(LIVE))),
			})
			.chain((1..ranges).map(|i| track::Range::groups(LIVE + i * 2..LIVE + i * 2 + 1))),
		);
		let mut reader = track.subscribe(subscription);
		drain(&mut reader);
		Self {
			_broadcast: broadcast,
			track,
			reader,
			next: 1,
		}
	}

	pub(crate) fn step(&mut self) {
		let mut group = self.track.create_group(group::Info { sequence: self.next }).unwrap();
		let mut handed = drain(&mut self.reader);
		group.write_frame(millis(self.next), Bytes::from_static(b"x")).unwrap();
		group.finish().unwrap();
		handed += drain(&mut self.reader);
		self.next += 1;
		assert_eq!(handed, 1, "the group is in range");
	}
}

/// One reader waiting on fills that never answer; see `ranges_fills_out`.
pub(crate) struct FillsOut {
	_broadcast: broadcast::Producer,
	track: track::Producer,
	_requests: track::Requests,
	reader: track::Subscriber,
	next: u64,
}

impl FillsOut {
	pub(crate) fn new(ranges: u64) -> Self {
		let broadcast = broadcast::Info::default().produce();
		let mut track = broadcast.create_track("bench", timed()).unwrap();
		// Held and never polled, so every fill stays out.
		let requests = track.requests();
		// The feed starts past every range, with a sequence between each that no range asks
		// for, so each range is a fill of its own.
		let start = ranges * 2;
		track.start_at(start).unwrap();
		land(&track, start, start);
		let subscription = track::Subscription::default().with_max_delay(BUDGET).with_ranges(
			(0..ranges)
				.map(|i| track::Range::groups(i * 2..i * 2 + 1))
				.chain([track::Range::groups(start..)]),
		);
		let mut reader = track.subscribe(subscription);
		drain(&mut reader);
		Self {
			_broadcast: broadcast,
			track,
			_requests: requests,
			reader,
			next: start + 1,
		}
	}

	pub(crate) fn step(&mut self) {
		land(&self.track, self.next, self.next);
		self.next += 1;
		black_box(drain(&mut self.reader));
	}
}

/// One reader whose ranges sit in the gaps of a sparsely numbered feed; see `ranges_sparse`.
pub(crate) struct Sparse {
	_broadcast: broadcast::Producer,
	track: track::Producer,
	reader: track::Subscriber,
	next: u64,
}

/// The step between a sparse feed's sequences.
const STRIDE: u64 = 4;

impl Sparse {
	pub(crate) fn new(ranges: u64) -> Self {
		let broadcast = broadcast::Info::default().produce();
		let track = broadcast.create_track("bench", timed()).unwrap();
		// The feed's groups at every multiple of the stride, and the reader's ranges one past
		// each, which no group fills, above group 0, the one group in range.
		for i in 0..=ranges {
			land(&track, i * STRIDE, i);
		}
		let subscription = track::Subscription::default().with_max_delay(BUDGET).with_ranges(
			std::iter::once(track::Range::groups(0..1))
				.chain((1..=ranges).map(|i| track::Range::groups(i * STRIDE + 1..i * STRIDE + 2))),
		);
		let mut reader = track.subscribe(subscription);
		assert_eq!(drain(&mut reader), 1, "group 0 is the one in range");
		Self {
			_broadcast: broadcast,
			track,
			reader,
			next: ranges + 1,
		}
	}

	pub(crate) fn step(&mut self) {
		land(&self.track, self.next * STRIDE, self.next);
		self.next += 1;
		assert_eq!(drain(&mut self.reader), 0, "nothing lands in range");
	}
}

/// One reader whose time ends wait in the gaps of a sparsely numbered feed; see
/// `ranges_time_gaps`.
pub(crate) struct TimeGaps {
	_broadcast: broadcast::Producer,
	track: track::Producer,
	reader: track::Subscriber,
	/// Where the live feed went on from the history.
	live: u64,
	next: u64,
}

impl TimeGaps {
	pub(crate) fn new(ranges: u64) -> Self {
		let broadcast = broadcast::Info::default().produce();
		let track = broadcast.create_track("bench", timed()).unwrap();
		// The history's groups at every multiple of the stride, ten milliseconds apart, and
		// each range from just after one starts until halfway to the next: it ends before the
		// next group, but the sequences between may still arrive inside it. The live feed goes
		// on from the last, one sequence and millisecond at a time, read by a range from there.
		for i in 0..=ranges {
			land(&track, i * STRIDE, i * 10);
		}
		let live = track::Range {
			start: track::Anchor::Time(millis(ranges * 10)),
			end: Some(track::Anchor::Time(millis(LIVE))),
		};
		let subscription = track::Subscription::default().with_max_delay(BUDGET).with_ranges(
			(0..ranges)
				.map(|i| track::Range {
					start: track::Anchor::Time(millis(i * 10 + 1)),
					end: Some(track::Anchor::Time(millis(i * 10 + 5))),
				})
				.chain([live]),
		);
		let mut reader = track.subscribe(subscription);
		let groups = usize::try_from(ranges).unwrap() + 1;
		assert_eq!(drain(&mut reader), groups, "each range's own group, and the live edge");
		Self {
			_broadcast: broadcast,
			track,
			reader,
			live: ranges * STRIDE,
			next: ranges * STRIDE + 1,
		}
	}

	pub(crate) fn step(&mut self) {
		// Each a millisecond after the last, from where the history ended.
		let at = self.next - self.live + self.live / STRIDE * 10;
		let mut group = self.track.create_group(group::Info { sequence: self.next }).unwrap();
		let mut handed = drain(&mut self.reader);
		group.write_frame(millis(at), Bytes::from_static(b"x")).unwrap();
		group.finish().unwrap();
		handed += drain(&mut self.reader);
		self.next += 1;
		assert_eq!(handed, 1, "the live group is in range");
	}
}

/// One reader judging datagrams no range admits; see `ranges_datagram`.
pub(crate) struct Datagrams {
	_broadcast: broadcast::Producer,
	track: track::Producer,
	reader: track::Subscriber,
	at: u64,
}

impl Datagrams {
	pub(crate) fn new(ranges: u64) -> Self {
		let broadcast = broadcast::Info::default().produce();
		let mut track = broadcast.create_track("bench", timed()).unwrap();
		// Each range presents from a time of its own through group 0, which every datagram
		// after the first passes.
		let subscription = track::Subscription::default()
			.with_max_delay(BUDGET)
			.with_ranges((0..ranges).map(|i| track::Range {
				start: track::Anchor::Time(millis(i)),
				end: Some(track::Anchor::Absolute(1)),
			}));
		let mut reader = track.subscribe(subscription);
		// Group 0 is in every range, so only the first datagram is admitted.
		track.append_datagram(millis(LIVE), Bytes::from_static(b"x")).unwrap();
		let waiter = kio::Waiter::noop();
		assert!(matches!(reader.poll_recv_datagram(&waiter), Poll::Ready(Ok(Some(_)))));
		Self {
			_broadcast: broadcast,
			track,
			reader,
			at: LIVE + 1,
		}
	}

	pub(crate) fn step(&mut self) {
		self.track
			.append_datagram(millis(self.at), Bytes::from_static(b"x"))
			.unwrap();
		self.at += 1;
		let waiter = kio::Waiter::noop();
		assert!(
			self.reader.poll_recv_datagram(&waiter).is_pending(),
			"no range admits the datagram"
		);
	}
}

/// A lite-07 publisher's frame bounds; see `ranges_frames`.
pub(crate) struct Frames {
	bounds: moq_net::fuzz::FrameBounds,
	ranges: u64,
	next: u64,
}

impl Frames {
	pub(crate) fn new(ranges: u64) -> Self {
		Self {
			bounds: moq_net::fuzz::FrameBounds::new(ranges),
			ranges,
			next: 0,
		}
	}

	pub(crate) fn step(&mut self) {
		// Each group in turn, a range's or the one between, so a lookup can't stop early.
		black_box(self.bounds.serve(self.next % (self.ranges * 2)));
		self.next += 1;
	}
}

fn sweep<S>(c: &mut Criterion, name: &str, new: impl Fn(u64) -> S, step: impl Fn(&mut S)) {
	let mut group = c.benchmark_group(name);
	group.throughput(Throughput::Elements(1));
	for ranges in RANGES {
		group.bench_function(BenchmarkId::from_parameter(ranges), |b| {
			let mut scenario = new(ranges);
			b.iter(|| step(&mut scenario));
		});
	}
	group.finish();
}

fn bench_time_end(c: &mut Criterion) {
	sweep(c, "ranges_time_end", TimeEnds::new, TimeEnds::step);
}

fn bench_time_cap(c: &mut Criterion) {
	sweep(c, "ranges_time_cap", TimeCap::new, TimeCap::step);
}

fn bench_fills_out(c: &mut Criterion) {
	sweep(c, "ranges_fills_out", FillsOut::new, FillsOut::step);
}

fn bench_sparse(c: &mut Criterion) {
	sweep(c, "ranges_sparse", Sparse::new, Sparse::step);
}

fn bench_time_gaps(c: &mut Criterion) {
	sweep(c, "ranges_time_gaps", TimeGaps::new, TimeGaps::step);
}

fn bench_datagram(c: &mut Criterion) {
	sweep(c, "ranges_datagram", Datagrams::new, Datagrams::step);
}

fn bench_frames(c: &mut Criterion) {
	sweep(c, "ranges_frames", Frames::new, Frames::step);
}

criterion_group!(
	benches,
	bench_time_end,
	bench_time_cap,
	bench_fills_out,
	bench_sparse,
	bench_time_gaps,
	bench_datagram,
	bench_frames
);
criterion_main!(benches);
