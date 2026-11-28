//! Delivery-path benchmarks for the group and track models.
//!
//! Small frames expose control and frame-slot allocation costs. The write sweep
//! includes the initial deque capacity and its first growth boundary, as well as
//! groups with thousands of tiny frames.
//!
//! `track_recv_groups` covers the layer above: how much it costs to hand out one
//! cached group, swept over cache depth so a per-delivery scan shows up as a slope.
//! It also covers the steady-state path after a subscriber has reached the edge.
//!
//! `track_stamp_groups` covers stamping a group's first frame, which checks it
//! against its neighbors' starts, swept over cache depth so a scan shows as a slope:
//! `append` among stamped groups, `sparse` among groups created but not yet stamped.
//!
//! `group_contended` reads one live multi-frame group from readers spread over worker
//! threads, swept over threads, readers, and frames appended per round, so contention
//! on the group's shared state shows up as a slope across cores. Its `subscribed` arm
//! hands each reader the group through its own track subscription, as a relay does, so
//! every read that parks at the edge also judges the group's expiry.
//!
//! `group_stream_read` reads one frame while it arrives from readers spread over worker
//! threads, swept over arrival chunks, threads, and readers, so contention on the bytes
//! the readers share shows up as a slope across cores.
//!
//! `group_shed` writes one live group far past what it holds, swept over the frames it
//! holds (a small pool's debt, or the 32 MiB live tail) and the frames it sheds, so
//! shedding the front stays a flat cost per frame.
//!
//! `group_paced` keeps a writer one frame past the 16 MiB window ahead of its fastest
//! reader, as a paced ingest runs, swept over readers: each frame read reports progress,
//! and the fastest reader's read wakes the parked writer.
//!
//! `group_flights` replays a live group half again past its 32 MiB tail to late readers
//! through flights, swept over readers and how far apart they join: within the 16 MiB a
//! flight admits they share one, further apart each asks its own. Each reader hands over
//! to the live copy once it catches up, and its flight stops.
//!
//! `group_compressed_updates` republishes single-frame compressed groups, swept over
//! tracks and overlapping groups, to expose encoder allocation on the update path.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench group`.

use std::{
	fmt,
	hint::black_box,
	sync::{
		Arc, LazyLock, Mutex,
		atomic::{AtomicU64, AtomicUsize, Ordering},
	},
	task::Poll,
	thread,
};

use bytes::Bytes;
use futures::FutureExt;
use moq_net::{Timestamp, broadcast, frame, group, track};
use swole::bench;

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> track::Info {
	track::Info::default()
		.with_max_age(None)
		.with_timescale(moq_net::Timescale::MILLI)
}

/// Where the group at `key` starts on a [`timed`] track: groups start strictly in key order.
fn at(key: u64) -> Timestamp {
	Timestamp::from_millis(key).unwrap()
}

/// A small, fixed payload shared across every frame. Cloning a `Bytes` is a
/// refcount bump (no allocation), so the benchmark isolates the per-frame control
/// overhead rather than payload allocation.
const PAYLOAD: usize = 64;

/// Frame counts to sweep. The top end is a long group of tiny frames, the most the
/// group cap used to allow. Counts 32 and 33 straddle a full write batch and its next
/// flush.
const COUNTS: [usize; 7] = [4, 5, 32, 33, 512, 2_048, 8_192];

/// Cached group counts to sweep for the track-level delivery benchmarks. A track
/// publishing one group per frame inside the default 30s cache expiry sits in the hundreds,
/// so the top end is deliberately past anything realistic: a per-delivery scan over
/// the cache shows up as a slope here, while a seek stays flat.
const DEPTHS: [usize; 3] = [64, 512, 4_096];

/// Groups appended and received in one caught-up benchmark iteration.
const LIVE_BATCH: usize = 128;

/// The payload every small frame clones.
fn payload() -> Bytes {
	Bytes::from(vec![0u8; PAYLOAD])
}

/// Keeps the broadcast/track producers alive alongside the group so the group
/// isn't torn down mid-benchmark. Only `group` is written to.
struct Ctx<G = group::Producer> {
	_broadcast: broadcast::Producer,
	_track: track::Producer,
	group: G,
}

/// Build a fresh, empty group via the public producer path.
fn fresh_group() -> Ctx {
	let broadcast = broadcast::Producer::new(broadcast::Info::default());
	let track = broadcast.create_track("bench", timed()).unwrap();
	let group = track.append_group(Timestamp::from_millis(0).unwrap()).unwrap();
	Ctx {
		_broadcast: broadcast,
		_track: track,
		group,
	}
}

/// Write N small frames into a fresh group (producer-side per-frame cost).
///
/// `single` is one `write_frame` per frame: a group lock plus a track-level eviction
/// settle each. `batch32` fills a `moq_net::fuzz::FrameBatch` and hands the whole thing to
/// `write_frames`, paying both once per batch.
mod group_write_frames {
	use swole::bench;

	use super::*;

	/// A fresh group, the frames to write into it, and a batch buffer.
	struct Write {
		ctx: Ctx,
		buf: moq_net::fuzz::FrameBatch<32>,
		payload: Bytes,
		frames: usize,
	}

	fn write(frames: usize) -> Write {
		Write {
			ctx: fresh_group(),
			buf: moq_net::fuzz::FrameBatch::new(),
			payload: payload(),
			frames,
		}
	}

	#[bench(args = COUNTS, setup = write, items = |n: usize| n)]
	fn batch32(mut write: Write) -> Write {
		for _ in 0..write.frames {
			let frame = frame::Frame::new(Timestamp::ZERO, write.payload.clone());
			// A full buffer hands the frame back: flush, then take it.
			if let Err(frame) = write.buf.push(frame) {
				write.buf.write(&mut write.ctx.group).unwrap();
				write.buf.push(frame).unwrap();
			}
		}
		write.buf.write(&mut write.ctx.group).unwrap();
		write
	}

