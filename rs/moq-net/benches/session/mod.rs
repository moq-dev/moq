//! End-to-end session benchmarks with no sockets: publishing clients, a mesh of
//! model or production relay nodes, and
//! viewing clients, all connected over the in-memory mock transport.
//!
//! `session_delivery_*` times one round: every watched broadcast writes a group
//! and every viewer reads one group per broadcast it watches. Each group sweeps
//! one or two axes of a [`Shape`] (relays, publisher sessions, broadcasts per
//! publisher, viewers, broadcasts per viewer, frame size) with the rest held
//! fixed, so a cost that grows with a table instead of the touched path shows
//! as a slope. Unwatched broadcasts stay announced and silent: their cost is the
//! route table they occupy, not a publisher writing into its own cache.
//!
//! `SESSION_ALLOCS=1` prints allocations per viewer-group instead of timing.
//! `SESSION_CPU=1` measures thread CPU time on Unix instead of elapsed time, excluding
//! preemption while retaining all protocol work on this current-thread runtime. CPU-timed
//! groups have a `_cpu` suffix. Cache-clock advancement remains outside both clocks.
//! `session_delivery_group_cost` sweeps publishers and viewers independently for SUBSCRIBE
//! and FETCH; its constrained cases release held streams only after a real blocked write.
//! A one-relay FETCH fixture asserts one upstream FETCH per group during setup.
//! `session_delivery_fetch_range` FETCHes missing groups independently, swept over burst length.
//! `session_range_updates` widens established draft-22 subscriptions over a fresh cached
//! burst's largest group, swept over burst length and concurrent requests. Narrowing and
//! writing the next burst happen outside timing; the update and largest-group fill count.
//! `session_delivery_credit` grants a viewer one uni stream at a time, swept over its
//! subscriptions and the backlog each holds, so every group waits for a stream grant. A
//! publisher that parks state per backlogged group pays for it on each grant, which shows
//! as a slope over the backlog.
//! `SESSION_EXPIRY_ONLY=1` uses the standalone 30s-expiry pool and reports groups
//! written plus charged cache bytes every 10s during long profiling runs. Each round
//! advances the cache clock by a second, so the expiry window spans 30 rounds.
//!
//! `session_join_*` times a new viewer connecting, resolving a broadcast, and
//! receiving its latest group, swept over what the relays already announce.
//!
//! `session_dash_*` models the moq.pro dash aggregator: one session following
//! every node's per-project stats broadcast, each announced by a peer relay, at
//! one small frame per track per tick. Throughput counts tracks, so a flat
//! elements/s across a sweep means the cost per track stays constant.
//!
//! `session_idle` times one second of idle viewer sessions, swept over sessions.
//! Sweep held consumers per session too: only held consumers request PROBE or transport estimates.
//! Nothing else happens, so it measures the sessions' own work. Paused time skips waiting.
//!
//! `session_churn_*` times one request opened and closed by a session that already
//! holds others open, swept over sessions and requests held per session, so a slope
//! here is per-request bookkeeping growing with the sessions or the requests.
//!
//! Everything runs on one current-thread runtime, so the only work measured is
//! protocol and relay work, never scheduling across threads.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench session`.

#![allow(
	dead_code,
	reason = "the model and relay entry points use different scenario subsets"
)]
#![allow(
	clippy::disallowed_types,
	reason = "benchmarks time work independently of the protocol clock"
)]
#![expect(
	clippy::disallowed_methods,
	reason = "custom timings measure the real time the work took, not the ambient clock"
)]

#[path = "../../tests/integration/support/harness.rs"]
mod harness;
#[path = "../../tests/integration/support/mock.rs"]
pub(crate) mod mock;

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use criterion::{BenchmarkId, Criterion, Throughput};
pub(crate) use harness::MockConnectOptions;
use harness::{MockPair, connect_mock};
use mock::{MockSession, MockStats};
use moq_net::{Hop, Timestamp, Version, broadcast, cache, group, origin, track};

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> track::Info {
	track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

/// The lite draft production negotiates, the next one (opt-in), and the newest
/// IETF draft.
const VERSIONS: [&str; 3] = ["moq-lite-06", "moq-lite-07-wip", "moq-transport-22"];

/// Frames per group, so per-frame and per-group costs both appear.
const FRAMES: usize = 4;

const TRACK: &str = "video";

/// Lite 06 is what the dash session and cluster peers negotiate today.
const DASH_VERSIONS: [&str; 2] = ["moq-lite-06", "moq-transport-22"];

/// A stats frame: a small JSON snapshot.
const DASH_FRAME: usize = 128;

/// Each endpoint gets its own bounded pool, as a relay configures one per
/// process: without a byte target, every hop keeps each group for the whole
/// expiry window and memory grows with the run length instead of the shape.
const RELAY_CACHE: u64 = 16 * 1024 * 1024;
const CLIENT_CACHE: u64 = 1024 * 1024;

/// One topology: publishers and viewers spread round-robin over a full mesh of
/// relays.
#[derive(Clone, Copy)]
struct Shape {
	/// Relays, each peered with every other.
	relays: usize,
	/// Publisher sessions.
	publishers: usize,
	/// Broadcasts each publisher session announces.
	broadcasts: usize,
	/// Viewer sessions.
	viewers: usize,
	/// Broadcasts each viewer subscribes to.
	watch: usize,
	/// Payload bytes per frame.
	frame: usize,
	/// Frames in a complete group.
	group_frames: usize,
	/// Write one frame per round, as a live source does, instead of a whole group.
	paced: bool,
	/// Whole groups each watched broadcast writes per round, so a serve loop finds a
	/// backlog ready: the group rate a round stands in for.
	burst: usize,
	/// Cached groups per track before a join, whose older groups the live budget skips.
	history: usize,
	/// Bytes queued before a mock stream applies backpressure.
	window: usize,
	/// Group streams a viewer's relay may have open toward it at once.
	credit: Option<usize>,
	/// Publish compressed groups and exercise negotiated versus plain fallback egress.
	compress: bool,
	/// Read each group via a fresh FETCH rather than a live subscription.
	fetch: bool,
}

impl Shape {
	/// A 16-publisher, 16-viewer room on one relay, each viewer watching one
	/// broadcast with small frames, so sweeps measure per-message costs.
	const BASE: Self = Self {
		relays: 1,
		publishers: 16,
		broadcasts: 1,
		viewers: 16,
		watch: 1,
		frame: 64,
		group_frames: FRAMES,
		paced: false,
		burst: 1,
		history: 1,
		window: 1024 * 1024,
		credit: None,
		fetch: false,
		compress: false,
	};

	/// What each viewer subscribes with: a burst is a backlog, which the default
	/// subscription writes off as late.
	fn subscription(&self) -> Option<track::Subscription> {
		(self.burst > 1).then(|| track::Subscription::default().with_max_delay(Duration::from_secs(30)))
	}

	fn total(&self) -> usize {
		self.publishers * self.broadcasts
	}

	/// Frames each group gets per round.
	fn frames(&self) -> usize {
		if self.paced { 1 } else { self.group_frames }
	}

	/// Payload bytes every viewer reads in one round.
	fn expected(&self) -> usize {
		self.viewers * self.watch * self.burst * self.frames() * self.frame
	}

	fn id(&self, version: &str) -> String {
		// The credit sweep names only what it varies: the full form passes Criterion's
		// 100-character title limit, which truncates and renumbers it.
		if let Some(credit) = self.credit {
			return format!("{version}/watch={}/burst={}/credit={credit}", self.watch, self.burst);
		}
		if self.fetch && self.burst > 1 {
			return format!(
				"{version}/publishers={}/viewers={}/burst={}/fetch",
				self.publishers, self.viewers, self.burst
			);
		}
		format!(
			"{version}/relays={}/publishers={}/broadcasts={}/viewers={}/watch={}/frame={}{}{}{}{}{}{}",
			self.relays,
			self.publishers,
			self.broadcasts,
			self.viewers,
			self.watch,
			self.frame,
			if self.group_frames != FRAMES {
				format!("/frames={}", self.group_frames)
			} else {
				String::new()
			},
			if self.paced {
				"/paced".to_string()
			} else {
				String::new()
			},
			if self.burst > 1 {
				format!("/burst={}", self.burst)
			} else {
				String::new()
			},
			if self.window < 1024 * 1024 {
				format!("/window={}", self.window)
			} else {
				String::new()
			},
			if self.fetch { "/fetch" } else { "" },
			if self.history > 1 {
				format!("/history={}", self.history)
			} else {
				String::new()
			},
		)
	}
}

struct Counter;

static COUNTING: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

#[global_allocator]
static ALLOCATOR: Counter = Counter;

// Counting only. Every call forwards to the system allocator unchanged.
unsafe impl GlobalAlloc for Counter {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		if COUNTING.load(Ordering::Relaxed) {
			ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
		}
		unsafe { System.alloc(layout) }
	}

	unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
		if COUNTING.load(Ordering::Relaxed) {
			ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
		}
		unsafe { System.alloc_zeroed(layout) }
	}

	unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
		if COUNTING.load(Ordering::Relaxed) {
			ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
		}
		unsafe { System.realloc(ptr, layout, new_size) }
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		unsafe { System.dealloc(ptr, layout) }
	}
}

