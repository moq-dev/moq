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
//! Each round's cache tick runs before it, outside the measurement.
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
//! `session_delivery_prefixes` holds silent broadcasts open on one viewer session beside the
//! one it watches, against a fixed route table. Each is an announce request the session
//! keeps, so a per-group cost that grows with them shows as a slope over `follow`.
//! `session_delivery_datagram` writes to a datagram track per broadcast instead of groups:
//! one datagram per track per round across publisher and viewer axes, then larger payloads
//! and cadences, on moq-lite and moq-transport-22. Throughput counts delivered datagrams,
//! each checked for its key and payload, so a lost one fails.
//! Delivery uses expiry-only pools: history expires between rounds outside timing,
//! so byte pressure cannot shed unread frames during a batch.
//! `SESSION_EXPIRY_ONLY=1` advances the cache clock by a second instead of the 30s
//! expiry window, retaining 30 rounds for profiling. It reports groups written and
//! charged cache bytes every 10s during long runs.
//!
//! `session_join_*` times a new viewer connecting, resolving a broadcast, and
//! receiving its latest group, swept over what the relays already announce.
//!
//! `session_announce` times one broadcast announced and withdrawn beside many a publisher
//! already announces, until every viewer session saw both, swept over those broadcasts and
//! the viewers: a change's cost grows with the viewers it reaches, never with the claims held.
//!
//! `session_dash_*` models the moq.pro dash aggregator: one session following
//! every node's per-project stats broadcast, each announced by a peer relay, at
//! one small frame per track per tick. Throughput counts tracks, so a flat
//! items/s across a sweep means the cost per track stays constant.
//!
//! `session_idle` times one second of idle viewer sessions, swept over sessions.
//! Sweep held consumers per session too: only held consumers request PROBE or transport estimates.
//! Nothing else happens, so it measures the sessions' own work. Paused time skips waiting.
//!
//! `session_churn_*` times one request opened and closed by a session that already
//! holds others open, swept over sessions and requests held per session, so a slope
//! here is per-request bookkeeping growing with the sessions or the requests.
//!
//! `session_fanout_threads` serves one broadcast's one-frame groups through one relay to 64
//! viewers on multi-threaded runtimes; compare their process CPU, which `swole bench`
//! reports. Each group wakes every subscription's serve task at once, so a read path that
//! makes them take one shared lock many times pays for it in lock waits and wakes across
//! workers: the `workers=16` rows climb above the `workers=1` ones.
//!
//! Everything else runs on one current-thread runtime, so the only work measured is
//! protocol and relay work, never scheduling across threads.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench session`, which runs each case
//! once, or time them with swole.

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
#[path = "../../tests/integration/support/schedule.rs"]
mod schedule;

use std::fmt;
use std::time::{Duration, Instant};

use bytes::Bytes;
pub(crate) use harness::MockConnectOptions;
use harness::{MockPair, connect_mock};
use mock::{MockSession, MockStats};
use moq_net::{Timestamp, Version, broadcast, cache, datagram, group, origin, track};

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> track::Info {
	track::Info::default()
		.with_max_age(None)
		.with_timescale(moq_net::Timescale::MILLI)
}

/// The lite draft production negotiates, the next one (opt-in), and the newest
/// IETF draft.
const VERSIONS: [&str; 3] = ["moq-lite-06", "moq-lite-07-wip", "moq-transport-22"];

/// Frames per group, so per-frame and per-group costs both appear.
const FRAMES: usize = 4;

/// Distinct datagram payloads a datagram round cycles through by sequence.
const DATAGRAM_PAYLOADS: usize = 16;

const TRACK: &str = "video";

/// The datagram track's name. Lite-05/06 carry a name's stream and datagram tracks on one
/// subscription, so a name of its own keeps the stream rounds from carrying datagrams.
const DATAGRAMS: &str = "audio";

/// Lite 06 is what the dash session and cluster peers negotiate today.
const DASH_VERSIONS: [&str; 2] = ["moq-lite-06", "moq-transport-22"];

/// A stats frame: a small JSON snapshot.
const DASH_FRAME: usize = 128;