	#[bench(args = COUNTS, setup = write, items = |n: usize| n)]
	fn single(mut write: Write) -> Write {
		for _ in 0..write.frames {
			write
				.ctx
				.group
				.write_frame(Timestamp::ZERO, write.payload.clone())
				.unwrap();
		}
		write
	}
}

/// A pre-filled, finished group plus a consumer positioned at its first frame.
struct Filled {
	_ctx: Ctx<group::Finished>,
	consumer: group::Consumer,
	frames: usize,
}

fn filled_group(frames: usize) -> Filled {
	let payload = payload();
	let Ctx {
		_broadcast,
		_track,
		mut group,
	} = fresh_group();
	for _ in 0..frames {
		group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
	}
	let group = group.finish().unwrap();
	let consumer = group.consume();
	Filled {
		_ctx: Ctx {
			_broadcast,
			_track,
			group,
		},
		consumer,
		frames,
	}
}

/// Drain a whole group through a reused `moq_net::fuzz::FrameBatch<N>`.
fn drain_batched<const N: usize>(consumer: &mut group::Consumer, buf: &mut moq_net::fuzz::FrameBatch<N>) {
	loop {
		let batch = buf.read(consumer).now_or_never().unwrap().unwrap();
		if batch.is_empty() {
			break;
		}
		for frame in batch.iter() {
			black_box(frame);
		}
	}
}

/// Drain N small frames from a pre-filled group (consumer-side per-frame cost).
///
/// `single` is one frame at a time via [`group::Consumer::read_frame`]: a lock and a
/// waker each. `batch{N}` is [`group::Consumer::read_frames`] cloning the ready tail
/// into a reused `N`-frame buffer, amortizing both across the batch.
///
/// Throughput keeps climbing to 32 and stalls after (128 falls out of L1), but the
/// default capacity is 8: the tail of that curve only pays off for a reader draining a
/// backlog, while every buffer pays its stack whether or not the batch fills.
mod group_read_frames {
	use swole::bench;

	use super::*;

	#[bench(args = COUNTS, setup = filled_group, items = |n: usize| n)]
	fn single(mut filled: Filled) -> Filled {
		for _ in 0..filled.frames {
			// All frames are already present and finished, so a single poll
			// resolves immediately (no runtime needed).
			let frame = filled.consumer.read_frame().now_or_never().unwrap().unwrap();
			black_box(frame);
		}
		filled
	}

	// The buffer is allocated in setup and refilled for the whole group, like a
	// real reader that keeps one per task.
	macro_rules! batched {
		($name:ident, $setup:ident, $cap:literal) => {
			fn $setup(frames: usize) -> (Filled, moq_net::fuzz::FrameBatch<$cap>) {
				(filled_group(frames), moq_net::fuzz::FrameBatch::new())
			}

			#[bench(args = COUNTS, setup = $setup, items = |n: usize| n)]
			fn $name(
				(mut filled, mut buf): (Filled, moq_net::fuzz::FrameBatch<$cap>),
			) -> (Filled, moq_net::fuzz::FrameBatch<$cap>) {
				drain_batched(&mut filled.consumer, &mut buf);
				(filled, buf)
			}
		};
	}

	batched!(batch4, batch4_group, 4);
	batched!(batch8, batch8_group, 8);
	batched!(batch32, batch32_group, 32);
	batched!(batch128, batch128_group, 128);
}

/// Keeps the broadcast alive alongside the track being drained.
struct TrackCtx {
	_broadcast: broadcast::Producer,
	track: track::Producer,
}

/// Build a track holding N cached groups, each with a single small frame.
fn filled_track(n: usize, payload: &Bytes) -> TrackCtx {
	let broadcast = broadcast::Producer::new(broadcast::Info::default());
	let track = broadcast.create_track("bench", timed()).unwrap();
	for key in 0..n as u64 {
		let mut group = track.append_group(Timestamp::from_millis(key).unwrap()).unwrap();
		group.write_frame(at(key), payload.clone()).unwrap();
		group.finish().unwrap();
	}
	TrackCtx {
		_broadcast: broadcast,
		track,
	}
}

/// Request the full cache window instead of the default live edge.
fn replay() -> track::Subscription {
	track::Subscription::default().with_ranges([track::Range {
		start: track::Start::Time(0),
		end: None,
		..Default::default()
	}])
}

/// Drain N cached groups from a track, in arrival order and in key order.
///
/// The two differ in how they find the next group: `recv_group` walks an arrival
/// index, while `next_group` seeks by key. Both should be flat in the cache
/// depth; a per-delivery scan over the cache makes the key-ordered arm
/// quadratic in N.
mod track_recv_groups {
	use swole::bench;

	use super::*;

	fn arrival_track(n: usize) -> (TrackCtx, track::Subscriber, usize) {
		let ctx = filled_track(n, &payload());
		let subscriber = ctx.track.subscribe(replay());
		(ctx, subscriber, n)
	}

	#[bench(args = DEPTHS, setup = arrival_track, items = |n: usize| n)]
	fn arrival((ctx, mut subscriber, n): (TrackCtx, track::Subscriber, usize)) -> (TrackCtx, track::Subscriber) {
		for _ in 0..n {
			let group = subscriber.recv_group().now_or_never().unwrap().unwrap().unwrap();
			black_box(group);
		}
		(ctx, subscriber)
	}

	fn key_track(n: usize) -> (TrackCtx, track::Ordered, usize) {
		let ctx = filled_track(n, &payload());
		let subscriber = ctx.track.subscribe(replay()).ordered();
		(ctx, subscriber, n)
	}

	#[bench(args = DEPTHS, setup = key_track, items = |n: usize| n)]
	fn key((ctx, mut subscriber, n): (TrackCtx, track::Ordered, usize)) -> (TrackCtx, track::Ordered) {
		for _ in 0..n {
			let group = subscriber.next_group().now_or_never().unwrap().unwrap().unwrap();
			black_box(group);
		}
		(ctx, subscriber)
	}

	/// A subscriber at the live edge of a track holding N groups.
	struct Edge {
		ctx: TrackCtx,
		subscriber: track::Subscriber,
		payload: Bytes,
		next: usize,
	}