fn runtime() -> tokio::runtime::Runtime {
	tokio::runtime::Builder::new_current_thread()
		.enable_time()
		.build()
		.unwrap()
}

/// One round's clock; the CPU variant excludes scheduling delays from other processes.
enum MeasurementClock {
	Wall(Instant),
	#[cfg(unix)]
	Cpu(Duration),
}

impl MeasurementClock {
	fn start(cpu: bool) -> Self {
		if cpu {
			#[cfg(unix)]
			return Self::Cpu(Self::cpu());
			#[cfg(not(unix))]
			panic!("SESSION_CPU requires Unix thread CPU clocks");
		}
		Self::Wall(Instant::now())
	}

	fn elapsed(self) -> Duration {
		match self {
			Self::Wall(start) => start.elapsed(),
			#[cfg(unix)]
			Self::Cpu(start) => Self::cpu()
				.checked_sub(start)
				.expect("thread CPU clock moved backwards"),
		}
	}

	#[cfg(unix)]
	fn cpu() -> Duration {
		let mut time = libc::timespec { tv_sec: 0, tv_nsec: 0 };
		// SAFETY: `time` is a valid, initialized output pointer for this supported clock.
		let result = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut time) };
		assert_eq!(result, 0, "thread CPU clock: {}", std::io::Error::last_os_error());
		Duration::new(
			u64::try_from(time.tv_sec).unwrap(),
			u32::try_from(time.tv_nsec).unwrap(),
		)
	}
}

fn write_group(track: &track::Producer, payload: &Bytes, frames: usize) -> u64 {
	let mut group = track.append_group().unwrap();
	for _ in 0..frames {
		group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
	}
	group.finish().unwrap();
	group.sequence
}

/// Read one whole group, returning its payload bytes so skipped work can't look
/// like a speedup.
async fn read_group(subscriber: &mut track::Subscriber) -> usize {
	let mut group = subscriber.recv_group().await.unwrap().expect("track ended");
	let mut bytes = 0;
	while let Some(frame) = group.read_frame().await.unwrap() {
		bytes += frame.payload.len();
	}
	bytes
}

fn path(broadcast: usize) -> String {
	format!("room/{broadcast}")
}

/// The relay implementation a shared scenario drives.
pub(crate) trait Backend {
	type Relay: Clone;
	const PREFIX: &'static str;

	fn relay(id: u64, capacity: u64) -> (Self::Relay, cache::Pool);
	fn origin(relay: &Self::Relay) -> &origin::Producer;
	async fn connect(options: MockConnectOptions, relay: &Self::Relay, hidden: bool) -> Pair;
}

/// The model-only relay used by the original session benchmark.
pub(crate) struct Model;

impl Backend for Model {
	type Relay = origin::Producer;
	const PREFIX: &'static str = "";

	fn relay(id: u64, capacity: u64) -> (Self::Relay, cache::Pool) {
		make_origin(id, capacity)
	}

	fn origin(relay: &Self::Relay) -> &origin::Producer {
		relay
	}

	async fn connect(options: MockConnectOptions, _relay: &Self::Relay, _hidden: bool) -> Pair {
		let pair = connect_mock(options).await;
		Pair {
			client: pair.client,
			server_transport: pair.server_transport,
			server: Some(pair.server),
			discovery: pair.client_discovery,
			task: None,
		}
	}
}