/// Initial budgets for join and churn fixtures. Delivery rooms use expiry-only pools.
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
	/// Further broadcasts each viewer watches the announcements of without subscribing,
	/// each keeping one more announce request open on its session.
	follow: usize,
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
	/// Deliver each round as `burst` datagrams of `frame` bytes per watched broadcast's
	/// datagram track instead of groups.
	datagrams: bool,
	/// The metadata sweep (lite-07 only): `Some(true)` reads each group as a metadata
	/// subscription, frame headers alone, and `Some(false)` is its payload baseline.
	meta: Option<bool>,
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
		follow: 0,
		frame: 64,
		group_frames: FRAMES,
		paced: false,
		burst: 1,
		history: 1,
		window: 1024 * 1024,
		credit: None,
		fetch: false,
		compress: false,
		datagrams: false,
		meta: None,
	};

	/// What each viewer subscribes with: a burst is a backlog, which the default
	/// subscription writes off as late.
	fn subscription(&self) -> Option<track::Subscription> {
		(self.burst > 1).then(|| {
			track::Subscription::default().with_ranges([track::Range::from(track::Start::Live {
				min: Some(Duration::from_secs(30)),
				max: Some(Duration::from_secs(30)),
			})])
		})
	}

	fn total(&self) -> usize {
		self.publishers * self.broadcasts
	}

	/// Datagrams every viewer reads in one datagram round.
	fn objects(&self) -> usize {
		self.viewers * self.watch * self.burst
	}

	/// Whether `version` carries this shape. Datagram tracks run on moq-lite and on
	/// moq-transport-22, which maps one to OBJECT_DATAGRAM at object ID 0.
	fn runs_on(&self, version: &str) -> bool {
		let datagrams =
			!self.datagrams || version.parse::<Version>().unwrap().is_lite() || version == "moq-transport-22";
		datagrams && (self.meta.is_none() || version == "moq-lite-07-wip")
	}

	/// The broadcasts viewer `viewer` watches: a contiguous window starting at
	/// `viewer * watch`, so viewers spread over the broadcasts before any doubles up.
	fn watching(&self, viewer: usize) -> impl Iterator<Item = usize> {
		let (watch, total) = (self.watch, self.total());
		(0..watch).map(move |k| (viewer * watch + k) % total)
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
		// The credit sweep names only what it varies, keeping its names short.
		if let Some(credit) = self.credit {
			return format!("{version}/watch={}/burst={}/credit={credit}", self.watch, self.burst);
		}
		if self.follow > 0 {
			return format!("{version}/broadcasts={}/follow={}", self.broadcasts, self.follow);
		}
		if self.datagrams {
			return format!(
				"{version}/publishers={}/viewers={}/payload={}/cadence={}",
				self.publishers, self.viewers, self.frame, self.burst
			);
		}
		if let Some(meta) = self.meta {
			return format!(
				"{version}/relays={}/viewers={}/watch={}/frame={}/{}",
				self.relays,
				self.viewers,
				self.watch,
				self.frame,
				if meta { "meta" } else { "payload" }
			);
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

/// A current-thread runtime for one benchmark cell: the sessions a cell leaves behind stop
/// with its runtime instead of running inside the next cell's setup and samples.
fn runtime() -> tokio::runtime::Runtime {
	tokio::runtime::Builder::new_current_thread()
		.enable_time()
		.build()
		.unwrap()
}

/// How long a lite-07 origin's route node holds a closed session's claims in case another
/// session serves them: its debounce, 32 hops of 25 ms (`route::relay::debounce`).
const ROUTE_LINGER: Duration = Duration::from_millis(32 * 25);

/// Drop what a cell built on `rt` and assert every task its sessions and origins spawned
/// ends with it: a task that outlives its handles leaks for as long as a relay runs. The
/// origins may first wait out [`ROUTE_LINGER`], on a paused clock so the wait costs nothing.
fn retire<T>(rt: &tokio::runtime::Runtime, built: T) {
	drop(built);
	rt.block_on(async {
		tokio::time::pause();
		// Each close reaches the next driver on its following poll; this many passes is far
		// more than any teardown chain takes.
		let settle = || async {
			for _ in 0..1024 {
				if rt.metrics().num_alive_tasks() == 0 {
					break;
				}
				tokio::task::yield_now().await;
			}
		};
		settle().await;
		tokio::time::sleep(ROUTE_LINGER).await;
		settle().await;
	});
	assert_eq!(rt.metrics().num_alive_tasks(), 0, "tasks outlived the cell's handles");
}

/// A multi-threaded runtime with `workers` threads, for the scenarios compared in process CPU.
fn threaded(workers: usize) -> tokio::runtime::Runtime {
	tokio::runtime::Builder::new_multi_thread()
		.worker_threads(workers)
		.enable_time()
		.build()
		.unwrap()
}

/// Append the next group on a [`timed`] track, one millisecond past its largest start or at
/// 0 on an empty track, so groups keep the key order a counter would give them.
fn append_next(track: &track::Producer) -> group::Producer {
	let key = track.largest().map_or(0, |largest| largest.group + 1);
	track.append_group(Timestamp::from_millis(key).unwrap()).unwrap()
}

/// Where a group on a [`timed`] track starts, as its frames' timestamp.
fn at(group: &group::Producer) -> Timestamp {
	Timestamp::from_millis(group.start).unwrap()
}

/// The groups from the one starting at `key` onward.
fn from(key: u64) -> track::Range {
	track::Range {
		start: track::Start::Time(key),
		end: None,
	}
}

fn write_group(track: &track::Producer, payload: &Bytes, frames: usize) -> u64 {
	let mut group = append_next(track);
	let timestamp = at(&group);
	for _ in 0..frames {
		group.write_frame(timestamp, payload.clone()).unwrap();
	}
	let group = group.finish().unwrap();
	group.start
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

/// The bytes the next group's frames declare, read as a metadata subscription reads them: the
/// same total [`read_group`] reads, with no payload behind it.
async fn read_headers(subscriber: &mut moq_net::meta::Subscriber) -> usize {
	let mut group = subscriber.recv_group().await.unwrap().expect("track ended");
	let mut bytes = 0;
	while let Some(header) = group.next_header().await.unwrap() {
		bytes += header.size as usize;
	}
	bytes
}

fn path(broadcast: usize) -> String {
	format!("room/{broadcast}")
}

/// The relay implementation a shared scenario drives.
pub(crate) trait Backend {
	type Relay: Clone;

	fn relay(id: u64, capacity: Option<u64>) -> (Self::Relay, cache::Pool);
	fn origin(relay: &Self::Relay) -> &origin::Producer;
	async fn connect(options: MockConnectOptions, relay: &Self::Relay, hidden: bool) -> Pair;
}

/// The model-only relay used by the original session benchmark.
pub(crate) struct Model;

impl Backend for Model {
	type Relay = origin::Producer;

	fn relay(id: u64, capacity: Option<u64>) -> (Self::Relay, cache::Pool) {
		// A relay forwards the routes it learns, as moq-relay's cluster does; off, a
		// moq-lite-07 viewer never learns the publisher's broadcasts.
		make_origin(id, capacity, true)
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
	pub discovery: Option<moq_net::origin::Routes>,
	pub task: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for Pair {
	fn drop(&mut self) {
		if let Some(task) = self.task.take() {
			task.abort();
		}
	}
}

fn make_origin(id: u64, capacity: Option<u64>, proxy: bool) -> (origin::Producer, cache::Pool) {
	let mut config = origin::Config::new(std::num::NonZeroU64::new(id).unwrap());
	let profiling = std::env::var_os("SESSION_EXPIRY_ONLY").is_some();
	let pool = cache::Pool::new(if capacity.is_none() || profiling {
		cache::Config::default().with_expiry(cache::DEFAULT_EXPIRY)
	} else {
		cache::Config::default().with_capacity(capacity)
	});
	config.pool = pool.clone();
	config.proxy = proxy;
	let (producer, driver) = origin::Producer::new(config);
	harness::spawn(driver);
	(producer, pool)
}

/// Relays and publishers, before any viewer joins. Holds every handle that
/// keeps a session or broadcast alive.
struct Cluster<B: Backend> {
	expiry: bool,
	version: Version,
	window: usize,
	credit: Option<usize>,
	/// Whether each viewer asks its relay about everything, rather than only what it reads.
	discovers_all: bool,
	relays: Vec<B::Relay>,
	/// One track per broadcast, indexed like [`path`].
	tracks: Vec<track::Producer>,
	/// One datagram track per broadcast, indexed like [`path`], for datagram shapes only.
	datagrams: Vec<datagram::Producer>,
	_broadcasts: Vec<origin::Route>,
	/// Each publisher's origin, indexed like `publisher`.
	origins: Vec<origin::Producer>,
	_pairs: Vec<Pair>,
	next_hop: u64,
	/// The relay and publisher caches; each viewer owns its own.
	pools: Vec<cache::Pool>,
}

impl<B: Backend> Cluster<B> {
	async fn new(version: &str, shape: Shape, expiry: bool) -> Self {
		let mut this = Self {
			expiry,
			version: version.parse().unwrap(),
			window: shape.window,
			credit: shape.credit,
			// Asking about everything would cover every followed broadcast with one request.
			discovers_all: shape.follow == 0,
			relays: Vec::new(),
			tracks: Vec::new(),
			datagrams: Vec::new(),
			_broadcasts: Vec::new(),
			origins: Vec::new(),
			_pairs: Vec::new(),
			next_hop: 0,
			pools: Vec::new(),
		};

		for _ in 0..shape.relays {
			this.next_hop += 1;
			let (relay, pool) = B::relay(this.next_hop, (!expiry).then_some(RELAY_CACHE));
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
					.route(
						path(publisher * shape.broadcasts + index),
						origin::RouteOptions::default(),
					)
					.unwrap();
				this.tracks.push(
					broadcast
						.broadcast()
						.create_track(
							TRACK,
							timed().with_compression(match shape.compress {
								true => moq_net::track::Compression::Deflate,
								false => moq_net::track::Compression::None,
							}),
						)
						.unwrap(),
				);
				if shape.datagrams {
					let info = datagram::Info::default().with_timescale(moq_net::Timescale::MILLI);
					this.datagrams
						.push(broadcast.broadcast().create_datagrams(DATAGRAMS, info).unwrap());
				}
				this._broadcasts.push(broadcast);
			}

			let mut options = MockConnectOptions::new(this.version);
			options.transport.stream_buffer = this.window;
			options.client_publish = Some(origin.consume());
			let target = &this.relays[publisher % shape.relays];
			options.server_subscribe = Some(B::origin(target).clone());
			this._pairs.push(B::connect(options, target, false).await);
			this.origins.push(origin);
		}

		this
	}

	fn origin(&mut self, capacity: u64) -> (origin::Producer, cache::Pool) {
		self.next_hop += 1;
		make_origin(self.next_hop, (!self.expiry).then_some(capacity), false)
	}

	/// Connect a viewer session to `relay`.
	async fn join(&mut self, relay: usize, hidden: bool) -> Viewer {
		let (origin, pool) = self.origin(CLIENT_CACHE);
		let mut options = MockConnectOptions::new(self.version);
		options.transport.stream_buffer = self.window;
		// Draft 22 keeps its SETUP stream open beside the groups, and moq-lite-07 the
		// relay's Announce Stream: neither may take the one grant the groups share.
		let persistent = !self.version.is_lite() || self.version.as_str() == "moq-lite-07-wip";
		options.transport.max_concurrent_uni_streams = self.credit.map(|credit| credit + usize::from(persistent));
		options.client_discovers_all = self.discovers_all;
		// Hidden included, as the dash's `.stats/` scope sees its subtree.
		options.server_publish = Some(B::origin(&self.relays[relay]).consume().with_hidden(true));
		options.client_subscribe = Some(origin.clone());
		let pair = B::connect(options, &self.relays[relay], hidden).await;

		Viewer {
			following: Vec::new(),
			subscribers: Vec::new(),
			metas: Vec::new(),
			datagrams: Vec::new(),
			fetches: Vec::new(),
			blocked: 0,
			_pair: pair,
			origin,
			pool,
		}
	}
}

struct Viewer {
	/// Announcements watched without a subscription, one announce request each.
	following: Vec<moq_net::origin::Routes>,
	subscribers: Vec<track::Subscriber>,
	/// Metadata subscriptions, read beside `subscribers`.
	metas: Vec<moq_net::meta::Subscriber>,
	/// Datagram track consumers, in the order of the broadcasts watched.
	datagrams: Vec<datagram::Consumer>,
	fetches: Vec<(usize, track::Consumer)>,
	blocked: usize,
	_pair: Pair,
	origin: origin::Producer,
	pool: cache::Pool,
}

impl Viewer {
	/// Watch the announcements under `path` until it is announced, subscribing to nothing.
	async fn follow(&mut self, path: &str) {
		let mut announced = self
			.origin
			.consume()
			.scope(path, &moq_net::Prefixes::all())
			.unwrap()
			.routes();
		let event = announced.next().await.expect("announced");
		assert!(
			matches!(
				event,
				moq_net::origin::RouteEvent {
					kind: moq_net::origin::RouteKind::Start,
					..
				}
			),
			"{event:?}"
		);
		self.following.push(announced);
	}

	/// Resolve `path` and consume its datagram track, once its publisher answers.
	async fn watch_datagrams(&mut self, path: &str) {
		let broadcast = self
			.origin
			.consume()
			.follow(path, None)
			.unwrap()
			.available()
			.await
			.unwrap()
			.unwrap();
		let consumer = broadcast.datagrams(DATAGRAMS).unwrap();
		consumer.info().await.unwrap();
		self.datagrams.push(consumer);
	}

	/// Resolve `path` and subscribe to the metadata of its `track`.
	async fn watch_meta(&mut self, path: &str, track: &str, subscription: Option<track::Subscription>) {
		let broadcast = self
			.origin
			.consume()
			.follow(path, None)
			.unwrap()
			.available()
			.await
			.unwrap()
			.unwrap();
		self.metas
			.push(broadcast.meta(track).unwrap().subscribe(subscription).await.unwrap());
	}

	/// Resolve `path` and subscribe to each of its `tracks`.
	async fn watch(&mut self, path: &str, tracks: &[impl AsRef<str>], subscription: Option<track::Subscription>) {
		let broadcast = self
			.origin
			.consume()
			.follow(path, None)
			.unwrap()
			.available()
			.await
			.unwrap()
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
pub(crate) struct Room<B: Backend> {
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
	/// Distinct datagram payloads, picked by sequence so a crossed or stale delivery fails.
	payloads: Vec<Bytes>,
	/// The sequence each broadcast's next datagram takes.
	next: Vec<u64>,
	cache_now: moq_net::time::Instant,
	/// Under `SESSION_EXPIRY_ONLY`, when rounds began and when the next cache sample is due.
	report: Option<(Instant, Duration)>,
}

impl<B: Backend> Room<B> {
	async fn new(version: &str, shape: Shape) -> Self {
		assert!(
			shape.watch + shape.follow <= shape.total(),
			"a viewer can't watch or follow more broadcasts than exist"
		);
		assert!(!shape.paced || shape.burst == 1, "a paced round writes one frame");
		assert!(!shape.fetch || !shape.paced, "fetch rounds read finished groups");
		let mut cluster = Cluster::<B>::new(version, shape, true).await;

		let mut viewers = Vec::new();
		let mut watched = vec![false; shape.total()];
		for viewer_index in 0..shape.viewers {
			let broadcasts: Vec<_> = shape.watching(viewer_index).collect();
			for &broadcast in &broadcasts {
				watched[broadcast] = true;
			}
			// Offset by one so a viewer lands on a different relay than the
			// publisher it watches whenever there is more than one.
			let mut viewer = cluster.join((viewer_index + 1) % shape.relays, false).await;
			for broadcast in broadcasts {
				if shape.datagrams {
					viewer.watch_datagrams(&path(broadcast)).await;
				} else if shape.fetch {
					let source = viewer
						.origin
						.consume()
						.follow(path(broadcast), None)
						.unwrap()
						.available()
						.await
						.unwrap()
						.unwrap();
					viewer.fetches.push((broadcast, source.track(TRACK).unwrap()));
				} else if shape.meta == Some(true) {
					viewer.watch_meta(&path(broadcast), TRACK, shape.subscription()).await;
				} else {
					viewer.watch(&path(broadcast), &[TRACK], shape.subscription()).await;
				}
			}
			// The followed broadcasts sit just past the watched window, so they stay silent.
			for k in 0..shape.follow {
				viewer
					.follow(&path((viewer_index * shape.watch + shape.watch + k) % shape.total()))
					.await;
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
			payloads: (0..DATAGRAM_PAYLOADS)
				.map(|index| Bytes::from(vec![index as u8; shape.frame]))
				.collect(),
			next: vec![0; shape.total()],
			cache_now: moq_net::time::Instant::now(),
			report: None,
		};
		room.writing.resize_with(room.watched.len(), || None);
		room.reading.resize_with(shape.viewers * shape.watch, || None);

		let blocked_before: usize = room
			.viewers
			.iter()
			.map(|viewer| viewer._pair.server_transport.blocked_writes())
			.sum();
		if shape.datagrams {
			room.sync_datagrams().await;
		}
		// Warm every path so the timed rounds skip first-group setup.
		for _ in 0..shape.group_frames / shape.frames() {
			room.tick();
			room.round().await;
		}
		// Exercise turnover as well as the empty cache, even in a run that makes one call.
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
		if std::env::var_os("SESSION_EXPIRY_ONLY").is_some() {
			room.report = Some((Instant::now(), Duration::from_secs(10)));
		}
		// Lite-07 has no FETCH stream: its fetches travel as bounded SUBSCRIBE ranges.
		if shape.fetch && shape.relays == 1 && room.cluster.version.as_str() != "moq-lite-07-wip" {
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

	/// Advance the cache/model clock outside timing to expire old batches.
	/// Each round completes its reads before this step; pending read deadlines
	/// share this clock too.
	fn tick(&mut self) {
		self.cache_now = self
			.cache_now
			.saturating_add(if std::env::var_os("SESSION_EXPIRY_ONLY").is_some() {
				Duration::from_secs(1)
			} else {
				cache::DEFAULT_EXPIRY
			});
		for pool in self.pools() {
			pool.poll(self.cache_now, &kio::Waiter::noop());
		}
		if let Some((began, next)) = self.report
			&& began.elapsed() >= next
		{
			eprintln!(
				"cache sample: elapsed={:.1}s groups={} charged={}",
				began.elapsed().as_secs_f64(),
				self.groups,
				self.pools().map(cache::Pool::used).sum::<u64>()
			);
			self.report = Some((began, next + Duration::from_secs(10)));
		}
	}

	async fn round(&mut self) {
		if self.shape.paced {
			return self.paced_round().await;
		}
		if self.shape.datagrams {
			return self.datagram_round().await;
		}
		self.group_round().await;
	}

	async fn group_round(&mut self) {
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
						for key in newest + 1 - burst..=newest {
							let mut group = track.fetch_group(key, None).await.unwrap();
							while let Some(frame) = group.read_frame().await.unwrap() {
								bytes += frame.payload.len();
							}
						}
					}
					for subscriber in &mut viewer.subscribers {
						bytes += read_group(subscriber).await;
					}
					for subscriber in &mut viewer.metas {
						bytes += read_headers(subscriber).await;
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
		for &broadcast in &self.watched {
			self.next[broadcast] = newest[broadcast] + 1;
		}
	}

	/// Write the next datagram into every watched track.
	fn write_datagrams(&mut self) {
		for &broadcast in &self.watched {
			let sequence = self.next[broadcast];
			let payload = self.payloads[sequence as usize % DATAGRAM_PAYLOADS].clone();
			let track = &mut self.cluster.datagrams[broadcast];
			assert_eq!(track.append(Timestamp::ZERO, payload).unwrap(), sequence);
			self.next[broadcast] = sequence + 1;
		}
	}

	/// Line every viewer up on the same next key before the strict rounds.
	///
	/// A consumer is live once its track's info arrives, but on lite-05/06 the SUBSCRIBE
	/// that carries the datagrams follows that answer, so a datagram may still miss it.
	/// moq-transport's SUBSCRIBE is that answer. Write until every viewer has read one, then
	/// a last one each viewer reads through: the memory transport keeps a subscription's
	/// datagrams in order, so nothing earlier arrives after.
	async fn sync_datagrams(&mut self) {
		let waiter = kio::Waiter::noop();
		let mut reached = vec![false; self.shape.viewers * self.shape.watch];
		for attempt in 0.. {
			if !reached.contains(&false) {
				break;
			}
			// Fail rather than spin: a viewer the datagrams never reach is a broken path.
			assert!(attempt < 1024, "datagrams never reached every viewer");
			self.write_datagrams();
			tokio::task::yield_now().await;
			let consumers = self.viewers.iter_mut().flat_map(|viewer| &mut viewer.datagrams);
			for (consumer, reached) in consumers.zip(&mut reached) {
				while let std::task::Poll::Ready(Ok(Some(_))) = consumer.poll_recv(&waiter) {
					*reached = true;
				}
			}
		}
		self.write_datagrams();
		for (index, viewer) in self.viewers.iter_mut().enumerate() {
			for (consumer, broadcast) in viewer.datagrams.iter_mut().zip(self.shape.watching(index)) {
				let last = self.next[broadcast] - 1;
				// Bounded as above: a last datagram that never arrives fails the run.
				let mut attempts = 0;
				loop {
					match consumer.poll_recv(&waiter) {
						std::task::Poll::Ready(Ok(Some(datagram))) if datagram.sequence == last => break,
						std::task::Poll::Ready(Ok(Some(_))) => continue,
						std::task::Poll::Ready(end) => panic!("the datagram track ended: {end:?}"),
						std::task::Poll::Pending => {
							attempts += 1;
							assert!(attempts < 1024, "the last datagram never reached a viewer");
							tokio::task::yield_now().await;
						}
					}
				}
			}
		}
	}

	/// `burst` datagrams into every watched track, then every one out to each viewer
	/// watching it. The memory transport loses none below its queue bound, so each must
	/// arrive in order with its payload: a dropped datagram fails rather than looking
	/// cheaper.
	async fn datagram_round(&mut self) {
		for _ in 0..self.shape.burst {
			self.write_datagrams();
		}
		let burst = self.shape.burst as u64;
		let mut delivered = 0;
		for (index, viewer) in self.viewers.iter_mut().enumerate() {
			for (consumer, broadcast) in viewer.datagrams.iter_mut().zip(self.shape.watching(index)) {
				let first = self.next[broadcast] - burst;
				for sequence in first..first + burst {
					let datagram = consumer.recv().await.unwrap().expect("track ended");
					assert_eq!(datagram.sequence, sequence, "datagram lost or reordered");
					assert_eq!(
						datagram.payload,
						self.payloads[sequence as usize % DATAGRAM_PAYLOADS],
						"datagram payload"
					);
					delivered += 1;
				}
			}
		}
		assert_eq!(delivered, self.shape.objects());
	}

	/// One frame into every watched group and one frame out to every viewer, so each
	/// hop serves a group frame by frame across its configured frame count.
	async fn paced_round(&mut self) {
		let last = self.written % self.shape.group_frames == self.shape.group_frames - 1;
		self.written += 1;

		for (writing, &broadcast) in self.writing.iter_mut().zip(&self.watched) {
			let group = writing.get_or_insert_with(|| append_next(&self.cluster.tracks[broadcast]));
			group.write_frame(at(group), self.payload.clone()).unwrap();
			if last {
				writing.take().expect("written above").finish().unwrap();
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
}

/// A fixture beside the runtime it runs on. Dropping it [`retire`]s the fixture when built
/// with [`Running::retired`], and otherwise drops it before the runtime.
pub(crate) struct Running<T> {
	fixture: Option<T>,
	runtime: tokio::runtime::Runtime,
	retire: bool,
}

impl<T> Running<T> {
	/// Build a fixture on `runtime` whose tasks must all end with it.
	fn retired(runtime: tokio::runtime::Runtime, build: impl Future<Output = T>) -> Self {
		Self {
			fixture: Some(runtime.block_on(build)),
			runtime,
			retire: true,
		}
	}

	/// Build a fixture on `runtime`, dropped without waiting for its tasks.
	fn dropped(runtime: tokio::runtime::Runtime, build: impl Future<Output = T>) -> Self {
		Self {
			fixture: Some(runtime.block_on(build)),
			runtime,
			retire: false,
		}
	}

	fn parts(&mut self) -> (&tokio::runtime::Runtime, &mut T) {
		(
			&self.runtime,
			self.fixture.as_mut().expect("dropped only with its runtime"),
		)
	}
}

impl<T> Drop for Running<T> {
	fn drop(&mut self) {
		let fixture = self.fixture.take();
		match self.retire {
			true => retire(&self.runtime, fixture),
			false => {
				let _context = self.runtime.enter();
				drop(fixture);
			}
		}
	}
}

/// One delivery case: a version and a [`Shape`] it runs on, named by [`Shape::id`].
#[derive(Clone, Copy)]
pub(crate) struct Case {
	version: &'static str,
	shape: Shape,
}

impl Case {
	/// Payload bytes every viewer reads in one round.
	pub(crate) fn bytes(self) -> usize {
		self.shape.expected()
	}

	/// Datagrams every viewer reads in one datagram round.
	pub(crate) fn objects(self) -> usize {
		self.shape.objects()
	}
}

impl fmt::Display for Case {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		f.write_str(&self.shape.id(self.version))
	}
}

/// Every [`VERSIONS`] entry crossed with `shapes`, keeping only the versions a shape runs on
/// when `filter` is set.
fn cases(shapes: impl IntoIterator<Item = Shape>, filter: bool) -> Vec<Case> {
	let shapes: Vec<_> = shapes.into_iter().collect();
	VERSIONS
		.into_iter()
		.flat_map(|version| {
			shapes
				.iter()
				.filter(move |shape| !filter || shape.runs_on(version))
				.map(move |&shape| Case { version, shape })
		})
		.collect()
}

/// A delivery room on its own current-thread runtime.
pub(crate) type Delivering<B> = Running<Room<B>>;

pub(crate) fn delivering<B: Backend>(case: Case) -> Delivering<B> {
	Running::retired(runtime(), Room::<B>::new(case.version, case.shape))
}

/// Advance the cache clock before a round, outside the measurement: its cost grows with the
/// number of pools, not with the delivery being measured.
pub(crate) fn tick<B: Backend>(room: &mut Delivering<B>) {
	let (runtime, room) = room.parts();
	let _context = runtime.enter();
	room.tick();
}

pub(crate) fn round<B: Backend>(room: &mut Delivering<B>) {
	let (runtime, room) = room.parts();
	runtime.block_on(room.round());
}

/// Delivery cases over `shapes`, each on the versions it runs on.
fn delivery(shapes: impl IntoIterator<Item = Shape>) -> Vec<Case> {
	cases(shapes, true)
}

/// Metadata subscriptions against the same rooms read with the payload, swept over relay depth,
/// viewers, and tracks each viewer watches, at a frame large enough that payload dominates: the
/// headers' cost must not grow with the payload they stand for.
pub(crate) fn delivery_meta() -> Vec<Case> {
	let mut shapes = Vec::new();
	for relays in [1, 3] {
		for viewers in [1, 64] {
			for watch in [1, 8] {
				for meta in [false, true] {
					shapes.push(Shape {
						relays,
						viewers,
						watch,
						frame: 16 * 1024,
						meta: Some(meta),
						..Shape::BASE
					});
				}
			}
		}
	}
	delivery(shapes)
}

/// Small-group cost over independent publisher/viewer axes, with both egress request paths.
pub(crate) fn delivery_group_cost() -> Vec<Case> {
	delivery([1, 16].into_iter().flat_map(|publishers| {
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
	}))
}

/// Complete datagram delivery: per-object cost over independent publisher and viewer axes at
/// a small payload and one datagram per track per round, then payload size and cadence at 16
/// publishers and 16 viewers. 64 per round fills what a datagram track holds for an unread
/// consumer exactly.
pub(crate) fn delivery_datagram() -> Vec<Case> {
	let base = Shape {
		datagrams: true,
		..Shape::BASE
	};
	let cost = [1, 16].into_iter().flat_map(move |publishers| {
		[1, 16, 64].map(move |viewers| Shape {
			publishers,
			viewers,
			..base
		})
	});
	let payload = [1024].map(|frame| Shape { frame, ..base });
	let cadence = [16, 64].map(|burst| Shape { burst, ..base });
	delivery(cost.chain(payload).chain(cadence))
}

/// Cached FETCH write cost across independent publishers, viewers, and group lengths.
pub(crate) fn delivery_fetch_frames() -> Vec<Case> {
	delivery([1, 4].into_iter().flat_map(|publishers| {
		[1, 16].into_iter().flat_map(move |viewers| {
			[4, 256].map(move |group_frames| Shape {
				publishers,
				viewers,
				group_frames,
				fetch: true,
				..Shape::BASE
			})
		})
	}))
}

/// Independent one-group FETCHes across publishers, viewers, and burst lengths.
pub(crate) fn delivery_fetch_range() -> Vec<Case> {
	delivery([1, 16].into_iter().flat_map(|publishers| {
		[1, 16].into_iter().flat_map(move |viewers| {
			[2, 16].map(move |burst| Shape {
				publishers,
				viewers,
				burst,
				fetch: true,
				..Shape::BASE
			})
		})
	}))
}

pub(crate) fn delivery_compression() -> Vec<Case> {
	delivery([1, 16].into_iter().flat_map(|publishers| {
		[1, 16].map(move |viewers| Shape {
			publishers,
			viewers,
			compress: true,
			frame: 1024,
			..Shape::BASE
		})
	}))
}

pub(crate) fn delivery_publishers() -> Vec<Case> {
	delivery([1, 16, 256].map(|publishers| Shape {
		publishers,
		..Shape::BASE
	}))
}

pub(crate) fn delivery_viewers() -> Vec<Case> {
	delivery([1, 16, 256].map(|viewers| Shape { viewers, ..Shape::BASE }))
}

/// Live media: every hop serves each group frame by frame.
pub(crate) fn delivery_paced() -> Vec<Case> {
	delivery([1, 16, 256].map(|viewers| Shape {
		viewers,
		paced: true,
		..Shape::BASE
	}))
}

pub(crate) fn delivery_scale() -> Vec<Case> {
	delivery([16, 64, 256].map(|n| Shape {
		publishers: n,
		viewers: n,
		..Shape::BASE
	}))
}

/// One publisher session announcing many broadcasts, most of them unwatched.
pub(crate) fn delivery_broadcasts() -> Vec<Case> {
	delivery([16, 256, 4096].map(|broadcasts| Shape {
		publishers: 1,
		broadcasts,
		..Shape::BASE
	}))
}

/// Each viewer watches many broadcasts over one session, like a conference.
pub(crate) fn delivery_watch() -> Vec<Case> {
	delivery([1, 16, 256].map(|watch| Shape {
		publishers: 1,
		broadcasts: 256,
		watch,
		..Shape::BASE
	}))
}

/// One viewer session holding many silent broadcasts open beside the one it watches: each is
/// an announce request on the session, against a fixed route table. A wake for the watched
/// group should not cost a poll per held request.
pub(crate) fn delivery_prefixes() -> Vec<Case> {
	delivery([1, 64, 1024].map(|follow| Shape {
		publishers: 1,
		broadcasts: 1025,
		viewers: 1,
		follow,
		..Shape::BASE
	}))
}

pub(crate) fn delivery_frame() -> Vec<Case> {
	delivery([64, 1024, 16 * 1024].map(|frame| Shape { frame, ..Shape::BASE }))
}

/// One session serving a backlog on each of its subscriptions: the serve loop that a
/// `kio::coop::Budget` bounds per poll.
pub(crate) fn delivery_burst() -> Vec<Case> {
	delivery(
		[(1, 1), (1, 8), (1, 64), (8, 1), (8, 8), (16, 16), (16, 64)].map(|(watch, burst)| Shape {
			publishers: 1,
			broadcasts: 16,
			viewers: 1,
			watch,
			burst,
			..Shape::BASE
		}),
	)
}

/// The same backlog behind one uni stream grant at a time.
pub(crate) fn delivery_credit() -> Vec<Case> {
	delivery([1, 16, 64].into_iter().flat_map(|watch| {
		[1, 16, 64].map(move |burst| Shape {
			publishers: 1,
			broadcasts: 64,
			viewers: 1,
			watch,
			burst,
			credit: Some(1),
			..Shape::BASE
		})
	}))
}

pub(crate) fn delivery_relays() -> Vec<Case> {
	delivery([1, 2, 4, 8].map(|relays| Shape {
		relays,
		viewers: 64,
		..Shape::BASE
	}))
}

/// Largest-group fills, swept over cached backlog (`groups_<span>`) and concurrent requests.
#[derive(Clone, Copy)]
pub(crate) struct Span {
	span: u64,
	pub(crate) requests: usize,
}

impl fmt::Display for Span {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "groups_{}/{}", self.span, self.requests)
	}
}

pub(crate) fn spans() -> Vec<Span> {
	[2u64, 16]
		.into_iter()
		.flat_map(|span| [1usize, 16].map(|requests| Span { span, requests }))
		.collect()
}

/// Draft-22 viewers each holding a subscription a cached burst ahead of the publisher.
pub(crate) struct RangeUpdates<B: Backend> {
	cluster: Cluster<B>,
	viewers: Vec<Viewer>,
	payload: Bytes,
	span: u64,
	/// The first group of the burst [`narrow`] writes next.
	next: u64,
	/// One past the burst [`narrow`] wrote, which [`range_update`] widens over.
	future: u64,
}

pub(crate) fn range_updates<B: Backend>(case: Span) -> Running<RangeUpdates<B>> {
	Running::dropped(runtime(), async move {
		let mut cluster = Cluster::<B>::new(
			"moq-transport-22",
			Shape {
				publishers: 1,
				..Shape::BASE
			},
			false,
		)
		.await;
		let mut viewers = Vec::new();
		for _ in 0..case.requests {
			let mut viewer = cluster.join(0, false).await;
			viewer
				.watch(
					&path(0),
					&[TRACK],
					Some(track::Subscription::default().with_ranges([from(case.span)])),
				)
				.await;
			viewers.push(viewer);
		}
		RangeUpdates {
			cluster,
			viewers,
			payload: Bytes::from_static(&[0; 64]),
			span: case.span,
			next: 0,
			future: 0,
		}
	})
}

/// Narrow every subscription past a fresh burst and write the burst, outside the measurement.
pub(crate) fn narrow<B: Backend>(fixture: &mut Running<RangeUpdates<B>>) {
	let (runtime, fixture) = fixture.parts();
	runtime.block_on(async {
		let future = fixture.next + fixture.span;
		for viewer in &mut fixture.viewers {
			viewer.subscribers[0]
				.update(track::Subscription::default().with_ranges([from(future)]))
				.unwrap();
		}
		let track = &mut fixture.cluster.tracks[0];
		let viewers = &mut fixture.viewers;
		kio::wait(|waiter| {
			for viewer in viewers.iter_mut() {
				assert!(viewer.subscribers[0].poll_recv_group(waiter).is_pending());
			}
			loop {
				if track
					.subscription()
					.is_some_and(|subscription| subscription.ranges == [from(future)])
				{
					return std::task::Poll::Ready(());
				}
				if track.poll_subscription_changed(waiter).is_pending() {
					return std::task::Poll::Pending;
				}
			}
		})
		.await;
		for key in fixture.next..future {
			assert_eq!(write_group(track, &fixture.payload, 1), key);
		}
		fixture.future = future;
	});
}

/// Widen every subscription over the burst's largest group and read it.
pub(crate) fn range_update<B: Backend>(fixture: &mut Running<RangeUpdates<B>>) {
	let (runtime, fixture) = fixture.parts();
	runtime.block_on(async {
		let future = fixture.future;
		for viewer in &mut fixture.viewers {
			viewer.subscribers[0]
				.update(track::Subscription::default().with_ranges([from(future - 1)]))
				.unwrap();
		}
		for viewer in &mut fixture.viewers {
			assert_eq!(read_group(&mut viewer.subscribers[0]).await, fixture.payload.len());
		}
		fixture.next = future;
	});
}

/// A cluster with history cached on every track, which new viewers join.
pub(crate) struct Joining<B: Backend> {
	cluster: Cluster<B>,
	shape: Shape,
	/// Broadcasts published to relay 0, joined from the last relay: every join is local on
	/// one relay and crosses one peer hop on a mesh.
	hosted: Vec<usize>,
	joins: usize,
	/// The viewer the last join connected, left after the measurement.
	joined: Option<Viewer>,
}

pub(crate) fn joining<B: Backend>(case: Case) -> Running<Joining<B>> {
	let Case { version, shape } = case;
	Running::retired(runtime(), async move {
		let cluster = Cluster::<B>::new(version, shape, false).await;
		let payload = Bytes::from(vec![0; shape.frame]);
		for track in &cluster.tracks {
			for key in 0..shape.history as u64 {
				let start = Timestamp::from_millis(key).unwrap();
				let mut group = track.append_group(start).unwrap();
				for _ in 0..shape.group_frames {
					group.write_frame(start, payload.clone()).unwrap();
				}
				group.finish().unwrap();
			}
		}
		Joining {
			cluster,
			shape,
			hosted: (0..shape.total())
				.filter(|broadcast| (broadcast / shape.broadcasts) % shape.relays == 0)
				.collect(),
			joins: 0,
			joined: None,
		}
	})
}

/// Connect a viewer, resolve a hosted broadcast, and read its latest group.
pub(crate) fn join<B: Backend>(fixture: &mut Running<Joining<B>>) {
	let (runtime, fixture) = fixture.parts();
	let broadcast = fixture.hosted[fixture.joins % fixture.hosted.len()];
	fixture.joins += 1;
	let shape = fixture.shape;
	let viewer = runtime.block_on(async {
		let mut viewer = fixture.cluster.join(shape.relays - 1, false).await;
		viewer.watch(&path(broadcast), &[TRACK], None).await;
		let bytes = read_group(&mut viewer.subscribers[0]).await;
		assert_eq!(bytes, shape.group_frames * shape.frame);
		viewer
	});
	fixture.joined = Some(viewer);
}

/// Teardown is a leaving viewer's cost, not a joining one's.
pub(crate) fn leave<B: Backend>(fixture: &mut Running<Joining<B>>) {
	let (runtime, fixture) = fixture.parts();
	let joined = fixture.joined.take();
	runtime.block_on(async {
		drop(joined);
		tokio::task::yield_now().await;
	});
}

pub(crate) fn join_broadcasts() -> Vec<Case> {
	cases(
		[1, 2, 8].into_iter().flat_map(|relays| {
			[1, 64, 1024].map(move |broadcasts| Shape {
				publishers: 1,
				broadcasts,
				relays,
				..Shape::BASE
			})
		}),
		false,
	)
}

/// A live join skips a cached prefix. Sweep publishers against that prefix so resolving one
/// start cannot hide a scan of unrelated publishers or group IDs.
pub(crate) fn join_stale() -> Vec<Case> {
	cases(
		[1, 8, 32].into_iter().flat_map(|publishers| {
			[1, 64, 1024].map(move |history| Shape {
				publishers,
				history,
				..Shape::BASE
			})
		}),
		false,
	)
}

pub(crate) fn join_relays() -> Vec<Case> {
	cases([1, 2, 8].map(|relays| Shape { relays, ..Shape::BASE }), false)
}

/// One more broadcast announced and withdrawn beside `broadcasts` a publisher already
/// announces through one relay, until each of `viewers` sessions saw both.
#[derive(Clone, Copy)]
pub(crate) struct Announce {
	version: &'static str,
	broadcasts: usize,
	viewers: usize,
}

impl fmt::Display for Announce {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(
			f,
			"{}/broadcasts={}/viewers={}",
			self.version, self.broadcasts, self.viewers
		)
	}
}

pub(crate) fn announces() -> Vec<Announce> {
	VERSIONS
		.into_iter()
		.flat_map(|version| {
			[(16, 1), (4096, 1), (16, 64), (4096, 64)].map(|(broadcasts, viewers)| Announce {
				version,
				broadcasts,
				viewers,
			})
		})
		.collect()
}

/// A publisher's broadcasts and every viewer session's announcements of them.
pub(crate) struct Announcing<B: Backend> {
	cluster: Cluster<B>,
	routes: Vec<(Viewer, origin::Routes)>,
	/// The next fresh path: a broadcast's identity never comes back.
	next: usize,
}

pub(crate) fn announcing<B: Backend>(case: Announce) -> Running<Announcing<B>> {
	let Announce {
		version,
		broadcasts,
		viewers,
	} = case;
	Running::retired(runtime(), async move {
		let shape = Shape {
			publishers: 1,
			broadcasts,
			viewers,
			..Shape::BASE
		};
		let mut cluster = Cluster::<B>::new(version, shape, false).await;
		let mut routes = Vec::new();
		for _ in 0..viewers {
			let viewer = cluster.join(0, false).await;
			let mut announced = viewer.origin.consume().routes();
			for _ in 0..broadcasts {
				let event = announced.next().await.expect("announced");
				assert_eq!(event.kind, origin::RouteKind::Start);
			}
			routes.push((viewer, announced));
		}
		Announcing {
			cluster,
			routes,
			next: broadcasts,
		}
	})
}

pub(crate) fn announce<B: Backend>(fixture: &mut Running<Announcing<B>>) {
	let (runtime, fixture) = fixture.parts();
	runtime.block_on(async {
		let route = fixture.cluster.origins[0]
			.route(path(fixture.next), origin::RouteOptions::default())
			.unwrap();
		fixture.next += 1;
		for (_, announced) in fixture.routes.iter_mut() {
			let event = announced.next().await.expect("announced");
			assert_eq!(event.kind, origin::RouteKind::Start);
		}
		drop(route);
		for (_, announced) in fixture.routes.iter_mut() {
			let event = announced.next().await.expect("withdrawn");
			assert_eq!(event.kind, origin::RouteKind::End);
		}
	});
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
pub(crate) struct DashRoom<B: Backend> {
	_cluster: Cluster<B>,
	tracks: Vec<track::Producer>,
	_broadcasts: Vec<origin::Route>,
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
			false,
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
					.route(path.as_str(), origin::RouteOptions::default())
					.unwrap();
				for name in &names {
					tracks.push(broadcast.broadcast().create_track(name.as_str(), timed()).unwrap());
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
}

/// One dash case: a version and a [`Dash`].
#[derive(Clone, Copy)]
pub(crate) struct Dashing {
	version: &'static str,
	shape: Dash,
}

impl Dashing {
	/// Tracks the dash reads each tick.
	pub(crate) fn total(self) -> usize {
		self.shape.total()
	}
}

impl fmt::Display for Dashing {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		f.write_str(&self.shape.id(self.version))
	}
}

fn dashes(shapes: impl IntoIterator<Item = Dash>) -> Vec<Dashing> {
	let shapes: Vec<_> = shapes.into_iter().collect();
	DASH_VERSIONS
		.into_iter()
		.flat_map(|version| shapes.iter().map(move |&shape| Dashing { version, shape }))
		.collect()
}

/// Production (2026-09): ~34 nodes x >=5 projects x ~24 tracks.
const PROD: Dash = Dash {
	relays: 35,
	projects: 5,
	tracks: 24,
};

/// More broadcasts at a fixed track count per broadcast.
pub(crate) fn dash_projects() -> Vec<Dashing> {
	dashes([1, 4, 16].map(|projects| Dash {
		relays: 5,
		projects,
		..PROD
	}))
}

/// More tracks per broadcast at a fixed broadcast count.
pub(crate) fn dash_tracks() -> Vec<Dashing> {
	dashes([6, 24, 96].map(|tracks| Dash {
		relays: 5,
		projects: 4,
		tracks,
	}))
}

/// More peer sessions carrying the same 32 broadcasts, then the production shape.
pub(crate) fn dash_relays() -> Vec<Dashing> {
	dashes(
		[2, 3, 5, 9, 17, 33]
			.map(|relays| Dash {
				relays,
				projects: 32 / (relays - 1),
				..PROD
			})
			.into_iter()
			.chain([PROD]),
	)
}

pub(crate) fn dashing<B: Backend>(case: Dashing) -> Running<DashRoom<B>> {
	Running::retired(runtime(), DashRoom::new(case.version, case.shape))
}

pub(crate) fn dash<B: Backend>(room: &mut Running<DashRoom<B>>) {
	let (runtime, room) = room.parts();
	runtime.block_on(room.round());
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

/// One churn case: a version and a [`Churn`].
#[derive(Clone, Copy)]
pub(crate) struct Churning {
	version: &'static str,
	shape: Churn,
}

impl fmt::Display for Churning {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		f.write_str(&self.shape.id(self.version))
	}
}

fn churns(shapes: impl IntoIterator<Item = Churn>) -> Vec<Churning> {
	let shapes: Vec<_> = shapes.into_iter().collect();
	CHURN_VERSIONS
		.into_iter()
		.flat_map(|version| shapes.iter().map(move |&shape| Churning { version, shape }))
		.collect()
}

pub(crate) fn churn_sessions() -> Vec<Churning> {
	churns([1, 16, 128].map(|sessions| Churn { sessions, held: 16 }))
}

pub(crate) fn churn_held() -> Vec<Churning> {
	churns(
		[1, 4, 16]
			.into_iter()
			.flat_map(|sessions| [1, 64, 1024].map(move |held| Churn { sessions, held })),
	)
}

pub(crate) fn churning<B: Backend>(case: Churning) -> Running<ChurnRoom<B>> {
	Running::retired(runtime(), ChurnRoom::new(case.version, case.shape))
}

pub(crate) fn churn<B: Backend>(room: &mut Running<ChurnRoom<B>>) {
	let (runtime, room) = room.parts();
	runtime.block_on(room.churn());
}

/// One relay publishing a broadcast with `held` shared tracks plus one churn track per session.
pub(crate) struct ChurnRoom<B: Backend> {
	_cluster: Cluster<B>,
	_held: Vec<track::Producer>,
	_viewers: Vec<Viewer>,
	/// Each session's view of the broadcast, to open the churned subscription on.
	broadcasts: Vec<broadcast::Consumer>,
	/// The relay's side of each session's churn track, to wait out its close.
	churn: Vec<track::Producer>,
	/// The session the next churn goes to.
	next: usize,
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
			false,
		)
		.await;
		let published = cluster._broadcasts[0].broadcast();
		let held: Vec<_> = (0..shape.held).map(|i| format!("held{i}")).collect();
		let tracks: Vec<_> = held
			.iter()
			.map(|name| {
				published
					.create_track(name.as_str(), moq_net::track::Info::default().with_max_age(None))
					.unwrap()
			})
			.collect();
		let churn: Vec<_> = (0..shape.sessions)
			.map(|i| {
				published
					.create_track(format!("churn{i}"), moq_net::track::Info::default().with_max_age(None))
					.unwrap()
			})
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
					.unwrap()
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
			next: 0,
		}
	}

	/// Open and close one subscription, round-robin over the sessions.
	async fn churn(&mut self) {
		let session = self.next;
		self.next = (self.next + 1) % self.broadcasts.len();
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
		while demand.wanted_changed().await.unwrap() {}
	}
}

pub(crate) struct IdleRoom {
	_viewers: Vec<(MockPair, Vec<moq_net::bandwidth::Consumer>)>,
}

impl IdleRoom {
	async fn new(version: &str, sessions: usize, consumers: usize) -> Self {
		let (relay, driver) = origin::Producer::new(origin::Config::new(std::num::NonZeroU64::new(1).unwrap()));
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
}

/// Idle viewer sessions, each holding `consumers` PROBE or transport-estimate consumers.
#[derive(Clone, Copy)]
pub(crate) struct Idle {
	version: &'static str,
	pub(crate) sessions: usize,
	consumers: usize,
}

impl fmt::Display for Idle {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(
			f,
			"{}/sessions={}/consumers={}",
			self.version, self.sessions, self.consumers
		)
	}
}

pub(crate) fn idles() -> Vec<Idle> {
	["moq-lite-06", "moq-transport-22"]
		.into_iter()
		.flat_map(|version| {
			[1, 64, 1024].into_iter().flat_map(move |sessions| {
				[0, 1, 8].map(move |consumers| Idle {
					version,
					sessions,
					consumers,
				})
			})
		})
		.collect()
}

/// Its own runtime, so its paused clock and sessions end with the case.
pub(crate) fn idling(case: Idle) -> Running<IdleRoom> {
	Running::dropped(runtime(), async move {
		tokio::time::pause();
		IdleRoom::new(case.version, case.sessions, case.consumers).await
	})
}

/// Let a second of protocol time pass. Paused time skips the wait itself.
pub(crate) fn idle(room: &mut Running<IdleRoom>) {
	// The timer arms when built, which needs the runtime, so build it inside.
	room.runtime
		.block_on(async { kio::clock::sleep(Duration::from_secs(1)).await });
}

/// One relay serving one broadcast's one-frame groups to viewers on a multi-threaded runtime
/// of `workers` threads, as the relay load harness's `fanout` workload does; see the module
/// docs.
#[derive(Clone, Copy)]
pub(crate) struct Fanout {
	version: &'static str,
	workers: usize,
}

impl Fanout {
	const SHAPE: Shape = Shape {
		publishers: 1,
		viewers: 64,
		frame: 400,
		group_frames: 1,
		..Shape::BASE
	};

	/// Groups delivered per round: one to each viewer.
	pub(crate) fn groups(self) -> usize {
		Self::SHAPE.viewers
	}
}

impl fmt::Display for Fanout {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}/workers={}", self.version, self.workers)
	}
}

pub(crate) fn fanouts() -> Vec<Fanout> {
	[1, 16]
		.into_iter()
		.flat_map(|workers| ["moq-lite-06", "moq-lite-07-wip"].map(|version| Fanout { version, workers }))
		.collect()
}

pub(crate) fn fanning<B: Backend>(case: Fanout) -> Delivering<B> {
	Running::dropped(threaded(case.workers), Room::<B>::new(case.version, Fanout::SHAPE))
}

/// Withdraw a batch across a full mesh of `relays`, `broadcasts` at once.
#[derive(Clone, Copy)]
pub(crate) struct Withdrawal {
	relays: usize,
	broadcasts: usize,
}

impl fmt::Display for Withdrawal {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}r_{}b", self.relays, self.broadcasts)
	}
}

/// Sweeping both peers and prefixes.
pub(crate) fn withdrawals() -> Vec<Withdrawal> {
	[4, 8, 16]
		.into_iter()
		.flat_map(|relays| [1, 32].map(|broadcasts| Withdrawal { relays, broadcasts }))
		.collect()
}

/// A mesh with every broadcast announced to the last relay, ready to withdraw them.
pub(crate) struct Withdrawing<B: Backend> {
	announced: origin::Routes,
	live: Vec<origin::Route>,
	broadcasts: usize,
	_cluster: Cluster<B>,
}

/// Paused protocol time lets the quiet-point timeout drain pending work without including a
/// real wait in the measurement.
pub(crate) fn withdrawing<B: Backend>(case: Withdrawal) -> Running<Withdrawing<B>> {
	let Withdrawal { relays, broadcasts } = case;
	Running::dropped(runtime(), async move {
		tokio::time::pause();
		let shape = Shape {
			relays,
			publishers: 0,
			..Shape::BASE
		};
		let cluster = Cluster::<B>::new("moq-lite-06", shape, false).await;
		let mut announced = B::origin(cluster.relays.last().unwrap()).consume().routes();
		let live: Vec<_> = (0..broadcasts)
			.map(|i| {
				B::origin(&cluster.relays[i % (relays - 1)])
					.route(path(i), origin::RouteOptions::default())
					.unwrap()
			})
			.collect();
		while kio::clock::timeout(Duration::from_secs(1), announced.next())
			.await
			.is_ok()
		{}
		Withdrawing {
			announced,
			live,
			broadcasts,
			_cluster: cluster,
		}
	})
}

/// Withdraw every broadcast and wait until the last relay saw each go.
pub(crate) fn withdraw<B: Backend>(mut fixture: Running<Withdrawing<B>>) -> Running<Withdrawing<B>> {
	let (runtime, fixture_ref) = fixture.parts();
	runtime.block_on(async {
		drop(std::mem::take(&mut fixture_ref.live));
		let mut retracted = 0;
		while let Ok(Some(event)) = kio::clock::timeout(Duration::from_secs(1), fixture_ref.announced.next()).await {
			assert!(
				matches!(
					event,
					moq_net::origin::RouteEvent {
						kind: moq_net::origin::RouteKind::End,
						..
					}
				),
				"{event:?}"
			);
			retracted += 1;
		}
		assert_eq!(retracted, fixture_ref.broadcasts);
	});
	fixture
}

/// Stamp the scenarios both the model and the relay run as benchmarks on `$backend`, a
/// [`Backend`] in scope where this expands.
macro_rules! benches {
	($backend:ident) => {
		$crate::session::benches!(@delivery $backend, session_delivery_compression, delivery_compression, bytes = bytes);
		$crate::session::benches!(@delivery $backend, session_delivery_publishers, delivery_publishers, bytes = bytes);
		$crate::session::benches!(@delivery $backend, session_delivery_viewers, delivery_viewers, bytes = bytes);
		$crate::session::benches!(@delivery $backend, session_delivery_paced, delivery_paced, bytes = bytes);
		$crate::session::benches!(@delivery $backend, session_delivery_scale, delivery_scale, bytes = bytes);
		$crate::session::benches!(@delivery $backend, session_delivery_broadcasts, delivery_broadcasts, bytes = bytes);
		$crate::session::benches!(@delivery $backend, session_delivery_watch, delivery_watch, bytes = bytes);
		$crate::session::benches!(@delivery $backend, session_delivery_prefixes, delivery_prefixes, bytes = bytes);
		$crate::session::benches!(@delivery $backend, session_delivery_frame, delivery_frame, bytes = bytes);
		$crate::session::benches!(@delivery $backend, session_delivery_burst, delivery_burst, bytes = bytes);
		$crate::session::benches!(@delivery $backend, session_delivery_credit, delivery_credit, bytes = bytes);
		$crate::session::benches!(@delivery $backend, session_delivery_relays, delivery_relays, bytes = bytes);
		$crate::session::benches!(@delivery $backend, session_delivery_group_cost, delivery_group_cost, bytes = bytes);
		$crate::session::benches!(@delivery $backend, session_delivery_datagram, delivery_datagram, items = objects);
		$crate::session::benches!(@delivery $backend, session_delivery_fetch_frames, delivery_fetch_frames, bytes = bytes);
		$crate::session::benches!(@delivery $backend, session_delivery_fetch_range, delivery_fetch_range, bytes = bytes);

		$crate::session::benches!(@join $backend, session_join_broadcasts, join_broadcasts);
		$crate::session::benches!(@join $backend, session_join_stale, join_stale);
		$crate::session::benches!(@join $backend, session_join_relays, join_relays);

		#[swole::bench(
			args = $crate::session::announces(),
			setup = $crate::session::announcing::<$backend>
		)]
		fn session_announce(fixture: &mut $crate::session::Running<$crate::session::Announcing<$backend>>) {
			$crate::session::announce(fixture);
		}

		$crate::session::benches!(@churn $backend, session_churn_sessions, churn_sessions);
		$crate::session::benches!(@churn $backend, session_churn_held, churn_held);

		$crate::session::benches!(@dash $backend, session_dash_projects, dash_projects);
		$crate::session::benches!(@dash $backend, session_dash_tracks, dash_tracks);
		$crate::session::benches!(@dash $backend, session_dash_relays, dash_relays);

		#[swole::bench(
			args = $crate::session::withdrawals(),
			setup = $crate::session::withdrawing::<$backend>
		)]
		fn session_withdrawal(
			fixture: $crate::session::Running<$crate::session::Withdrawing<$backend>>,
		) -> $crate::session::Running<$crate::session::Withdrawing<$backend>> {
			$crate::session::withdraw(fixture)
		}
	};
	(@delivery $backend:ident, $name:ident, $cases:ident, $unit:ident = $count:ident) => {
		#[swole::bench(
			args = $crate::session::$cases(),
			setup = $crate::session::delivering::<$backend>,
			before = $crate::session::tick::<$backend>,
			$unit = $crate::session::Case::$count
		)]
		fn $name(room: &mut $crate::session::Delivering<$backend>) {
			$crate::session::round(room);
		}
	};
	(@join $backend:ident, $name:ident, $cases:ident) => {
		#[swole::bench(
			args = $crate::session::$cases(),
			setup = $crate::session::joining::<$backend>,
			after = $crate::session::leave::<$backend>
		)]
		fn $name(fixture: &mut $crate::session::Running<$crate::session::Joining<$backend>>) {
			$crate::session::join(fixture);
		}
	};
	(@churn $backend:ident, $name:ident, $cases:ident) => {
		#[swole::bench(
			args = $crate::session::$cases(),
			setup = $crate::session::churning::<$backend>,
			items = |_| 1
		)]
		fn $name(room: &mut $crate::session::Running<$crate::session::ChurnRoom<$backend>>) {
			$crate::session::churn(room);
		}
	};
	(@dash $backend:ident, $name:ident, $cases:ident) => {
		#[swole::bench(
			args = $crate::session::$cases(),
			setup = $crate::session::dashing::<$backend>,
			items = $crate::session::Dashing::total
		)]
		fn $name(room: &mut $crate::session::Running<$crate::session::DashRoom<$backend>>) {
			$crate::session::dash(room);
		}
	};
}
pub(crate) use benches;