	fn edge(n: usize) -> Edge {
		let payload = payload();
		let ctx = filled_track(n, &payload);
		let mut subscriber = ctx.track.subscribe(None);
		let edge = subscriber.recv_group().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(edge.start, n as u64 - 1);
		Edge {
			ctx,
			subscriber,
			payload,
			next: n,
		}
	}

	#[bench(args = DEPTHS, setup = edge, items = |_: usize| LIVE_BATCH)]
	fn caught_up(mut edge: Edge) -> Edge {
		for key in (edge.next..edge.next + LIVE_BATCH).map(|key| key as u64) {
			let mut group = edge
				.ctx
				.track
				.append_group(Timestamp::from_millis(key).unwrap())
				.unwrap();
			group.write_frame(at(key), edge.payload.clone()).unwrap();
			group.finish().unwrap();
			let group = edge.subscriber.recv_group().now_or_never().unwrap().unwrap().unwrap();
			black_box(group);
		}
		edge
	}
}

fn roundtrip(frames: usize) -> (Ctx, Bytes, usize) {
	(fresh_group(), payload(), frames)
}

/// The full lifecycle: build a group, write N frames, then drain them.
#[bench(args = COUNTS, setup = roundtrip, items = |n: usize| n)]
fn group_roundtrip(
	(mut ctx, payload, frames): (Ctx, Bytes, usize),
) -> (broadcast::Producer, track::Producer, group::Finished, group::Consumer) {
	for _ in 0..frames {
		ctx.group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
	}
	let group = ctx.group.finish().unwrap();
	let mut consumer = group.consume();
	for _ in 0..frames {
		let frame = consumer.read_frame().now_or_never().unwrap().unwrap();
		black_box(frame);
	}
	(ctx._broadcast, ctx._track, group, consumer)
}

/// One finished group read in full by every subscriber, for plain and property-bearing
/// objects, named `<frames>/<subscribers>`.
mod group_relay_objects {
	use swole::bench;

	use super::*;

	#[derive(Clone, Copy)]
	struct Case {
		frames: usize,
		subscribers: usize,
	}

	impl fmt::Display for Case {
		fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
			write!(f, "{}/{}", self.frames, self.subscribers)
		}
	}

	fn cases() -> Vec<Case> {
		[4, 512, 8192]
			.into_iter()
			.flat_map(|frames| [1, 8, 64].map(|subscribers| Case { frames, subscribers }))
			.collect()
	}

	/// A finished group, and the readers the next call drains, handed out between calls.
	struct Relay {
		context: (broadcast::Producer, track::Producer, group::Finished),
		subscribers: usize,
		readers: Vec<group::Consumer>,
	}

	fn relay(case: Case, properties: bool) -> Relay {
		Relay {
			context: moq_net::fuzz::object_group(case.frames, properties),
			subscribers: case.subscribers,
			readers: Vec::new(),
		}
	}

	fn plain_group(case: Case) -> Relay {
		relay(case, false)
	}

	fn properties_group(case: Case) -> Relay {
		relay(case, true)
	}

	fn readers(relay: &mut Relay) {
		relay.readers = (0..relay.subscribers).map(|_| relay.context.2.consume()).collect();
	}

	fn read(relay: &mut Relay) -> Vec<group::Consumer> {
		let mut readers = std::mem::take(&mut relay.readers);
		let mut buffer = moq_net::fuzz::ObjectBatch::<16>::new();
		let waiter = kio::Waiter::noop();
		for reader in &mut readers {
			loop {
				let Poll::Ready(count) = buffer.poll_read(&waiter, reader) else {
					panic!("finished group blocked")
				};
				if count.unwrap() == 0 {
					break;
				}
				buffer.inspect();
			}
		}
		readers
	}

	#[bench(args = cases(), setup = plain_group, before = readers, items = |case: Case| case.frames * case.subscribers)]
	fn plain(relay: &mut Relay) -> Vec<group::Consumer> {
		read(relay)
	}

	#[bench(args = cases(), setup = properties_group, before = readers, items = |case: Case| case.frames * case.subscribers)]
	fn properties(relay: &mut Relay) -> Vec<group::Consumer> {
		read(relay)
	}
}

/// Append and stamp groups on a track already holding N cached groups.
///
/// A group's first frame is checked against the nearest stamped starts on either side,
/// once per group. A fresh track takes at most N more groups before `before` replaces it,
/// so the cache stays within 2N.
mod track_stamp_groups {
	use swole::bench;

	use super::*;

	/// A track of N groups taking one more per call.
	struct Append {
		depth: usize,
		waiting: bool,
		ctx: TrackCtx,
		/// A pending fetch, its handler, and the waiter it parked on, held only to stay alive.
		_fetching: Option<(track::Requests, kio::Pending<track::Fetching>, kio::Waiter)>,
		payload: Bytes,
		next: u64,
	}

	impl Append {
		fn new(depth: usize, waiting: bool) -> Self {
			let payload = payload();
			let ctx = filled_track(depth, &payload);
			// A pending fetch keeps the landing index occupied beyond this batch,
			// separating the no-reader fast path from an unrelated parked read.
			let fetching = waiting.then(|| {
				let requests = ctx.track.requests();
				let mut fetching = ctx.track.consume().fetch_group((2 * depth + 1) as u64, None);
				let waiter = kio::Waiter::noop();
				assert!(kio::Task::poll(&mut *fetching, &waiter).is_pending());
				(requests, fetching, waiter)
			});
			Self {
				depth,
				waiting,
				ctx,
				_fetching: fetching,
				payload,
				next: depth as u64,
			}
		}
	}

	fn append_track(depth: usize) -> Append {
		Append::new(depth, false)
	}

	fn waiting_track(depth: usize) -> Append {
		Append::new(depth, true)
	}

	/// Replace a track that took N more groups.
	fn refill(append: &mut Append) {
		if append.next == 2 * append.depth as u64 {
			*append = Append::new(append.depth, append.waiting);
		}
	}