/// A client session and the receiving transport whose delivery the scenario counts.
pub(crate) struct Pair {
	pub client: moq_net::Session,
	pub server_transport: MockSession,
	pub server: Option<moq_net::Session>,
	pub discovery: Option<moq_net::announce::Consumer>,
	pub task: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for Pair {
	fn drop(&mut self) {
		if let Some(task) = self.task.take() {
			task.abort();
		}
	}
}

fn make_origin(id: u64, capacity: u64) -> (origin::Producer, cache::Pool) {
	let mut config = origin::Config::new(Hop::new(id).unwrap());
	let pool = cache::Pool::new(if std::env::var_os("SESSION_EXPIRY_ONLY").is_some() {
		cache::Config::default().with_expiry(cache::DEFAULT_EXPIRY)
	} else {
		cache::Config::default().with_capacity(capacity)
	});
	config.pool = pool.clone();
	let (producer, driver) = origin::Producer::new(config);
	harness::spawn(driver);
	(producer, pool)
}

/// Relays and publishers, before any viewer joins. Holds every handle that
/// keeps a session or broadcast alive.
struct Cluster<B: Backend> {
	version: Version,
	window: usize,
	credit: Option<usize>,
	relays: Vec<B::Relay>,
	/// One track per broadcast, indexed like [`path`].
	tracks: Vec<track::Producer>,
	_broadcasts: Vec<origin::Publication>,
	_origins: Vec<origin::Producer>,
	_pairs: Vec<Pair>,
	next_hop: u64,
	/// The relay and publisher caches; each viewer owns its own.
	pools: Vec<cache::Pool>,
}

impl<B: Backend> Cluster<B> {
	async fn new(version: &str, shape: Shape) -> Self {
		let mut this = Self {
			version: version.parse().unwrap(),
			window: shape.window,
			credit: shape.credit,
			relays: Vec::new(),
			tracks: Vec::new(),
			_broadcasts: Vec::new(),
			_origins: Vec::new(),
			_pairs: Vec::new(),
			next_hop: 0,
			pools: Vec::new(),
		};

		for _ in 0..shape.relays {
			this.next_hop += 1;
			let (relay, pool) = B::relay(this.next_hop, RELAY_CACHE);
			this.relays.push(relay);
			this.pools.push(pool);
		}

		// Peer every pair the way a cluster dial does: each side publishes its
		// whole origin, hidden broadcasts included, and subscribes into it.
		for server in 0..shape.relays {
			for client in 0..server {
				let client = B::origin(&this.relays[client])
					.clone()
					.with_link(moq_net::origin::Link::Peer);
				let target = &this.relays[server];
				let server = B::origin(target).clone().with_link(moq_net::origin::Link::Peer);
				let mut options = MockConnectOptions::new(this.version);
				options.transport.stream_buffer = this.window;
				options.client_publish = Some(client.consume().with_hidden(true));
				options.client_subscribe = Some(client);
				options.server_publish = Some(server.consume().with_hidden(true));
				options.server_subscribe = Some(server);
				this._pairs.push(B::connect(options, target, false).await);
			}
		}

		for publisher in 0..shape.publishers {
			let (origin, pool) = this.origin(CLIENT_CACHE);
			this.pools.push(pool);
			for index in 0..shape.broadcasts {
				let broadcast = origin
					.publish(path(publisher * shape.broadcasts + index), Default::default())
					.unwrap();
				this.tracks.push(
					broadcast
						.create_track(TRACK, timed().with_compress(shape.compress))
						.unwrap(),
				);
				this._broadcasts.push(broadcast);
			}

			let mut options = MockConnectOptions::new(this.version);
			options.transport.stream_buffer = this.window;
			options.client_publish = Some(origin.consume());
			let target = &this.relays[publisher % shape.relays];
			options.server_subscribe = Some(B::origin(target).clone());
			this._pairs.push(B::connect(options, target, false).await);
			this._origins.push(origin);
		}

		this
	}

	fn origin(&mut self, capacity: u64) -> (origin::Producer, cache::Pool) {
		self.next_hop += 1;
		make_origin(self.next_hop, capacity)
	}

	/// Connect a viewer session to `relay`.
	async fn join(&mut self, relay: usize, hidden: bool) -> Viewer {
		let (origin, pool) = self.origin(CLIENT_CACHE);
		let mut options = MockConnectOptions::new(self.version);
		options.transport.stream_buffer = self.window;
		// Draft 22, the one IETF version here, keeps its SETUP stream open beside the groups.
		options.transport.max_concurrent_uni_streams =
			self.credit.map(|credit| credit + usize::from(!self.version.is_lite()));
		// Hidden included, as the dash's `.stats/` scope sees its subtree.
		options.server_publish = Some(B::origin(&self.relays[relay]).consume().with_hidden(true));
		options.client_subscribe = Some(origin.clone());
		let pair = B::connect(options, &self.relays[relay], hidden).await;

		Viewer {
			subscribers: Vec::new(),
			fetches: Vec::new(),
			blocked: 0,
			_pair: pair,
			origin,
			pool,
		}
	}
}

struct Viewer {
	subscribers: Vec<track::Subscriber>,
	fetches: Vec<(usize, track::Consumer)>,
	blocked: usize,
	_pair: Pair,
	origin: origin::Producer,
	pool: cache::Pool,
}

impl Viewer {
	/// Resolve `path` and subscribe to each of its `tracks`.
	async fn watch(&mut self, path: &str, tracks: &[impl AsRef<str>], subscription: Option<track::Subscription>) {
		let broadcast = self
			.origin
			.consume()
			.follow(path, None)
			.unwrap()
			.available()
			.await
			.unwrap();
		for track in tracks {
			self.subscribers.push(
				broadcast
					.track(track.as_ref())
					.unwrap()
					.subscribe(subscription.clone())
					.await
					.unwrap(),
			);
		}
	}
}

/// A cluster with its viewers attached.
struct Room<B: Backend> {
	cluster: Cluster<B>,
	viewers: Vec<Viewer>,
	/// Broadcasts at least one viewer watches; only these write each round.
	watched: Vec<usize>,
	payload: Bytes,
	shape: Shape,
	/// Frames written so far, so paced rounds know where each group starts and ends.
	written: usize,
	/// Groups finished so far across every watched broadcast.
	groups: usize,
	/// The group each watched broadcast is writing, when paced.
	writing: Vec<Option<group::Producer>>,
	/// The group each viewer's subscriber is reading, when paced, flattened in viewer order.
	reading: Vec<Option<group::Consumer>>,
	cache_now: moq_net::time::Instant,
	cpu: bool,
}

impl<B: Backend> Room<B> {
	async fn new(version: &str, shape: Shape) -> Self {
		assert!(
			shape.watch <= shape.total(),
			"a viewer can't watch more broadcasts than exist"
		);
		assert!(!shape.paced || shape.burst == 1, "a paced round writes one frame");
		assert!(!shape.fetch || !shape.paced, "fetch rounds read finished groups");
		let mut cluster = Cluster::<B>::new(version, shape).await;

		// Viewer v watches a contiguous window starting at v * watch, so viewers
		// spread over the broadcasts before any doubles up.
		let mut viewers = Vec::new();
		let mut watched = vec![false; shape.total()];
		for viewer in 0..shape.viewers {
			let broadcasts: Vec<_> = (0..shape.watch)
				.map(|k| (viewer * shape.watch + k) % shape.total())
				.collect();
			for &broadcast in &broadcasts {
				watched[broadcast] = true;
			}
			// Offset by one so a viewer lands on a different relay than the
			// publisher it watches whenever there is more than one.
			let mut viewer = cluster.join((viewer + 1) % shape.relays, false).await;
			for broadcast in broadcasts {
				if shape.fetch {
					let source = viewer
						.origin
						.consume()
						.follow(path(broadcast), None)
						.unwrap()
						.available()
						.await
						.unwrap();
					viewer.fetches.push((broadcast, source.track(TRACK).unwrap()));
				} else {
					viewer.watch(&path(broadcast), &[TRACK], shape.subscription()).await;
				}
			}
			viewers.push(viewer);
		}

		let mut room = Self {
			cluster,
			viewers,
			watched: watched
				.iter()
				.enumerate()
				.filter(|(_, w)| **w)
				.map(|(i, _)| i)
				.collect(),
			payload: Bytes::from(vec![0; shape.frame]),
			shape,
			written: 0,
			groups: 0,
			writing: Vec::new(),
			reading: Vec::new(),
			cache_now: moq_net::time::Instant::now(),
			cpu: std::env::var_os("SESSION_CPU").is_some(),
		};
		room.writing.resize_with(room.watched.len(), || None);
		room.reading.resize_with(shape.viewers * shape.watch, || None);

		// First measure the complete batch without eviction. The delivery benchmark must
		// fit its working set; cache-pressure loss is a different workload.
		let capacities: Vec<_> = room.pools().map(cache::Pool::capacity).collect();
		for pool in room.pools() {
			pool.resize(None);
		}
		let blocked_before: usize = room
			.viewers
			.iter()
			.map(|viewer| viewer._pair.server_transport.blocked_writes())
			.sum();
		// Warm every path so the timed rounds skip first-group setup.
		for _ in 0..shape.group_frames / shape.frames() {
			room.tick();
			room.round().await;
		}
		for (pool, capacity) in room.pools().zip(capacities) {
			// Keep the old and current batch resident during the handover, including
			// bookkeeping rather than estimating a group from payload bytes alone.
			pool.resize(capacity.map(|capacity| capacity.max(pool.used() * 2)));
		}
		// Exercise turnover as well as the empty cache, including Criterion's one-pass
		// nightly smoke mode.
		for _ in 0..4 {
			room.tick();
			room.round().await;
		}
		if shape.window < shape.frame * shape.group_frames {
			let blocked: usize = room
				.viewers
				.iter()
				.map(|viewer| viewer._pair.server_transport.blocked_writes())
				.sum();
			assert!(
				blocked > blocked_before,
				"the constrained fixture must exercise flow control"
			);
		}
		if shape.fetch && shape.relays == 1 {
			// Both FETCH types encode in one byte and every request names one group.
			let kind = if room.cluster.version.is_lite() { 3 } else { 0x16 };
			let upstream_fetches = room
				.cluster
				._pairs
				.iter()
				.map(|pair| {
					pair.server_transport
						.bidi_types()
						.into_iter()
						.filter(|found| *found == kind)
						.count()
				})
				.sum::<usize>();
			assert_eq!(upstream_fetches, room.groups, "one upstream FETCH per group");
		}
		room
	}

