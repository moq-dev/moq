//! End-to-end session benchmarks with no sockets: publishing clients, a mesh of
//! relays that forward through their origins the way `moq-relay` does, and
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
//!
//! `session_join_*` times a new viewer connecting, resolving a broadcast, and
//! receiving its latest group, swept over what the relays already announce.
//!
//! `session_dash_*` models the moq.pro dash aggregator: one session following
//! every node's per-project stats broadcast, each announced by a peer relay, at
//! one small frame per track per tick. Throughput counts tracks, so a flat
//! elements/s across a sweep means the cost per track stays constant.
//!
//! Everything runs on one current-thread runtime, so the only work measured is
//! the protocol and model code, never scheduling across threads.
//!
//! Run with `cargo bench -p moq-net --bench session`.

#[path = "../tests/support/mod.rs"]
mod support;

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_net::{Hop, Timestamp, Version, broadcast, cache, group, origin, track};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

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
	/// Write one frame per round, as a live source does, instead of a whole group.
	paced: bool,
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
		paced: false,
	};

	fn total(&self) -> usize {
		self.publishers * self.broadcasts
	}

	/// Frames each group gets per round.
	fn frames(&self) -> usize {
		if self.paced { 1 } else { FRAMES }
	}

	/// Payload bytes every viewer reads in one round.
	fn expected(&self) -> usize {
		self.viewers * self.watch * self.frames() * self.frame
	}

	fn id(&self, version: &str) -> String {
		format!(
			"{version}/relays={}/publishers={}/broadcasts={}/viewers={}/watch={}/frame={}{}",
			self.relays,
			self.publishers,
			self.broadcasts,
			self.viewers,
			self.watch,
			self.frame,
			if self.paced { "/paced" } else { "" },
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

fn write_group(track: &track::Producer, payload: &Bytes, frames: usize) {
	let mut group = track.append_group().unwrap();
	for _ in 0..frames {
		group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
	}
	group.finish().unwrap();
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

/// Relays and publishers, before any viewer joins. Holds every handle that
/// keeps a session or broadcast alive.
struct Cluster {
	version: Version,
	relays: Vec<origin::Producer>,
	/// One track per broadcast, indexed like [`path`].
	tracks: Vec<track::Producer>,
	_broadcasts: Vec<broadcast::Producer>,
	_origins: Vec<origin::Producer>,
	_pairs: Vec<MockPair>,
	next_hop: u64,
}

impl Cluster {
	async fn new(version: &str, shape: Shape) -> Self {
		let mut this = Self {
			version: version.parse().unwrap(),
			relays: Vec::new(),
			tracks: Vec::new(),
			_broadcasts: Vec::new(),
			_origins: Vec::new(),
			_pairs: Vec::new(),
			next_hop: 0,
		};

		for _ in 0..shape.relays {
			let relay = this.origin(RELAY_CACHE);
			this.relays.push(relay);
		}

		// Peer every pair the way a cluster dial does: each side publishes its
		// whole origin, hidden broadcasts included, and subscribes into it.
		for server in 0..shape.relays {
			for client in 0..server {
				let client = this.relays[client].clone().peer();
				let server = this.relays[server].clone().peer();
				let mut options = MockConnectOptions::new(this.version);
				options.client_publish = Some(client.consume().with_hidden(true));
				options.client_subscribe = Some(client);
				options.server_publish = Some(server.consume().with_hidden(true));
				options.server_subscribe = Some(server);
				this._pairs.push(connect_mock(options).await);
			}
		}

		for publisher in 0..shape.publishers {
			let origin = this.origin(CLIENT_CACHE);
			for index in 0..shape.broadcasts {
				let broadcast = origin
					.publish(path(publisher * shape.broadcasts + index), Default::default())
					.unwrap();
				this.tracks.push(broadcast.create_track(TRACK, None).unwrap());
				this._broadcasts.push(broadcast);
			}

			let mut options = MockConnectOptions::new(this.version);
			options.client_publish = Some(origin.consume());
			options.server_subscribe = Some(this.relays[publisher % shape.relays].clone());
			this._pairs.push(connect_mock(options).await);
			this._origins.push(origin);
		}

		this
	}

	fn origin(&mut self, capacity: u64) -> origin::Producer {
		self.next_hop += 1;
		let mut config = origin::Config::new(Hop::new(self.next_hop).unwrap());
		config.pool = cache::Pool::new(cache::Config::default().with_capacity(capacity));
		let (producer, driver) = origin::Producer::new(config);
		tokio::spawn(support::harness::run(driver));
		producer
	}

	/// Connect a viewer session to `relay`.
	async fn join(&mut self, relay: usize) -> Viewer {
		let origin = self.origin(CLIENT_CACHE);
		let mut options = MockConnectOptions::new(self.version);
		// Hidden included, as the dash's `.stats/` scope sees its subtree.
		options.server_publish = Some(self.relays[relay].consume().with_hidden(true));
		options.client_subscribe = Some(origin.clone());
		let pair = connect_mock(options).await;

		Viewer {
			subscribers: Vec::new(),
			_pair: pair,
			origin,
		}
	}
}

struct Viewer {
	subscribers: Vec<track::Subscriber>,
	_pair: MockPair,
	origin: origin::Producer,
}

impl Viewer {
	/// Resolve `path` and subscribe to each of its `tracks`.
	async fn watch(&mut self, path: &str, tracks: &[impl AsRef<str>]) {
		let broadcast = self.origin.consume().routed_broadcast(path).await.unwrap();
		for track in tracks {
			self.subscribers
				.push(broadcast.track(track.as_ref()).unwrap().subscribe(None).await.unwrap());
		}
	}
}

/// A cluster with its viewers attached.
struct Room {
	cluster: Cluster,
	viewers: Vec<Viewer>,
	/// Broadcasts at least one viewer watches; only these write each round.
	watched: Vec<usize>,
	payload: Bytes,
	shape: Shape,
	/// Frames written so far, so paced rounds know where each group starts and ends.
	written: usize,
	/// The group each watched broadcast is writing, when paced.
	writing: Vec<Option<group::Producer>>,
	/// The group each viewer's subscriber is reading, when paced, flattened in viewer order.
	reading: Vec<Option<group::Consumer>>,
}

impl Room {
	async fn new(version: &str, shape: Shape) -> Self {
		assert!(
			shape.watch <= shape.total(),
			"a viewer can't watch more broadcasts than exist"
		);
		let mut cluster = Cluster::new(version, shape).await;

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
			let mut viewer = cluster.join((viewer + 1) % shape.relays).await;
			for broadcast in broadcasts {
				viewer.watch(&path(broadcast), &[TRACK]).await;
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
			writing: Vec::new(),
			reading: Vec::new(),
		};
		room.writing.resize_with(room.watched.len(), || None);
		room.reading.resize_with(shape.viewers * shape.watch, || None);
		// Warm every path so the timed rounds skip first-group setup.
		for _ in 0..FRAMES / shape.frames() {
			room.round().await;
		}
		room
	}

	async fn round(&mut self) {
		if self.shape.paced {
			return self.paced_round().await;
		}
		for &broadcast in &self.watched {
			write_group(&self.cluster.tracks[broadcast], &self.payload, FRAMES);
		}
		let mut bytes = 0;
		for viewer in &mut self.viewers {
			for subscriber in &mut viewer.subscribers {
				bytes += read_group(subscriber).await;
			}
		}
		assert_eq!(bytes, self.shape.expected());
	}

	/// One frame into every watched group and one frame out to every viewer, so each
	/// hop serves a group frame by frame across [`FRAMES`] rounds.
	async fn paced_round(&mut self) {
		let last = self.written % FRAMES == FRAMES - 1;
		self.written += 1;

		for (writing, &broadcast) in self.writing.iter_mut().zip(&self.watched) {
			let group = writing.get_or_insert_with(|| self.cluster.tracks[broadcast].append_group().unwrap());
			group.write_frame(Timestamp::ZERO, self.payload.clone()).unwrap();
			if last {
				group.finish().unwrap();
				*writing = None;
			}
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

	async fn measure(&mut self, iters: u64) -> Duration {
		let start = Instant::now();
		for _ in 0..iters {
			self.round().await;
		}
		start.elapsed()
	}
}

fn delivery(c: &mut Criterion, name: &str, shapes: impl IntoIterator<Item = Shape>) {
	let rt = runtime();
	let shapes: Vec<_> = shapes.into_iter().collect();
	let mut group = c.benchmark_group(format!("session_delivery_{name}"));
	for version in VERSIONS {
		for shape in &shapes {
			// Built on first call: Criterion only calls a routine its filter
			// selects, and calls it again for every sample.
			let mut room = None;
			group.throughput(Throughput::Bytes(shape.expected() as u64));
			group.bench_function(BenchmarkId::from_parameter(shape.id(version)), |b| {
				let room = room.get_or_insert_with(|| rt.block_on(Room::new(version, *shape)));
				b.iter_custom(|iters| rt.block_on(room.measure(iters)))
			});
		}
	}
	group.finish();
}

fn join(c: &mut Criterion, name: &str, shapes: impl IntoIterator<Item = Shape>) {
	let rt = runtime();
	let shapes: Vec<_> = shapes.into_iter().collect();
	let mut group = c.benchmark_group(format!("session_join_{name}"));
	for version in VERSIONS {
		for shape in &shapes {
			let mut cluster = None;
			group.bench_function(BenchmarkId::from_parameter(shape.id(version)), |b| {
				let cluster = cluster.get_or_insert_with(|| {
					rt.block_on(async {
						let cluster = Cluster::new(version, *shape).await;
						let payload = Bytes::from(vec![0; shape.frame]);
						for track in &cluster.tracks {
							write_group(track, &payload, FRAMES);
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
							let mut viewer = cluster.join(relay).await;
							viewer.watch(&path(broadcast), &[TRACK]).await;
							let bytes = read_group(&mut viewer.subscribers[0]).await;
							elapsed += start.elapsed();
							assert_eq!(bytes, FRAMES * shape.frame);
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
struct DashRoom {
	_cluster: Cluster,
	tracks: Vec<track::Producer>,
	_broadcasts: Vec<broadcast::Producer>,
	dash: Viewer,
	payload: Bytes,
}

impl DashRoom {
	async fn new(version: &str, shape: Dash) -> Self {
		assert!(shape.relays > 1, "the dash needs a peer node");
		let mut cluster = Cluster::new(
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
				let broadcast = cluster.relays[node].publish(path.as_str(), Default::default()).unwrap();
				for name in &names {
					tracks.push(broadcast.create_track(name.as_str(), None).unwrap());
				}
				broadcasts.push(broadcast);
				paths.push(path);
			}
		}

		let mut dash = cluster.join(0).await;
		for path in &paths {
			dash.watch(path, &names).await;
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

fn dash(c: &mut Criterion, name: &str, shapes: impl IntoIterator<Item = Dash>) {
	let rt = runtime();
	let shapes: Vec<_> = shapes.into_iter().collect();
	let mut group = c.benchmark_group(format!("session_dash_{name}"));
	group.sample_size(10);
	for version in DASH_VERSIONS {
		for shape in &shapes {
			let mut room = None;
			group.throughput(Throughput::Elements(shape.total() as u64));
			group.bench_function(BenchmarkId::from_parameter(shape.id(version)), |b| {
				let room = room.get_or_insert_with(|| rt.block_on(DashRoom::new(version, *shape)));
				b.iter_custom(|iters| rt.block_on(room.measure(iters)))
			});
		}
	}
	group.finish();
}

/// Print allocations per viewer-group instead of timing, to compare across revisions.
fn allocations() {
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
				let mut room = rt.block_on(Room::new(version, shape));
				ALLOCATIONS.store(0, Ordering::Relaxed);
				COUNTING.store(true, Ordering::Relaxed);
				rt.block_on(room.measure(ROUNDS as u64));
				COUNTING.store(false, Ordering::Relaxed);

				let groups = ROUNDS * shape.viewers * shape.watch * shape.frames() / FRAMES;
				let allocations = ALLOCATIONS.load(Ordering::Relaxed) as f64 / groups as f64;
				println!("{}: {allocations:.1} allocations per viewer-group", shape.id(version));
			}
		}
	}
}

fn session(c: &mut Criterion) {
	if std::env::var_os("SESSION_ALLOCS").is_some() {
		allocations();
		return;
	}
	let base = Shape::BASE;

	delivery(
		c,
		"publishers",
		[1, 16, 256].map(|publishers| Shape { publishers, ..base }),
	);
	delivery(c, "viewers", [1, 16, 256].map(|viewers| Shape { viewers, ..base }));
	// Live media: every hop serves each group frame by frame.
	delivery(
		c,
		"paced",
		[1, 16, 256].map(|viewers| Shape {
			viewers,
			paced: true,
			..base
		}),
	);
	delivery(
		c,
		"scale",
		[16, 64, 256].map(|n| Shape {
			publishers: n,
			viewers: n,
			..base
		}),
	);
	// One publisher session announcing many broadcasts, most of them unwatched.
	delivery(
		c,
		"broadcasts",
		[16, 256, 4096].map(|broadcasts| Shape {
			publishers: 1,
			broadcasts,
			..base
		}),
	);
	// Each viewer watches many broadcasts over one session, like a conference.
	delivery(
		c,
		"watch",
		[1, 16, 256].map(|watch| Shape {
			publishers: 1,
			broadcasts: 256,
			watch,
			..base
		}),
	);
	delivery(c, "frame", [64, 1024, 16 * 1024].map(|frame| Shape { frame, ..base }));
	delivery(
		c,
		"relays",
		[1, 2, 4, 8].map(|relays| Shape {
			relays,
			viewers: 64,
			..base
		}),
	);

	join(
		c,
		"broadcasts",
		[1, 64, 1024].map(|broadcasts| Shape {
			publishers: 1,
			broadcasts,
			..base
		}),
	);
	join(c, "relays", [1, 2, 8].map(|relays| Shape { relays, ..base }));

	// Production (2026-09): ~34 nodes x >=5 projects x ~24 tracks.
	let prod = Dash {
		relays: 35,
		projects: 5,
		tracks: 24,
	};
	// More broadcasts at a fixed track count per broadcast.
	dash(
		c,
		"projects",
		[1, 4, 16].map(|projects| Dash {
			relays: 5,
			projects,
			..prod
		}),
	);
	// More tracks per broadcast at a fixed broadcast count.
	dash(
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
	dash(
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

criterion_group!(benches, session);
criterion_main!(benches);