	fn stamp(append: &mut Append) {
		let key = append.next;
		append.next += 1;
		let mut group = append
			.ctx
			.track
			.append_group(Timestamp::from_millis(key).unwrap())
			.unwrap();
		group.write_frame(at(key), append.payload.clone()).unwrap();
		group.finish().unwrap();
	}

	#[bench(args = DEPTHS, setup = append_track, before = refill)]
	fn append(append: &mut Append) {
		stamp(append)
	}

	#[bench(args = DEPTHS, setup = waiting_track, before = refill)]
	fn append_waiting(append: &mut Append) {
		stamp(append)
	}

	/// N groups created before any is stamped, then stamped in order: each one's upper
	/// neighbor is a run of unstamped groups, which a slot scan walks.
	struct Sparse {
		_ctx: TrackCtx,
		groups: Vec<group::Producer>,
		payload: Bytes,
		next: usize,
	}

	fn sparse_track(depth: usize) -> Sparse {
		let payload = payload();
		let ctx = filled_track(0, &payload);
		let groups = (0..depth as u64)
			.map(|key| ctx.track.append_group(Timestamp::from_millis(key).unwrap()).unwrap())
			.collect();
		Sparse {
			_ctx: ctx,
			groups,
			payload,
			next: 0,
		}
	}

	/// Replace a track whose groups are all stamped.
	fn restamp(sparse: &mut Sparse) {
		if sparse.next == sparse.groups.len() {
			*sparse = sparse_track(sparse.groups.len());
		}
	}

	#[bench(args = DEPTHS, setup = sparse_track, before = restamp)]
	fn sparse(sparse: &mut Sparse) {
		let group = &mut sparse.groups[sparse.next];
		sparse.next += 1;
		let start = at(group.start);
		group.write_frame(start, sparse.payload.clone()).unwrap();
	}
}

/// Fresh compressed groups, each read by N plain readers, named `groups<G>/<readers>`.
#[derive(Clone, Copy)]
struct Inflate {
	groups: usize,
	readers: usize,
}

impl fmt::Display for Inflate {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "groups{}/{}", self.groups, self.readers)
	}
}

/// Frames in each compressed group.
const INFLATE_FRAMES: usize = 32;

fn inflate_cases() -> Vec<Inflate> {
	[1, 16]
		.into_iter()
		.flat_map(|groups| [1, 8, 64].map(|readers| Inflate { groups, readers }))
		.collect()
}

fn inflate(case: Inflate) -> (broadcast::Producer, track::Producer, Vec<group::Consumer>) {
	let payloads: Vec<Bytes> = (0..INFLATE_FRAMES)
		.map(|index| Bytes::from(format!("{{\"frame\":{index},\"status\":\"{}\"}}", "ok".repeat(100))))
		.collect();
	let broadcast = broadcast::Producer::new(broadcast::Info::default());
	let track = broadcast
		.create_track("bench", timed().with_compression(moq_net::track::Compression::Deflate))
		.unwrap();
	let consumers: Vec<group::Consumer> = (0..case.groups as u64)
		.flat_map(|key| {
			let mut group = track.append_group(Timestamp::from_millis(key).unwrap()).unwrap();
			for payload in &payloads {
				group.write_frame(at(key), payload.clone()).unwrap();
			}
			let group = group.finish().unwrap();
			(0..case.readers).map(move |_| group.consume()).collect::<Vec<_>>()
		})
		.collect();
	(broadcast, track, consumers)
}

/// Read every frame of fresh compressed groups with N plain readers each.
///
/// The group inflates once into a copy every plain reader shares, so the cost should
/// slope with the groups and stay nearly flat in the readers; a per-reader inflate
/// shows up as a slope over the readers.
#[bench(args = inflate_cases(), setup = inflate, items = |case: Inflate| case.groups * case.readers * INFLATE_FRAMES)]
fn group_inflate(
	(broadcast, track, mut consumers): (broadcast::Producer, track::Producer, Vec<group::Consumer>),
) -> (broadcast::Producer, track::Producer, Vec<group::Consumer>) {
	for consumer in &mut consumers {
		while let Some(frame) = consumer.read_frame().now_or_never().unwrap().unwrap() {
			black_box(frame);
		}
	}
	(broadcast, track, consumers)
}

/// Single-frame compressed updates on `tracks` tracks, `concurrent` groups overlapping
/// within each, named `tracks<T>/<concurrent>`.
#[derive(Clone, Copy)]
struct Updates {
	tracks: usize,
	concurrent: usize,
}

impl fmt::Display for Updates {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "tracks{}/{}", self.tracks, self.concurrent)
	}
}

/// Updates each track takes per call.
const UPDATES: usize = 32;

fn updates_cases() -> Vec<Updates> {
	[1, 16]
		.into_iter()
		.flat_map(|tracks| [1, 4].map(|concurrent| Updates { tracks, concurrent }))
		.collect()
}

/// Tracks that each published one compressed group, and room for the overlapping ones.
struct Compressed {
	_broadcast: broadcast::Producer,
	producers: Vec<track::Producer>,
	groups: Vec<group::Producer>,
	concurrent: usize,
	payload: Bytes,
}

fn compressed(case: Updates) -> Compressed {
	let payload = Bytes::from(format!("{{\"status\":\"{}\"}}", "ready".repeat(100)));
	let broadcast = broadcast::Info::default().produce();
	let producers = (0..case.tracks)
		.map(|i| {
			let track = broadcast
				.create_track(
					format!("bench{i}"),
					track::Info::default().with_compression(moq_net::track::Compression::Deflate),
				)
				.unwrap();
			let mut group = track.append_group(Timestamp::unscaled(0)).unwrap();
			group
				.write_frame(moq_net::Timestamp::unscaled(group.start), payload.clone())
				.unwrap();
			group.finish().unwrap();
			track
		})
		.collect();
	Compressed {
		_broadcast: broadcast,
		producers,
		groups: Vec::with_capacity(case.concurrent),
		concurrent: case.concurrent,
		payload,
	}
}