	fn pools(&self) -> impl Iterator<Item = &cache::Pool> {
		let viewers = self.viewers.iter().map(|viewer| &viewer.pool);
		self.cluster.pools.iter().chain(viewers)
	}

	/// Unpaced rounds otherwise collapse many complete batches into one cache tick,
	/// where yesterday's read outranks today's unread write. Sample a new recency
	/// epoch per round without sleeping or changing protocol time.
	fn tick(&mut self) {
		self.cache_now += Duration::from_secs(1);
		for pool in self.pools() {
			pool.poll(self.cache_now, &kio::Waiter::noop());
		}
	}

	async fn round(&mut self) {
		if self.shape.paced {
			return self.paced_round().await;
		}
		let constrained = self.shape.window < self.shape.frame * self.shape.group_frames;
		if constrained {
			for viewer in &mut self.viewers {
				viewer.blocked = viewer._pair.server_transport.blocked_writes();
				viewer._pair.server_transport.split_unis();
				if self.cluster.version.is_lite() {
					viewer._pair.server_transport.split_responses();
				}
			}
		}
		let mut newest = vec![0; self.cluster.tracks.len()];
		for &broadcast in &self.watched {
			for _ in 0..self.shape.burst {
				newest[broadcast] =
					write_group(&self.cluster.tracks[broadcast], &self.payload, self.shape.group_frames);
			}
		}
		let mut bytes = 0;
		// A fetching viewer asks for each group independently; a subscription reads them
		// round-robin, so no subscription's backlog waits on another's being read.
		let passes = if self.shape.fetch { 1 } else { self.shape.burst };
		let burst = self.shape.burst as u64;
		for _ in 0..passes {
			for viewer in &mut self.viewers {
				let transport = &viewer._pair.server_transport;
				let read = async {
					let mut bytes = 0;
					for (broadcast, track) in &viewer.fetches {
						let newest = newest[*broadcast];
						for sequence in newest + 1 - burst..=newest {
							let mut group = track.fetch_group(sequence, None).await.unwrap();
							while let Some(frame) = group.read_frame().await.unwrap() {
								bytes += frame.payload.len();
							}
						}
					}
					for subscriber in &mut viewer.subscribers {
						bytes += read_group(subscriber).await;
					}
					bytes
				};
				bytes += if constrained {
					let release = async {
						transport.blocked_after(viewer.blocked).await;
						transport.release_split();
					};
					futures::join!(read, release).0
				} else {
					read.await
				};
			}
		}
		assert_eq!(bytes, self.shape.expected());
		self.groups += self.shape.burst * self.watched.len();
	}

	/// One frame into every watched group and one frame out to every viewer, so each
	/// hop serves a group frame by frame across its configured frame count.
	async fn paced_round(&mut self) {
		let last = self.written % self.shape.group_frames == self.shape.group_frames - 1;
		self.written += 1;

		for (writing, &broadcast) in self.writing.iter_mut().zip(&self.watched) {
			let group = writing.get_or_insert_with(|| self.cluster.tracks[broadcast].append_group().unwrap());
			group.write_frame(Timestamp::ZERO, self.payload.clone()).unwrap();
			if last {
				group.finish().unwrap();
				*writing = None;
			}
		}
		if last {
			self.groups += self.watched.len();
		}

		let mut bytes = 0;
		let subscribers = self.viewers.iter_mut().flat_map(|viewer| &mut viewer.subscribers);
		for (subscriber, reading) in subscribers.zip(&mut self.reading) {
			if reading.is_none() {
				*reading = Some(subscriber.recv_group().await.unwrap().expect("track ended"));
			}
			let frame = reading.as_mut().unwrap().read_frame().await.unwrap();
			bytes += frame.expect("group ended early").payload.len();
			if last {
				let end = reading.take().unwrap().read_frame().await.unwrap();
				assert!(end.is_none(), "group did not end after its last frame");
			}
		}
		assert_eq!(bytes, self.shape.expected());
	}

