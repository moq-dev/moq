//! Frame throughput over the in-memory transport: one publisher session sending to one
//! subscriber session, swept over the streams in flight (one group per track) and the
//! frame size.
//!
//! Each round writes one group of [`FRAMES`] frames on every track, then reads them all
//! back. The payload crosses as the written allocation, so the time per byte should
//! fall as frames grow, and a copy would show as bytes allocated per byte holding steady
//! rather than falling as the frame size grows.
//!
//! Everything runs on one current-thread runtime, so the only work measured is the
//! protocol, model, and transport code, never scheduling across threads.
//!
//! Run with `cargo bench -p moq-net --features async --bench memory`.

use std::fmt;

use bytes::Bytes;
use moq_net::{Client, Server, Timestamp, Version, cache, origin, track, transport::memory};
use swole::bench;

const VERSION: &str = "moq-lite-06";

/// Frames per group, so per-frame and per-group costs both appear.
const FRAMES: usize = 4;

const SIZES: [usize; 4] = [64, 16 * 1024, 1024 * 1024, 4 * 1024 * 1024];
const STREAMS: [usize; 3] = [1, 16, 128];

/// The most payload one round may write across every stream, so the largest frames skip
/// the widest sweep rather than hold gigabytes.
const ROUND_BYTES: usize = 512 * 1024 * 1024;

/// Streams in flight and bytes per frame, named `streams=<streams>/size=<size>`.
#[derive(Clone, Copy)]
struct Case {
	streams: usize,
	size: usize,
}

impl Case {
	/// Payload bytes one round writes and reads.
	fn bytes(self) -> usize {
		self.streams * FRAMES * self.size
	}
}

impl fmt::Display for Case {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "streams={}/size={}", self.streams, self.size)
	}
}

/// Every case to measure.
fn cases() -> Vec<Case> {
	STREAMS
		.into_iter()
		.flat_map(|streams| SIZES.into_iter().map(move |size| Case { streams, size }))
		.filter(|case| case.bytes() <= ROUND_BYTES)
		.collect()
}

fn runtime() -> tokio::runtime::Runtime {
	tokio::runtime::Builder::new_current_thread()
		.enable_time()
		.build()
		.unwrap()
}

fn now() -> moq_net::time::Instant {
	kio::clock::now()
}

/// One publisher and one subscriber session over an in-memory pair.
struct Link {
	pools: Vec<cache::Pool>,
	cache_now: moq_net::time::Instant,
	tracks: Vec<track::Producer>,
	subscribers: Vec<track::Subscriber>,
	payload: Bytes,
	/// The next group's start on every track, in milliseconds.
	next: u64,
	_broadcast: origin::Route,
	_sessions: (moq_net::Session, moq_net::Session),
	_origins: (origin::Producer, origin::Producer),
}

impl Link {
	async fn new(streams: usize, size: usize) -> Self {
		let mut pools = Vec::new();
		let version: Version = VERSION.parse().unwrap();
		let mut origin = |hop: u64| {
			let mut config = origin::Config::new(std::num::NonZeroU64::new(hop).unwrap());
			config.pool = cache::Pool::new(cache::Config::default().with_expiry(cache::DEFAULT_EXPIRY));
			pools.push(config.pool.clone());
			let (producer, driver) = origin::Producer::new(config);
			tokio::spawn(moq_net::time::run(driver));
			producer
		};
		let publisher = origin(1);
		let subscriber = origin(2);

		let mut config = memory::Config::default();
		config.protocol = Some(version.alpn().to_string());
		let (client, server) = memory::pair(config);
		let client_config = Client::new()
			.with_versions(version.into())
			.with_publisher(publisher.consume());
		let server_config = Server::new()
			.with_versions(version.into())
			.with_subscriber(subscriber.clone());
		// Each driver runs as soon as its handshake resolves, since the server's waits on
		// the client's driver.
		let sessions = futures::join!(
			async {
				let (session, driver) = client_config.connect(now(), client).await.unwrap();
				tokio::spawn(moq_net::time::run(driver));
				session
			},
			async {
				let (session, driver) = server_config.accept(now(), server).await.unwrap();
				tokio::spawn(moq_net::time::run(driver));
				session
			}
		);

		let broadcast = publisher
			.route("bench", moq_net::origin::RouteOptions::hidden())
			.unwrap();
		let info = track::Info::default()
			.with_max_age(None)
			.with_timescale(moq_net::Timescale::MILLI);
		let tracks: Vec<_> = (0..streams)
			.map(|index| {
				broadcast
					.broadcast()
					.create_track(format!("{index}"), info.clone())
					.unwrap()
			})
			.collect();
		broadcast.announce().unwrap();

		let consumer = subscriber.consume();
		let remote = consumer
			.follow("bench", None)
			.unwrap()
			.available()
			.await
			.unwrap()
			.unwrap();
		let mut subscribers = Vec::new();
		for index in 0..streams {
			let track = remote.track(&format!("{index}")).unwrap();
			subscribers.push(track.subscribe(None).await.unwrap());
		}

		let mut link = Self {
			pools,
			cache_now: now(),
			tracks,
			subscribers,
			payload: Bytes::from(vec![0; size]),
			next: 0,
			_broadcast: broadcast,
			_sessions: sessions,
			_origins: (publisher, subscriber),
		};
		// Warm every stream so the timed rounds skip first-group setup.
		for _ in 0..4 {
			link.tick();
			link.round().await;
		}
		link
	}

	/// Write a group on every track, then read each back, returning the payload bytes
	/// read so skipped work can't look like a speedup.
	async fn round(&mut self) -> usize {
		let key = self.next;
		self.next += 1;
		let timestamp = Timestamp::from_millis(key).unwrap();
		for track in &self.tracks {
			let mut group = track.append_group(Timestamp::from_millis(key).unwrap()).unwrap();
			for _ in 0..FRAMES {
				group.write_frame(timestamp, self.payload.clone()).unwrap();
			}
			group.finish().unwrap();
		}

		let mut bytes = 0;
		for subscriber in &mut self.subscribers {
			let mut group = subscriber.recv_group().await.unwrap().expect("track ended");
			while let Some(frame) = group.read_frame().await.unwrap() {
				bytes += frame.payload.len();
			}
		}
		bytes
	}

	// Advance the cache/model clock between rounds to expire read history.
	fn tick(&mut self) {
		self.cache_now = self.cache_now.saturating_add(cache::DEFAULT_EXPIRY);
		for pool in &self.pools {
			pool.poll(self.cache_now, &kio::Waiter::noop());
		}
	}
}

/// A link and the runtime its sessions run on, declared last so it outlives them.
struct Running {
	link: Link,
	rt: tokio::runtime::Runtime,
}

fn running(case: Case) -> Running {
	let rt = runtime();
	let link = rt.block_on(Link::new(case.streams, case.size));
	Running { link, rt }
}

fn tick(running: &mut Running) {
	running.link.tick();
}

#[bench(args = cases(), setup = running, before = tick, bytes = Case::bytes)]
fn memory(running: &mut Running) -> usize {
	let expected = running.link.tracks.len() * FRAMES * running.link.payload.len();
	let bytes = running.rt.block_on(running.link.round());
	assert_eq!(bytes, expected);
	bytes
}

fn main() {
	swole::main();
}