/// Repeated single-frame compressed updates on independent tracks. Groups overlap
/// within each update so only the one spare per track can be reused.
#[bench(
	args = updates_cases(),
	setup = compressed,
	items = |case: Updates| UPDATES * case.tracks * case.concurrent,
)]
fn group_compressed_updates(mut compressed: Compressed) -> Compressed {
	for update in 0..UPDATES {
		for track in &compressed.producers {
			for index in 0..compressed.concurrent {
				let key = (1 + update * compressed.concurrent + index) as u64;
				let mut group = track.append_group(Timestamp::unscaled(key)).unwrap();
				group
					.write_frame(moq_net::Timestamp::unscaled(group.start), compressed.payload.clone())
					.unwrap();
				compressed.groups.push(group);
			}
			for group in compressed.groups.drain(..) {
				group.finish().unwrap();
			}
		}
	}
	compressed
}

/// Worker threads the contended readers are spread over.
const THREADS: [usize; 3] = [1, 4, 16];

/// Readers of the one contended group.
const READERS: [usize; 3] = [16, 256, 1_024];

/// Frames appended per contended round: one at the edge, or a burst that readers drain
/// several at a time.
const BURSTS: [usize; 2] = [1, 8];

/// Frames a contended group takes before the driver rolls to the next one.
const ROLL: usize = 4_096;

/// One pass of a worker over its readers.
type Work = Box<dyn FnMut() + Send>;

/// Worker threads that each run one pass when the driver releases a round.
struct Crew {
	control: Arc<Control>,
	threads: Vec<thread::JoinHandle<()>>,
}

#[derive(Default)]
struct Control {
	/// Rounds released so far, or [`STOP`].
	round: AtomicU64,
	/// Workers done with the current round.
	done: AtomicUsize,
}

const STOP: u64 = u64::MAX;

impl Crew {
	fn new(work: Vec<Work>) -> Self {
		let control = Arc::new(Control::default());
		let threads = work
			.into_iter()
			.map(|mut work| {
				let control = control.clone();
				thread::spawn(move || {
					let mut seen = 0;
					loop {
						seen = spin(|| {
							let round = control.round.load(Ordering::Acquire);
							(round != seen).then_some(round)
						});
						if seen == STOP {
							return;
						}
						work();
						control.done.fetch_add(1, Ordering::AcqRel);
					}
				})
			})
			.collect();
		Self { control, threads }
	}

	/// Run one pass on every worker and wait for all of them.
	fn run(&self) {
		self.control.done.store(0, Ordering::Relaxed);
		self.control.round.fetch_add(1, Ordering::Release);
		let threads = self.threads.len();
		spin(|| {
			// A panicked worker never reports done, so fail instead of spinning forever.
			assert!(!self.threads.iter().any(|t| t.is_finished()), "worker panicked");
			(self.control.done.load(Ordering::Acquire) == threads).then_some(())
		});
	}
}

impl Drop for Crew {
	fn drop(&mut self) {
		self.control.round.store(STOP, Ordering::Release);
		for thread in self.threads.drain(..) {
			let result = thread.join();
			// Already unwinding from `run`'s check: a second panic would abort.
			if !thread::panicking() {
				result.expect("worker panicked");
			}
		}
	}
}

/// Busy-wait for `f`, yielding once spinning has gone on long enough that the machine
/// may be oversubscribed.
fn spin<R>(mut f: impl FnMut() -> Option<R>) -> R {
	let mut spins = 0u32;
	loop {
		if let Some(r) = f() {
			return r;
		}
		if spins < 1_000 {
			spins += 1;
			std::hint::spin_loop();
		} else {
			thread::yield_now();
		}
	}
}

/// One `group_contended` case, named `[subscribed/]burst_<B>/threads_<T>/<readers>`.
#[derive(Clone, Copy)]
struct Contention {
	subscribed: bool,
	burst: usize,
	threads: usize,
	readers: usize,
}

impl fmt::Display for Contention {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		let source = if self.subscribed { "subscribed/" } else { "" };
		write!(
			f,
			"{source}burst_{}/threads_{}/{}",
			self.burst, self.threads, self.readers
		)
	}
}

fn contention_cases() -> Vec<Contention> {
	let mut cases = Vec::new();
	for subscribed in [false, true] {
		for burst in BURSTS {
			for threads in THREADS {
				for readers in READERS {
					cases.push(Contention {
						subscribed,
						burst,
						threads,
						readers,
					});
				}
			}
		}
	}
	cases
}

/// One live group its readers drain on worker threads. The crew is first so its workers
/// stop before the group they read drops.
struct Contended {
	crew: Crew,
	_broadcast: broadcast::Producer,
	track: track::Producer,
	group: group::Producer,
	key: u64,
	/// The group readers move to once theirs ends.
	next: Arc<Mutex<group::Consumer>>,
	written: usize,
	burst: usize,
	payload: Bytes,
}

fn contended(case: Contention) -> Contended {
	let Contention {
		subscribed,
		burst,
		threads,
		readers,
	} = case;
	let broadcast = broadcast::Producer::new(broadcast::Info::default());
	let track = broadcast
		.create_track("bench", track::Info::default().with_max_age(None))
		.unwrap();
	let group = track.append_group(Timestamp::unscaled(0)).unwrap();
	let next = Arc::new(Mutex::new(group.consume()));
	let mut sets: Vec<Vec<(Option<track::Subscriber>, group::Consumer)>> = (0..threads).map(|_| Vec::new()).collect();
	for i in 0..readers {
		let reader = match subscribed {
			true => {
				let mut subscriber = track.subscribe(None);
				let group = subscriber.recv_group().now_or_never().unwrap().unwrap().unwrap();
				assert_eq!(group.start, 0, "the subscriber starts at the live group");
				(Some(subscriber), group)
			}
			false => (None, group.consume()),
		};
		sets[i % threads].push(reader);
	}
	let work = sets
		.into_iter()
		.map(|mut set| -> Work {
			let next = next.clone();
			let waiter = kio::Waiter::noop();
			Box::new(move || {
				for (subscriber, reader) in &mut set {
					let mut read = 0;
					loop {
						match reader.poll_read_frame(&waiter) {
							Poll::Ready(Ok(Some(frame))) => {
								black_box(frame);
								read += 1;
							}
							Poll::Ready(Ok(None)) => {
								*reader = match subscriber {
									Some(subscriber) => {
										let next = subscriber.recv_group().now_or_never();
										next.unwrap().unwrap().expect("the next group is live")
									}
									None => next.lock().unwrap().clone(),
								}
							}
							Poll::Ready(Err(err)) => panic!("read failed: {err}"),
							Poll::Pending => break,
						}
					}
					assert_eq!(read, burst, "every appended frame is ready");
				}
			})
		})
		.collect();
	Contended {
		crew: Crew::new(work),
		_broadcast: broadcast,
		track,
		group,
		key: 0,
		next,
		written: 0,
		burst,
		payload: payload(),
	}
}