	/// Time `iters` rounds, excluding each round's cache tick: its cost grows with the
	/// number of pools, not with the delivery being measured.
	async fn measure(&mut self, iters: u64) -> Duration {
		let mut elapsed = Duration::ZERO;
		let report = std::env::var_os("SESSION_EXPIRY_ONLY").is_some();
		let began = Instant::now();
		let mut next_report = Duration::from_secs(10);
		for _ in 0..iters {
			self.tick();
			let start = MeasurementClock::start(self.cpu);
			self.round().await;
			elapsed += start.elapsed();
			if report && began.elapsed() >= next_report {
				eprintln!(
					"cache sample: elapsed={:.1}s groups={} charged={}",
					began.elapsed().as_secs_f64(),
					self.groups,
					self.pools().map(cache::Pool::used).sum::<u64>()
				);
				next_report += Duration::from_secs(10);
			}
		}
		elapsed
	}
}

fn delivery<B: Backend>(c: &mut Criterion, name: &str, shapes: impl IntoIterator<Item = Shape>) {
	let rt = runtime();
	let shapes: Vec<_> = shapes.into_iter().collect();
	let cpu = if std::env::var_os("SESSION_CPU").is_some() {
		"_cpu"
	} else {
		""
	};
	let mut group = c.benchmark_group(format!("{}session_delivery_{name}{cpu}", B::PREFIX));
	for version in VERSIONS {
		for shape in &shapes {
			// Built on first call: Criterion only calls a routine its filter
			// selects, and calls it again for every sample.
			let mut room = None;
			group.throughput(Throughput::Bytes(shape.expected() as u64));
			group.bench_function(BenchmarkId::from_parameter(shape.id(version)), |b| {
				let room = room.get_or_insert_with(|| rt.block_on(Room::<B>::new(version, *shape)));
				b.iter_custom(|iters| rt.block_on(room.measure(iters)))
			});
		}
	}
	group.finish();
}

/// Small-group cost over independent publisher/viewer axes, with both egress request paths.
pub(crate) fn group_cost<B: Backend>(c: &mut Criterion) {
	if std::env::var_os("SESSION_ALLOCS").is_some() || std::env::var_os("SESSION_EXPIRY_ONLY").is_some() {
		return;
	}
	let shapes = [1, 16].into_iter().flat_map(|publishers| {
		[1, 16, 64].into_iter().flat_map(move |viewers| {
			[64, 1024 * 1024].into_iter().flat_map(move |window| {
				[false, true].map(move |fetch| Shape {
					publishers,
					viewers,
					window,
					fetch,
					..Shape::BASE
				})
			})
		})
	});
	delivery::<B>(c, "group_cost", shapes);
}

/// Cached FETCH write cost across independent publishers, viewers, and group lengths.
pub(crate) fn fetch_frames<B: Backend>(c: &mut Criterion) {
	if std::env::var_os("SESSION_ALLOCS").is_some() || std::env::var_os("SESSION_EXPIRY_ONLY").is_some() {
		return;
	}
	let shapes = [1, 4].into_iter().flat_map(|publishers| {
		[1, 16].into_iter().flat_map(move |viewers| {
			[4, 256].map(move |group_frames| Shape {
				publishers,
				viewers,
				group_frames,
				fetch: true,
				..Shape::BASE
			})
		})
	});
	delivery::<B>(c, "fetch_frames", shapes);
}

/// Independent one-group FETCHes across publishers, viewers, and burst lengths.
pub(crate) fn fetch_range<B: Backend>(c: &mut Criterion) {
	if std::env::var_os("SESSION_ALLOCS").is_some() || std::env::var_os("SESSION_EXPIRY_ONLY").is_some() {
		return;
	}
	let shapes = [1, 16].into_iter().flat_map(|publishers| {
		[1, 16].into_iter().flat_map(move |viewers| {
			[2, 16].map(move |burst| Shape {
				publishers,
				viewers,
				burst,
				fetch: true,
				..Shape::BASE
			})
		})
	});
	delivery::<B>(c, "fetch_range", shapes);
}

/// Largest-group fills, over cached backlog and concurrent requests.
pub(crate) fn range_updates<B: Backend>(c: &mut Criterion) {
	if std::env::var_os("SESSION_ALLOCS").is_some() || std::env::var_os("SESSION_EXPIRY_ONLY").is_some() {
		return;
	}
	let rt = runtime();
	let mut group = c.benchmark_group(format!("{}session_range_updates", B::PREFIX));
	group.sample_size(10);
	for span in [2u64, 16] {
		for requests in [1usize, 16] {
			group.throughput(Throughput::Elements(requests as u64));
			group.bench_function(BenchmarkId::new(format!("groups_{span}"), requests), |b| {
				let (mut cluster, mut viewers) = rt.block_on(async {
					let mut cluster = Cluster::<B>::new(
						"moq-transport-22",
						Shape {
							publishers: 1,
							..Shape::BASE
						},
					)
					.await;
					let mut viewers = Vec::new();
					for _ in 0..requests {
						let mut viewer = cluster.join(0, false).await;
						viewer
							.watch(
								&path(0),
								&[TRACK],
								Some(track::Subscription::default().with_range(span..).with_max_delay(None)),
							)
							.await;
						viewers.push(viewer);
					}
					(cluster, viewers)
				});
				let payload = Bytes::from_static(&[0; 64]);
				let mut next = 0;
				b.iter_custom(|iterations| {
					rt.block_on(async {
						let mut elapsed = Duration::ZERO;
						for _ in 0..iterations {
							let future = next + span;
							for viewer in &mut viewers {
								viewer.subscribers[0]
									.update(track::Subscription::default().with_range(future..).with_max_delay(None))
									.unwrap();
							}
							kio::wait(|waiter| {
								for viewer in &mut viewers {
									assert!(viewer.subscribers[0].poll_recv_group(waiter).is_pending());
								}
								loop {
									if cluster.tracks[0].subscription().is_some_and(|subscription| {
										subscription.ranges == [track::Range::groups(future..)]
									}) {
										return std::task::Poll::Ready(());
									}
									if cluster.tracks[0].poll_subscription_changed(waiter).is_pending() {
										return std::task::Poll::Pending;
									}
								}
							})
							.await;
							for sequence in next..future {
								assert_eq!(write_group(&cluster.tracks[0], &payload, 1), sequence);
							}
							let started = Instant::now();
							for viewer in &mut viewers {
								viewer.subscribers[0]
									.update(
										track::Subscription::default()
											.with_range(future - 1..)
											.with_max_delay(None),
									)
									.unwrap();
							}
							for _ in 0..1 {
								for viewer in &mut viewers {
									assert_eq!(read_group(&mut viewer.subscribers[0]).await, payload.len());
								}
							}
							elapsed += started.elapsed();
							next = future;
						}
						elapsed
					})
				});
			});
		}
	}
	group.finish();
}

fn join<B: Backend>(c: &mut Criterion, name: &str, shapes: impl IntoIterator<Item = Shape>) {
	let rt = runtime();
	let shapes: Vec<_> = shapes.into_iter().collect();
	let mut group = c.benchmark_group(format!("{}session_join_{name}", B::PREFIX));
	for version in VERSIONS {
		for shape in &shapes {
			let mut cluster = None;
			group.bench_function(BenchmarkId::from_parameter(shape.id(version)), |b| {
				let cluster = cluster.get_or_insert_with(|| {
					rt.block_on(async {
						let cluster = Cluster::<B>::new(version, *shape).await;
						let payload = Bytes::from(vec![0; shape.frame]);
						for track in &cluster.tracks {
							for sequence in 0..shape.history {
								let mut group = track.append_group().unwrap();
								for _ in 0..shape.group_frames {
									group
										.write_frame(Timestamp::from_millis(sequence as u64).unwrap(), payload.clone())
										.unwrap();
								}
								group.finish().unwrap();
							}
						}
						cluster
					})
				});
				// Broadcasts published to relay 0, joined from the last relay: every
				// sample is local on one relay and crosses one peer hop on a mesh.
				let hosted: Vec<_> = (0..shape.total())
					.filter(|broadcast| (broadcast / shape.broadcasts) % shape.relays == 0)
					.collect();
				let relay = shape.relays - 1;
				b.iter_custom(|iters| {
					rt.block_on(async {
						let mut elapsed = Duration::ZERO;
						for iter in 0..iters as usize {
							let broadcast = hosted[iter % hosted.len()];
							let start = Instant::now();
							let mut viewer = cluster.join(relay, false).await;
							viewer.watch(&path(broadcast), &[TRACK], None).await;
							let bytes = read_group(&mut viewer.subscribers[0]).await;
							elapsed += start.elapsed();
							assert_eq!(bytes, shape.group_frames * shape.frame);
							// Teardown is a leaving viewer's cost, not a joining one's.
							drop(viewer);
							tokio::task::yield_now().await;
						}
						elapsed
					})
				})
			});
		}
	}
	group.finish();
}

/// The moq.pro dash aggregator's load: one session on relay 0 following every
/// node's per-project stats broadcast. Every other relay is a node publishing
/// `.stats/<project>/node/<node>` into its own origin, as `moq-relay`'s stats
/// producer does, so relay 0 reaches each one over a peer session.
#[derive(Clone, Copy)]
struct Dash {
	/// Relays in the mesh: relay 0 hosts the dash session, the rest are nodes.
	relays: usize,
	/// Projects with a stats broadcast on every node.
	projects: usize,
	/// Tracks per stats broadcast.
	tracks: usize,
}

impl Dash {
	fn total(&self) -> usize {
		(self.relays - 1) * self.projects * self.tracks
	}

