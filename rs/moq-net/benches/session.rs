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
//! `session_join_*` times a new viewer connecting, resolving a broadcast, and
//! receiving its latest group, swept over what the relays already announce.
//!
//! Everything runs on one current-thread runtime, so the only work measured is
//! the protocol and model code, never scheduling across threads.
//!
//! Run with `cargo bench -p moq-net --bench session`.

#[path = "../tests/support/mod.rs"]
mod support;

use std::time::{Duration, Instant};

use bytes::Bytes;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_net::{Hop, Timestamp, Version, broadcast, cache, origin, track};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

/// The newest moq-lite and IETF drafts.
const VERSIONS: [&str; 2] = ["moq-lite-07", "moq-transport-22"];

/// Frames per group, so per-frame and per-group costs both appear.
const FRAMES: usize = 4;

const TRACK: &str = "video";

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
	};

	fn total(&self) -> usize {
		self.publishers * self.broadcasts
	}

	/// Payload bytes every viewer reads in one round.
	fn expected(&self) -> usize {
		self.viewers * self.watch * FRAMES * self.frame
	}

	fn id(&self, version: &str) -> String {
		format!(
			"{version}/relays={}/publishers={}/broadcasts={}/viewers={}/watch={}/frame={}",
			self.relays, self.publishers, self.broadcasts, self.viewers, self.watch, self.frame
		)
	}
}

fn runtime() -> tokio::runtime::Runtime {
	tokio::runtime::Builder::new_current_thread()
		.enable_time()
		.build()
		.unwrap()
}

fn write_group(track: &track::Producer, payload: &Bytes) {
	let mut group = track.append_group().unwrap();
	for _ in 0..FRAMES {
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

	/// Connect a viewer to `relay` and subscribe it to each of `broadcasts`.
	async fn join(&mut self, relay: usize, broadcasts: impl Iterator<Item = usize>) -> Viewer {
		let origin = self.origin(CLIENT_CACHE);
		let mut options = MockConnectOptions::new(self.version);
		options.server_publish = Some(self.relays[relay].consume());
		options.client_subscribe = Some(origin.clone());
		let pair = connect_mock(options).await;

		let mut subscribers = Vec::new();
		for broadcast in broadcasts {
			let broadcast = origin.consume().routed_broadcast(path(broadcast)).await.unwrap();
			subscribers.push(broadcast.track(TRACK).unwrap().subscribe(None).await.unwrap());
		}

		Viewer {
			subscribers,
			_pair: pair,
			_origin: origin,
		}
	}
}

struct Viewer {
	subscribers: Vec<track::Subscriber>,
	_pair: MockPair,
	_origin: origin::Producer,
}

/// A cluster with its viewers attached.
struct Room {
	cluster: Cluster,
	viewers: Vec<Viewer>,
	/// Broadcasts at least one viewer watches; only these write each round.
	watched: Vec<usize>,
	payload: Bytes,
	expected: usize,
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
			let relay = (viewer + 1) % shape.relays;
			viewers.push(cluster.join(relay, broadcasts.into_iter()).await);
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
			expected: shape.expected(),
		};
		// Warm every path so the timed rounds skip first-group setup.
		room.round().await;
		room
	}

	async fn round(&mut self) {
		for &broadcast in &self.watched {
			write_group(&self.cluster.tracks[broadcast], &self.payload);
		}
		let mut bytes = 0;
		for viewer in &mut self.viewers {
			for subscriber in &mut viewer.subscribers {
				bytes += read_group(subscriber).await;
			}
		}
		assert_eq!(bytes, self.expected);
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
							write_group(track, &payload);
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
							let mut viewer = cluster.join(relay, std::iter::once(broadcast)).await;
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

fn session(c: &mut Criterion) {
	let base = Shape::BASE;

	delivery(
		c,
		"publishers",
		[1, 16, 256].map(|publishers| Shape { publishers, ..base }),
	);
	delivery(c, "viewers", [1, 16, 256].map(|viewers| Shape { viewers, ..base }));
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
}

criterion_group!(benches, session);
criterion_main!(benches);