/// Read one live group from readers on several threads.
///
/// Each call the driver appends a burst of frames; every reader drains them with
/// [`group::Consumer::poll_read_frame`] and parks at the edge again. Every [`ROLL`]
/// frames the driver publishes the next group and finishes the current one, and each
/// reader moves on once it reads the end, so the group stays under its frame cap.
///
/// A `subscribed` reader receives each group from its own [`track::Subscriber`], so the
/// group carries the subscription's expiry and every park judges it.
#[bench(
	args = contention_cases(),
	setup = contended,
	items = |case: Contention| case.readers * case.burst,
)]
fn group_contended(contended: &mut Contended) {
	if contended.written + contended.burst > ROLL {
		contended.key += 1;
		let rolled = contended
			.track
			.append_group(Timestamp::unscaled(contended.key))
			.unwrap();
		*contended.next.lock().unwrap() = rolled.consume();
		let done = std::mem::replace(&mut contended.group, rolled);
		done.finish().unwrap();
		contended.written = 0;
	}
	for _ in 0..contended.burst {
		let start = contended.group.start;
		contended
			.group
			.write_frame(moq_net::Timestamp::unscaled(start), contended.payload.clone())
			.unwrap();
	}
	contended.written += contended.burst;
	contended.crew.run();
}

/// Bytes each call of [`group_stream_read`] adds to the frame in flight.
const STREAM_CHUNK: usize = 1024;

/// Chunks a streamed frame arrives in.
const STREAM_ARRIVALS: [usize; 2] = [4, 16];

/// Readers of the one streamed frame.
const STREAM_READERS: [usize; 2] = [16, 256];

/// One `group_stream_read` case, named `arrivals_<A>/threads_<T>/<readers>`.
#[derive(Clone, Copy)]
struct Arrival {
	arrivals: usize,
	threads: usize,
	readers: usize,
}

impl fmt::Display for Arrival {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(
			f,
			"arrivals_{}/threads_{}/{}",
			self.arrivals, self.threads, self.readers
		)
	}
}

fn arrival_cases() -> Vec<Arrival> {
	let mut cases = Vec::new();
	for arrivals in STREAM_ARRIVALS {
		for threads in THREADS {
			for readers in STREAM_READERS {
				cases.push(Arrival {
					arrivals,
					threads,
					readers,
				});
			}
		}
	}
	cases
}

/// One frame in flight its readers read on worker threads. The crew is first so its
/// workers stop before the frame they read drops.
struct Streaming {
	crew: Crew,
	_broadcast: broadcast::Producer,
	track: track::Producer,
	key: u64,
	/// The group readers move to once theirs ends.
	next: Arc<Mutex<group::Consumer>>,
	frames: usize,
	writing: Option<frame::Producer>,
	arrivals: usize,
	chunk: Bytes,
}

impl Streaming {
	fn info(&self) -> frame::Info {
		frame::Info {
			size: (self.arrivals * self.chunk.len()) as u64,
			timestamp: moq_net::Timestamp::unscaled(self.key),
		}
	}
}

fn streaming(case: Arrival) -> Streaming {
	let Arrival {
		arrivals,
		threads,
		readers,
	} = case;
	let broadcast = broadcast::Producer::new(broadcast::Info::default());
	let track = broadcast
		.create_track("bench", track::Info::default().with_max_age(None))
		.unwrap();
	let group = track.append_group(Timestamp::unscaled(0)).unwrap();
	let next = Arc::new(Mutex::new(group.consume()));
	let mut sets: Vec<Vec<(group::Consumer, Option<frame::Consumer>)>> = (0..threads).map(|_| Vec::new()).collect();
	for i in 0..readers {
		sets[i % threads].push((group.consume(), None));
	}
	let work = sets
		.into_iter()
		.map(|mut set| -> Work {
			let next = next.clone();
			let waiter = kio::Waiter::noop();
			Box::new(move || {
				for (group, frame) in &mut set {
					let mut read = 0;
					loop {
						let reading = match frame {
							Some(frame) => frame,
							None => match group.next_frame().now_or_never() {
								Some(Ok(Some(next))) => frame.insert(next),
								Some(Ok(None)) => {
									*group = next.lock().unwrap().clone();
									continue;
								}
								Some(Err(err)) => panic!("next frame failed: {err}"),
								None => break,
							},
						};
						match reading.poll_read_chunk(&waiter) {
							Poll::Ready(Ok(Some(chunk))) => {
								black_box(chunk);
								read += 1;
							}
							Poll::Ready(Ok(None)) => *frame = None,
							Poll::Ready(Err(err)) => panic!("read failed: {err}"),
							Poll::Pending => break,
						}
					}
					assert_eq!(read, 1, "every written chunk is ready");
				}
			})
		})
		.collect();
	let mut streaming = Streaming {
		crew: Crew::new(work),
		_broadcast: broadcast,
		track,
		key: 0,
		next,
		frames: 0,
		writing: None,
		arrivals,
		chunk: Bytes::from(vec![0u8; STREAM_CHUNK]),
	};
	streaming.writing = Some(group.create_frame(streaming.info()).unwrap());
	streaming
}