	fn id(&self, version: &str) -> String {
		format!(
			"{version}/relays={}/projects={}/tracks={}/total={}",
			self.relays,
			self.projects,
			self.tracks,
			self.total()
		)
	}
}

/// A mesh of stats-publishing nodes with the dash session attached.
struct DashRoom<B: Backend> {
	_cluster: Cluster<B>,
	tracks: Vec<track::Producer>,
	_broadcasts: Vec<origin::Publication>,
	dash: Viewer,
	payload: Bytes,
}

impl<B: Backend> DashRoom<B> {
	async fn new(version: &str, shape: Dash) -> Self {
		assert!(shape.relays > 1, "the dash needs a peer node");
		let mut cluster = Cluster::<B>::new(
			version,
			Shape {
				relays: shape.relays,
				publishers: 0,
				viewers: 0,
				..Shape::BASE
			},
		)
		.await;

		let names: Vec<_> = (0..shape.tracks).map(|track| format!("stat{track}")).collect();
		let mut tracks = Vec::new();
		let mut broadcasts = Vec::new();
		let mut paths = Vec::new();
		for node in 1..shape.relays {
			for project in 0..shape.projects {
				let path = format!(".stats/{project}/node/{node}");
				let broadcast = B::origin(&cluster.relays[node])
					.publish(path.as_str(), Default::default())
					.unwrap();
				for name in &names {
					tracks.push(broadcast.create_track(name.as_str(), timed()).unwrap());
				}
				broadcasts.push(broadcast);
				paths.push(path);
			}
		}

		let mut dash = cluster.join(0, true).await;
		for path in &paths {
			dash.watch(path, &names, None).await;
		}

		let mut room = Self {
			_cluster: cluster,
			tracks,
			_broadcasts: broadcasts,
			dash,
			payload: Bytes::from(vec![0; DASH_FRAME]),
		};
		room.round().await;
		room
	}

	/// One stats tick: every track writes a single-frame group and the dash
	/// reads it.
	async fn round(&mut self) {
		for track in &self.tracks {
			write_group(track, &self.payload, 1);
		}
		let mut bytes = 0;
		for subscriber in &mut self.dash.subscribers {
			bytes += read_group(subscriber).await;
		}
		assert_eq!(bytes, self.tracks.len() * DASH_FRAME);
	}

	async fn measure(&mut self, iters: u64) -> Duration {
		let start = Instant::now();
		for _ in 0..iters {
			self.round().await;
		}
		start.elapsed()
	}
}

fn dash<B: Backend>(c: &mut Criterion, name: &str, shapes: impl IntoIterator<Item = Dash>) {
	let rt = runtime();
	let shapes: Vec<_> = shapes.into_iter().collect();
	let mut group = c.benchmark_group(format!("{}session_dash_{name}", B::PREFIX));
	group.sample_size(10);
	for version in DASH_VERSIONS {
		for shape in &shapes {
			let mut room = None;
			group.throughput(Throughput::Elements(shape.total() as u64));
			group.bench_function(BenchmarkId::from_parameter(shape.id(version)), |b| {
				let room = room.get_or_insert_with(|| rt.block_on(DashRoom::<B>::new(version, *shape)));
				b.iter_custom(|iters| rt.block_on(room.measure(iters)))
			});
		}
	}
	group.finish();
}

/// Print allocations per viewer-group instead of timing, to compare across revisions.
fn allocations<B: Backend>() {
	const ROUNDS: usize = 256;

	let rt = runtime();
	let fanout = Shape {
		publishers: 1,
		viewers: 256,
		..Shape::BASE
	};
	for version in VERSIONS {
		for shape in [Shape::BASE, fanout] {
			for paced in [false, true] {
				let shape = Shape { paced, ..shape };
				let mut room = rt.block_on(Room::<B>::new(version, shape));
				ALLOCATIONS.store(0, Ordering::Relaxed);
				COUNTING.store(true, Ordering::Relaxed);
				rt.block_on(room.measure(ROUNDS as u64));
				COUNTING.store(false, Ordering::Relaxed);

				let groups = ROUNDS * shape.viewers * shape.watch * shape.frames() / shape.group_frames;
				let allocations = ALLOCATIONS.load(Ordering::Relaxed) as f64 / groups as f64;
				println!("{}: {allocations:.1} allocations per viewer-group", shape.id(version));
			}
		}
	}
}

/// Versions for the churn sweep: the newest moq-transport draft and lite.
const CHURN_VERSIONS: [&str; 2] = ["moq-transport-22", "moq-lite-06"];

/// Sessions on one relay, each holding `held` subscriptions open while it churns one more.
#[derive(Clone, Copy)]
struct Churn {
	sessions: usize,
	held: usize,
}

impl Churn {
	fn id(&self, version: &str) -> String {
		format!("{version}/sessions={}/held={}", self.sessions, self.held)
	}
}

/// One relay publishing a broadcast with `held` shared tracks plus one churn track per session.
struct ChurnRoom<B: Backend> {
	_cluster: Cluster<B>,
	_held: Vec<track::Producer>,
	_viewers: Vec<Viewer>,
	/// Each session's view of the broadcast, to open the churned subscription on.
	broadcasts: Vec<broadcast::Consumer>,
	/// The relay's side of each session's churn track, to wait out its close.
	churn: Vec<track::Producer>,
}

impl<B: Backend> ChurnRoom<B> {
	async fn new(version: &str, shape: Churn) -> Self {
		let mut cluster = Cluster::<B>::new(
			version,
			Shape {
				relays: 1,
				publishers: 1,
				viewers: 0,
				..Shape::BASE
			},
		)
		.await;
		let published = &cluster._broadcasts[0];
		let held: Vec<_> = (0..shape.held).map(|i| format!("held{i}")).collect();
		let tracks: Vec<_> = held
			.iter()
			.map(|name| published.create_track(name.as_str(), None).unwrap())
			.collect();
		let churn: Vec<_> = (0..shape.sessions)
			.map(|i| published.create_track(format!("churn{i}"), None).unwrap())
			.collect();

		let mut viewers = Vec::new();
		let mut broadcasts = Vec::new();
		for _ in 0..shape.sessions {
			let mut viewer = cluster.join(0, false).await;
			viewer.watch(&path(0), &held, None).await;
			broadcasts.push(
				viewer
					.origin
					.consume()
					.follow(path(0), None)
					.unwrap()
					.available()
					.await
					.unwrap(),
			);
			viewers.push(viewer);
		}
		Self {
			_cluster: cluster,
			_held: tracks,
			_viewers: viewers,
			broadcasts,
			churn,
		}
	}

	/// Open and close one subscription per iteration, round-robin over the sessions.
	async fn measure(&mut self, iters: u64) -> Duration {
		let start = Instant::now();
		for iter in 0..iters as usize {
			let session = iter % self.broadcasts.len();
			let track = self.broadcasts[session].track(&format!("churn{session}")).unwrap();
			let mut subscriber = track.subscribe(None).await.unwrap();
			// Keep the reader polled while its per-route subscription resolves. No
			// groups are produced here, so this starts demand without consuming data.
			kio::wait(|waiter| {
				assert!(subscriber.poll_recv_group(waiter).is_pending());
				while std::task::ready!(self.churn[session].poll_subscription_changed(waiter))
					.unwrap()
					.is_none()
				{}
				std::task::Poll::Ready(())
			})
			.await;
			drop(subscriber);
			drop(track);
			let mut demand = self.churn[session].demand();
			while demand.wanted_changed().await.unwrap().is_some() {}
		}
		start.elapsed()
	}
}

fn churn<B: Backend>(c: &mut Criterion, name: &str, shapes: impl IntoIterator<Item = Churn>) {
	let rt = runtime();
	let shapes: Vec<_> = shapes.into_iter().collect();
	let mut group = c.benchmark_group(format!("{}session_churn_{name}", B::PREFIX));
	group.throughput(Throughput::Elements(1));
	for version in CHURN_VERSIONS {
		for shape in &shapes {
			let mut room = None;
			group.bench_function(BenchmarkId::from_parameter(shape.id(version)), |b| {
				let room = room.get_or_insert_with(|| rt.block_on(ChurnRoom::<B>::new(version, *shape)));
				b.iter_custom(|iters| rt.block_on(room.measure(iters)))
			});
		}
	}
	group.finish();
}

/// Idle sessions to one relay, holding PROBE or their local transport estimate.
struct IdleRoom {
	_viewers: Vec<(MockPair, Vec<moq_net::bandwidth::Consumer>)>,
}

impl IdleRoom {
	async fn new(version: &str, sessions: usize, consumers: usize) -> Self {
		let (relay, driver) = origin::Producer::new(origin::Config::new(Hop::new(1).unwrap()));
		harness::spawn(driver);
		let mut viewers = Vec::new();
		for _ in 0..sessions {
			let mut options = MockConnectOptions::new(version.parse().unwrap());
			options.server_publish = Some(relay.consume());
			// A QUIC backend reports both; Lite serves PROBE, IETF samples on demand.
			options.stats = MockStats {
				estimated_send_rate: Some(1_000_000),
				rtt: Some(Duration::from_millis(40)),
			};
			let pair = connect_mock(options).await;
			let mut probes = Vec::new();
			for _ in 0..consumers {
				let mut probe = if version.parse::<Version>().unwrap().is_lite() {
					pair.client.recv_bandwidth().unwrap()
				} else {
					pair.server.send_bandwidth().unwrap()
				};
				probe.changed().await.unwrap();
				probes.push(probe);
			}
			viewers.push((pair, probes));
		}
		Self { _viewers: viewers }
	}