/// Read one frame while it arrives from readers on several threads, as a relay forwards
/// a large frame to its subscribers.
///
/// Each call the driver writes one chunk of the frame in flight, and every reader reads
/// it with [`frame::Consumer::poll_read_chunk`] and parks again. Readers at the same
/// published length share one owner of its bytes, so contention on that owner shows up
/// as a slope across cores. A finished frame is followed by the next in the same group,
/// rolling to a new group every [`ROLL`] frames.
#[bench(args = arrival_cases(), setup = streaming, items = |case: Arrival| case.readers)]
fn group_stream_read(streaming: &mut Streaming) {
	let mut frame = streaming.writing.take().unwrap();
	frame.write(streaming.chunk.clone()).unwrap();
	streaming.writing = Some(match frame.remaining() {
		0 => {
			let group = frame.finish().unwrap();
			streaming.frames += 1;
			if streaming.frames == ROLL {
				streaming.key += 1;
				let rolled = streaming
					.track
					.append_group(Timestamp::unscaled(streaming.key))
					.unwrap();
				*streaming.next.lock().unwrap() = rolled.consume();
				group.finish().unwrap();
				streaming.frames = 0;
				rolled.create_frame(streaming.info()).unwrap()
			} else {
				group.create_frame(streaming.info()).unwrap()
			}
		}
		_ => frame,
	});
	streaming.crew.run();
}

/// A frame for the shedding benchmark: large enough that a group outgrows a small pool
/// in a few hundred frames.
const SHED_PAYLOAD: usize = 16 * 1024;

/// A live group cached in a pool of `capacity` bytes, or an unbounded one.
fn pooled_group(capacity: Option<u64>) -> Ctx {
	let mut info = broadcast::Info::default();
	if let Some(capacity) = capacity {
		info.pool = moq_net::cache::Pool::new(moq_net::cache::Config::default().with_capacity(capacity));
	}
	let broadcast = broadcast::Producer::new(info);
	let track = broadcast.create_track("bench", timed()).unwrap();
	let group = track.append_group(Timestamp::from_millis(0).unwrap()).unwrap();
	Ctx {
		_broadcast: broadcast,
		_track: track,
		group,
	}
}

/// One `group_shed` case: what the group holds, and the frames written past it.
#[derive(Clone, Copy)]
struct Shed {
	held: &'static str,
	pool: Option<u64>,
	frames: usize,
}

impl fmt::Display for Shed {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}/{}", self.held, self.frames)
	}
}

fn shed_cases() -> Vec<Shed> {
	[
		("pool-1MiB", Some(1 << 20)),
		("pool-16MiB", Some(16 << 20)),
		("tail-32MiB", None),
	]
	.into_iter()
	// Twice what the tail holds, and sixteen times.
	.flat_map(|(held, pool)| [4_096, 32_768].map(|frames| Shed { held, pool, frames }))
	.collect()
}

fn shed(case: Shed) -> (Ctx, Bytes, usize) {
	(
		pooled_group(case.pool),
		Bytes::from(vec![0u8; SHED_PAYLOAD]),
		case.frames,
	)
}

/// Write a live group far past what it holds, which sheds its front to make room: under
/// a small pool's debt, or past the 32 MiB live tail with no pool capacity.
///
/// The time per frame should stay flat across both axes: a shed costs the frames it
/// drops, never the frames the group still holds.
#[bench(args = shed_cases(), setup = shed, items = |case: Shed| case.frames)]
fn group_shed((mut ctx, payload, frames): (Ctx, Bytes, usize)) -> Ctx {
	for _ in 0..frames {
		ctx.group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
	}
	ctx
}

/// Counts wakes, so a bench can check that reads let a parked writer on.
#[derive(Default)]
struct Wakes(AtomicU64);

impl std::task::Wake for Wakes {
	fn wake(self: Arc<Self>) {
		self.0.fetch_add(1, Ordering::Relaxed);
	}
}

/// A writer held one frame past the pacing window, and its readers.
struct Paced {
	ctx: Ctx,
	consumers: Vec<group::Consumer>,
	writer: kio::Waiter,
	reader: kio::Waiter,
	next: frame::Info,
	payload: Bytes,
}

impl Paced {
	/// One round, returning whether the writer parked.
	fn round(&mut self) -> bool {
		let parked = self.ctx.group.poll_ready(&self.writer, &self.next).is_pending();
		for consumer in &mut self.consumers {
			let Poll::Ready(Ok(Some(frame))) = consumer.poll_read_frame(&self.reader) else {
				panic!("a frame is ready");
			};
			black_box(frame);
		}
		self.ctx
			.group
			.write_frame(Timestamp::ZERO, self.payload.clone())
			.unwrap();
		parked
	}
}

fn paced(readers: usize) -> Paced {
	// Payloads are shared clones, so a megabyte costs a refcount and the window is 16 frames.
	let payload = Bytes::from(vec![0u8; 1 << 20]);
	let mut ctx = fresh_group();
	let consumers = (0..readers).map(|_| ctx.group.consume()).collect();
	// One frame past the window, so the writer is paced from the first round.
	for _ in 0..17 {
		ctx.group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
	}
	let wakes = Arc::new(Wakes::default());
	let mut paced = Paced {
		ctx,
		consumers,
		writer: kio::Waiter::new(wakes.clone().into()),
		reader: kio::Waiter::noop(),
		next: moq_net::frame::Info {
			size: payload.len() as u64,
			timestamp: Timestamp::ZERO,
		},
		payload,
	};
	// One round leaves the writer as far ahead as it found it.
	assert!(paced.round(), "the writer never parked");
	assert!(wakes.0.load(Ordering::Relaxed) > 0, "no read woke the writer");
	paced
}

/// A writer held at the pacing window: each call it polls [`group::Producer::poll_ready`],
/// parking whenever it is past the window, every reader takes one frame, and the writer
/// writes the next. A reader reports a prefetched batch at once, so the fastest one's batch
/// read wakes the parked writer. Throughput counts frames read, so the per-read progress
/// report and the park and wake show up together, and the readers trailing the fastest one
/// as a slope across readers.
#[bench(args = [1, 8, 64], setup = paced, items = |readers: usize| readers)]
fn group_paced(paced: &mut Paced) -> bool {
	paced.round()
}

/// 48 MiB of 16 KiB frames: half again the live tail.
const FLIGHT_FRAMES: usize = 3_072;

/// A live group of [`FLIGHT_FRAMES`] on a track with a handler, its live reader at the edge.
struct Flights {
	_broadcast: broadcast::Producer,
	pool: moq_net::cache::Pool,
	requests: track::Requests,
	live: group::Producer,
	_reader: group::Consumer,
}

fn shed_live_group(payloads: &[Bytes]) -> Flights {
	let info = broadcast::Info::default();
	let pool = info.pool.clone();
	let broadcast = broadcast::Producer::new(info);
	let track = broadcast.create_track("bench", track::Info::default()).unwrap();
	let requests = track.requests();
	let mut live = track.append_group(Timestamp::unscaled(0)).unwrap();
	let mut reader = live.consume();
	let waiter = kio::Waiter::noop();
	for payload in payloads {
		live.write_frame(moq_net::Timestamp::unscaled(live.start), payload.clone())
			.unwrap();
		while let Poll::Ready(Ok(Some(_))) = reader.poll_read_frame(&waiter) {}
	}
	Flights {
		_broadcast: broadcast,
		pool,
		requests,
		live,
		_reader: reader,
	}
}

/// A flight the handler writes, from the frame it was asked at.
struct Flight {
	group: group::Producer,
	demand: group::Demand,
	next: usize,
	ridden: bool,
}

/// Replay the live group from frame 0 to `readers` late readers, each joining once the one
/// before it has read `apart` frames, and return how many flights the handler served, the
/// frames it wrote, and the most the pool held.
fn replay_flights(ctx: &Flights, payloads: &[Bytes], readers: usize, apart: usize) -> (usize, usize, u64) {
	let waiter = kio::Waiter::noop();
	let mut riders: Vec<(group::Consumer, usize)> = Vec::new();
	let mut flights: Vec<Flight> = Vec::new();
	let (mut served, mut written) = (0, 0);
	let mut peak = 0;
	while riders.len() < readers || riders.iter().any(|(_, read)| *read < payloads.len()) {
		while riders.len() < readers && riders.last().is_none_or(|(_, read)| *read >= apart) {
			riders.push((ctx.live.consume(), 0));
		}
		// A megabyte a round each, so a reader joins within a megabyte of when it is due.
		for (rider, read) in &mut riders {
			for _ in 0..64 {
				let Poll::Ready(frame) = rider.poll_read_frame(&waiter) else {
					break;
				};
				let frame = frame.expect("a late reader replays every frame");
				black_box(frame.expect("the live group is open"));
				*read += 1;
			}
		}
		while let Poll::Ready(Ok(fill)) = ctx.requests.poll_requested_fill(&waiter) {
			let start = fill.start();
			let group = fill.accept(start.group, None).unwrap().expect("a fresh group");
			let demand = group.demand();
			served += 1;
			flights.push(Flight {
				group,
				demand,
				next: start.frame as usize,
				ridden: false,
			});
		}
		// Riders that left their flight end it.
		flights.retain_mut(|flight| {
			let wanted = flight.demand.wanted();
			let keep = wanted || !flight.ridden;
			flight.ridden |= wanted;
			keep
		});
		for flight in &mut flights {
			while let Some(payload) = payloads.get(flight.next)
				&& flight.group.poll_ready(&waiter, &frame_info(payload)).is_ready()
			{
				flight
					.group
					.write_frame(moq_net::Timestamp::unscaled(flight.group.start), payload.clone())
					.unwrap();
				flight.next += 1;
				written += 1;
			}
		}
		peak = peak.max(ctx.pool.used());
	}
	(served, written, peak)
}

/// The header admission weighs: only its size.
fn frame_info(payload: &Bytes) -> frame::Info {
	frame::Info {
		size: payload.len() as u64,
		timestamp: moq_net::Timestamp::unscaled(0),
	}
}

/// The frames every `group_flights` call replays, built once.
static FLIGHT_PAYLOADS: LazyLock<Vec<Bytes>> = LazyLock::new(|| {
	(0..FLIGHT_FRAMES as u32)
		.map(|index| Bytes::from(index.to_le_bytes().repeat(4096)))
		.collect()
});

/// One `group_flights` case: late readers, and how far apart they join.
#[derive(Clone, Copy)]
struct Joining {
	readers: usize,
	apart_mib: usize,
}

impl fmt::Display for Joining {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "apart_{}MiB/{}", self.apart_mib, self.readers)
	}
}

fn joining_cases() -> Vec<Joining> {
	// Joining together, 8 MiB apart (within reach), and 24 MiB apart (past it).
	[1, 4, 16]
		.into_iter()
		.flat_map(|readers| [0, 8, 24].map(|apart_mib| Joining { readers, apart_mib }))
		.collect()
}

fn flights(case: Joining) -> (Flights, Joining) {
	(shed_live_group(&FLIGHT_PAYLOADS), case)
}

/// See `group_flights` in the module docs. Throughput counts frames the late readers read,
/// so the flight machinery (boarding, admission, and the handover checks a rider makes) shows
/// up as a slope across readers and how far apart they join.
#[bench(args = joining_cases(), setup = flights, items = |case: Joining| case.readers * FLIGHT_FRAMES)]
fn group_flights((ctx, case): (Flights, Joining)) -> Flights {
	let (served, written, peak) = replay_flights(&ctx, &FLIGHT_PAYLOADS, case.readers, case.apart_mib * 64);
	// Joiners within reach share a flight; flights share the group's
	// content, so the pool holds about one live tail and one flight.
	assert!(served >= 1 && written < case.readers * FLIGHT_FRAMES);
	assert!(peak < 64 << 20, "{} MiB held", peak >> 20);
	ctx
}

fn main() {
	swole::main();
}