	/// Let a second of protocol time pass per iteration.
	async fn measure(&self, iters: u64) -> Duration {
		let start = Instant::now();
		for _ in 0..iters {
			moq_clock::sleep(Duration::from_secs(1)).await;
		}
		start.elapsed()
	}
}

pub(crate) fn idle(c: &mut Criterion) {
	let mut group = c.benchmark_group("session_idle");
	for version in ["moq-lite-06", "moq-transport-22"] {
		for sessions in [1, 64, 1024] {
			for consumers in [0, 1, 8] {
				group.throughput(Throughput::Elements(sessions as u64));
				// Its own runtime, so its paused clock and sessions end with the shape.
				let rt = runtime();
				let mut room = None;
				group.bench_function(format!("{version}/sessions={sessions}/consumers={consumers}"), |b| {
					let room = room.get_or_insert_with(|| {
						rt.block_on(async {
							tokio::time::pause();
							IdleRoom::new(version, sessions, consumers).await
						})
					});
					b.iter_custom(|iters| rt.block_on(room.measure(iters)))
				});
			}
		}
	}
	group.finish();
}

/// Withdraw a batch across a full mesh, sweeping both peers and prefixes.
/// Paused protocol time lets the quiet-point timeout drain pending work without
/// including a real wait in the measured duration.
pub(crate) fn withdrawal<B: Backend>(c: &mut Criterion) {
	let mut group = c.benchmark_group(format!("{}session_withdrawal", B::PREFIX));
	group.sample_size(10);
	for relays in [4, 8, 16] {
		for broadcasts in [1, 32] {
			group.bench_function(format!("{relays}r_{broadcasts}b"), |b| {
				b.iter_custom(|iters| {
					let mut elapsed = Duration::ZERO;
					for _ in 0..iters {
						let rt = runtime();
						elapsed += rt.block_on(async {
							tokio::time::pause();
							let shape = Shape {
								relays,
								publishers: 0,
								..Shape::BASE
							};
							let cluster = Cluster::<B>::new("moq-lite-06", shape).await;
							let mut announced = B::origin(cluster.relays.last().unwrap()).consume().announced();
							let live: Vec<_> = (0..broadcasts)
								.map(|i| {
									B::origin(&cluster.relays[i % (relays - 1)])
										.publish(path(i), Default::default())
										.unwrap()
								})
								.collect();
							while moq_clock::timeout(Duration::from_secs(1), announced.next())
								.await
								.is_ok()
							{}
							let start = Instant::now();
							drop(live);
							let mut retracted = 0;
							while let Ok(Some(event)) =
								moq_clock::timeout(Duration::from_secs(1), announced.next()).await
							{
								assert!(matches!(event, moq_net::announce::Event::End(_)), "{event:?}");
								retracted += 1;
							}
							let elapsed = start.elapsed();
							assert_eq!(retracted, broadcasts);
							elapsed
						});
					}
					elapsed
				});
			});
		}
	}
	group.finish();
}

pub(crate) fn session<B: Backend>(c: &mut Criterion) {
	if std::env::var_os("SESSION_ALLOCS").is_some() {
		allocations::<B>();
		return;
	}
	let base = Shape::BASE;

	delivery::<B>(
		c,
		"compression",
		[1, 16].into_iter().flat_map(|publishers| {
			[1, 16].map(move |viewers| Shape {
				publishers,
				viewers,
				compress: true,
				frame: 1024,
				..base
			})
		}),
	);

	delivery::<B>(
		c,
		"publishers",
		[1, 16, 256].map(|publishers| Shape { publishers, ..base }),
	);
	delivery::<B>(c, "viewers", [1, 16, 256].map(|viewers| Shape { viewers, ..base }));
	// Live media: every hop serves each group frame by frame.
	delivery::<B>(
		c,
		"paced",
		[1, 16, 256].map(|viewers| Shape {
			viewers,
			paced: true,
			..base
		}),
	);
	delivery::<B>(
		c,
		"scale",
		[16, 64, 256].map(|n| Shape {
			publishers: n,
			viewers: n,
			..base
		}),
	);
	// One publisher session announcing many broadcasts, most of them unwatched.
	delivery::<B>(
		c,
		"broadcasts",
		[16, 256, 4096].map(|broadcasts| Shape {
			publishers: 1,
			broadcasts,
			..base
		}),
	);
	// Each viewer watches many broadcasts over one session, like a conference.
	delivery::<B>(
		c,
		"watch",
		[1, 16, 256].map(|watch| Shape {
			publishers: 1,
			broadcasts: 256,
			watch,
			..base
		}),
	);
	delivery::<B>(c, "frame", [64, 1024, 16 * 1024].map(|frame| Shape { frame, ..base }));
	// One session serving a backlog on each of its subscriptions: the serve loop that a
	// `kio::coop::Budget` bounds per poll.
	delivery::<B>(
		c,
		"burst",
		[(1, 1), (1, 8), (1, 64), (8, 1), (8, 8), (16, 16), (16, 64)].map(|(watch, burst)| Shape {
			publishers: 1,
			broadcasts: 16,
			viewers: 1,
			watch,
			burst,
			..base
		}),
	);
	// The same backlog behind one uni stream grant at a time.
	delivery::<B>(
		c,
		"credit",
		[1, 16, 64].into_iter().flat_map(|watch| {
			[1, 16, 64].map(move |burst| Shape {
				publishers: 1,
				broadcasts: 64,
				viewers: 1,
				watch,
				burst,
				credit: Some(1),
				..base
			})
		}),
	);
	delivery::<B>(
		c,
		"relays",
		[1, 2, 4, 8].map(|relays| Shape {
			relays,
			viewers: 64,
			..base
		}),
	);

	join::<B>(
		c,
		"broadcasts",
		[1, 2, 8].into_iter().flat_map(|relays| {
			[1, 64, 1024].map(move |broadcasts| Shape {
				publishers: 1,
				broadcasts,
				relays,
				..base
			})
		}),
	);
	// A live join skips a cached prefix. Sweep publishers against that prefix so
	// resolving one start cannot hide a scan of unrelated publishers or group IDs.
	join::<B>(
		c,
		"stale",
		[1, 8, 32].into_iter().flat_map(|publishers| {
			[1, 64, 1024].map(move |history| Shape {
				publishers,
				history,
				..base
			})
		}),
	);

	join::<B>(c, "relays", [1, 2, 8].map(|relays| Shape { relays, ..base }));

	churn::<B>(c, "sessions", [1, 16, 128].map(|sessions| Churn { sessions, held: 16 }));
	churn::<B>(
		c,
		"held",
		[1, 4, 16]
			.into_iter()
			.flat_map(|sessions| [1, 64, 1024].map(move |held| Churn { sessions, held })),
	);

	// Production (2026-09): ~34 nodes x >=5 projects x ~24 tracks.
	let prod = Dash {
		relays: 35,
		projects: 5,
		tracks: 24,
	};
	// More broadcasts at a fixed track count per broadcast.
	dash::<B>(
		c,
		"projects",
		[1, 4, 16].map(|projects| Dash {
			relays: 5,
			projects,
			..prod
		}),
	);
	// More tracks per broadcast at a fixed broadcast count.
	dash::<B>(
		c,
		"tracks",
		[6, 24, 96].map(|tracks| Dash {
			relays: 5,
			projects: 4,
			tracks,
		}),
	);
	// More peer sessions carrying the same 32 broadcasts, then the production
	// shape.
	dash::<B>(
		c,
		"relays",
		[2, 3, 5, 9, 17, 33]
			.map(|relays| Dash {
				relays,
				projects: 32 / (relays - 1),
				..prod
			})
			.into_iter()
			.chain([prod]),
	);
}
