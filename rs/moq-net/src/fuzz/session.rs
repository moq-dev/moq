//! The session targets: a whole moq-net session, as a relay runs it, against a peer that
//! does anything, beside a bystander whose broadcast it must not disturb.
//!
//! The input's first byte picks the version, from those of the target's protocol a relay
//! accepts. The rest is a script of [`Action`]s the peer plays over the in-memory transport,
//! on the simulated clock with a fixed seed, so a failing input replays exactly.
//!
//! The relay is one origin serving three sessions: the peer, a bystander publishing a
//! broadcast, and a bystander subscribing to it. Beyond "does not panic", each run asserts:
//!
//! - The bystander's subscriber reads every group its publisher wrote, one a second, in
//!   order and byte for byte.
//! - Once the peer is gone and the clock runs past every timeout, the origin holds what it
//!   held for the bystander alone: the same pool charge, tasks, routes, cursors, and
//!   listeners. The peer's session has finished, and nothing of its transport is held.
//!
//! Unbounded work shows as libFuzzer's per-input timeout instead.

use std::{
	cell::RefCell,
	rc::Rc,
	sync::{Arc, Mutex},
	task::Poll,
	time::Duration,
};

use bytes::Bytes;
use kio::clock::sim::{self, JoinHandle};

use crate::{
	Client, Pattern, Patterns, Server, Session, Version, broadcast, cache,
	coding::{Encoder, Form},
	origin, track,
	transport::{
		memory,
		poll::{self, RecvStream as _, SendStream as _, Session as _},
	},
};

use super::Seed;

/// The simulator's seed. Fixed, so the input alone decides the run.
const SEED: u64 = 0;

/// Streams the peer may have open toward the relay at once, per direction: quinn's default.
const STREAMS: usize = 100;

/// How long the peer waits after each action, so everything it caused lands first.
const SETTLE: Duration = Duration::from_millis(1);

/// The clock a [`Action::Advance`] tick moves.
const TICK: Duration = Duration::from_millis(100);

/// How often the bystander publishes a group while the peer plays.
const PUBLISH: Duration = Duration::from_secs(1);

/// The most a script may advance the clock in all. Idle sessions still wake for their timers,
/// so hours of simulated time cost seconds that would read as a timeout the peer caused.
const ADVANCE: Duration = Duration::from_secs(600);

/// How long a cached group nobody reads stays in the relay's pool.
const EXPIRY: Duration = Duration::from_secs(1);

/// How long the clock may run once the peer leaves before the origin must be back to its
/// baseline: past every timeout the relay keeps.
const DRAIN: Duration = Duration::from_secs(300);

/// How often the drain checks whether the origin is back to its baseline.
const POLL: Duration = Duration::from_millis(250);

/// The broadcast and track names the bystander publishes, and the track the relay reads of
/// each broadcast the peer announces.
const BYSTANDER: &str = "bystander";
const TRACK: &str = "t";

/// The scope a relay's token would give the peer to publish in, so it cannot take over the
/// bystander's path: moq-net routes a path to its newest publisher by design.
const PEER: &str = "peer";

/// Drive a moq-lite session.
pub fn session_lite(data: &[u8]) -> bool {
	session(true, data)
}

/// Drive a moq-transport session.
pub fn session_ietf(data: &[u8]) -> bool {
	session(false, data)
}

/// The versions of one protocol a relay accepts.
fn versions(lite: bool) -> Vec<Version> {
	Version::ALL
		.iter()
		.copied()
		.filter(|version| version.is_lite() == lite)
		.collect()
}

/// Play `data` against a relay, returning whether the relay accepted the session.
fn session(lite: bool, data: &[u8]) -> bool {
	let versions = versions(lite);
	let Some((&index, script)) = data.split_first() else {
		return false;
	};
	let version = versions[index as usize % versions.len()];

	sim::run_seeded(SEED, async move {
		let relay = Relay::new(version).await;
		let (transport, victim) = memory::pair(transport(version));
		let victim = relay.serve(&versions, victim);

		let mut peer = Peer {
			transport,
			streams: Vec::new(),
			lite,
			form: version.into(),
			advance: ADVANCE,
		};
		let mut script = Script(script);
		// The bystander publishes on its own clock, so its groups interleave with whatever
		// the peer does, bounded by the time a script can pass rather than its length.
		let publishing = {
			let bystander = relay.bystander.clone();
			sim::spawn(async move {
				loop {
					kio::clock::sleep(PUBLISH).await;
					bystander.publish();
				}
			})
		};
		while let Some(action) = script.next() {
			peer.play(action).await;
			kio::clock::sleep(SETTLE).await;
		}
		publishing.abort();
		drop(peer);

		relay.check(victim).await
	})
}

/// The transport between a relay and one of its peers at `version`.
fn transport(version: Version) -> memory::Config {
	memory::Config {
		protocol: Some(version.alpn().to_string()),
		max_concurrent_uni_streams: Some(STREAMS),
		max_concurrent_bi_streams: Some(STREAMS),
		..Default::default()
	}
}

/// One thing the peer does.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Action {
	/// Open a uni stream toward the relay.
	OpenUni,
	/// Open a bidi stream toward the relay.
	OpenBi,
	/// Accept a uni stream the relay opened, if one is waiting.
	AcceptUni,
	/// Accept a bidi stream the relay opened, if one is waiting.
	AcceptBi,
	/// Write raw bytes to a stream.
	Write { stream: u8, data: Vec<u8> },
	/// Write a control message: for moq-lite a size-prefixed body, for moq-transport a type, a
	/// two-byte length, and the body. Framed here so a mutated body keeps its length right.
	Message { stream: u8, kind: u8, body: Vec<u8> },
	/// Finish a stream's sending half.
	Finish { stream: u8 },
	/// Reset a stream's sending half with a code.
	Reset { stream: u8, code: u8 },
	/// Stop a stream's receiving half with a code.
	Stop { stream: u8, code: u8 },
	/// Read whatever a stream's receiving half holds, which returns its credit.
	Read { stream: u8 },
	/// Send a datagram.
	Datagram { data: Vec<u8> },
	/// Let `ticks` of [`TICK`] pass, within [`ADVANCE`].
	Advance { ticks: u8 },
	/// Close the session with a code.
	Close { code: u8 },
}

impl Action {
	const KINDS: u8 = 13;

	fn encode(&self, out: &mut Vec<u8>) {
		let bytes = |out: &mut Vec<u8>, data: &[u8]| {
			let len = u16::try_from(data.len()).expect("a recorded write fits a u16");
			out.extend_from_slice(&len.to_be_bytes());
			out.extend_from_slice(data);
		};
		match self {
			Self::OpenUni => out.push(0),
			Self::OpenBi => out.push(1),
			Self::AcceptUni => out.push(2),
			Self::AcceptBi => out.push(3),
			Self::Write { stream, data } => {
				out.extend([4, *stream]);
				bytes(out, data);
			}
			Self::Message { stream, kind, body } => {
				out.extend([5, *stream, *kind]);
				bytes(out, body);
			}
			Self::Finish { stream } => out.extend([6, *stream]),
			Self::Reset { stream, code } => out.extend([7, *stream, *code]),
			Self::Stop { stream, code } => out.extend([8, *stream, *code]),
			Self::Read { stream } => out.extend([9, *stream]),
			Self::Datagram { data } => {
				out.push(10);
				bytes(out, data);
			}
			Self::Advance { ticks } => out.extend([11, *ticks]),
			Self::Close { code } => out.extend([12, *code]),
		}
	}
}

/// The fuzzer's bytes as [`Action`]s, ending at the first one cut short.
struct Script<'a>(&'a [u8]);

impl Script<'_> {
	fn byte(&mut self) -> Option<u8> {
		let (&byte, rest) = self.0.split_first()?;
		self.0 = rest;
		Some(byte)
	}

	/// A length-prefixed run of bytes, cut to what the input still holds.
	fn bytes(&mut self) -> Option<Vec<u8>> {
		let len = u16::from_be_bytes([self.byte()?, self.byte()?]) as usize;
		let (data, rest) = self.0.split_at(len.min(self.0.len()));
		self.0 = rest;
		Some(data.to_vec())
	}

	fn next(&mut self) -> Option<Action> {
		Some(match self.byte()? % Action::KINDS {
			0 => Action::OpenUni,
			1 => Action::OpenBi,
			2 => Action::AcceptUni,
			3 => Action::AcceptBi,
			4 => Action::Write {
				stream: self.byte()?,
				data: self.bytes()?,
			},
			5 => Action::Message {
				stream: self.byte()?,
				kind: self.byte()?,
				body: self.bytes()?,
			},
			6 => Action::Finish { stream: self.byte()? },
			7 => Action::Reset {
				stream: self.byte()?,
				code: self.byte()?,
			},
			8 => Action::Stop {
				stream: self.byte()?,
				code: self.byte()?,
			},
			9 => Action::Read { stream: self.byte()? },
			10 => Action::Datagram { data: self.bytes()? },
			11 => Action::Advance { ticks: self.byte()? },
			_ => Action::Close { code: self.byte()? },
		})
	}
}

/// The peer's end of the transport, and every stream it opened or accepted, in that order.
struct Peer {
	transport: memory::Session,
	streams: Vec<Stream>,
	lite: bool,
	form: Form,
	/// What remains of [`ADVANCE`].
	advance: Duration,
}

#[derive(Default)]
struct Stream {
	send: Option<memory::SendStream>,
	recv: Option<memory::RecvStream>,
}

impl Peer {
	/// Do `action` without waiting: whatever the transport cannot take at once is skipped,
	/// as a peer that stalls would.
	async fn play(&mut self, action: Action) {
		let waiter = kio::Waiter::noop();
		let opened = |send, recv| Stream { send, recv };
		match action {
			Action::OpenUni => {
				if let Poll::Ready(Ok(send)) = self.transport.poll_open_uni(&waiter, Default::default()) {
					self.streams.push(opened(Some(send), None));
				}
			}
			Action::OpenBi => {
				if let Poll::Ready(Ok((send, recv))) = self.transport.poll_open_bi(&waiter, Default::default()) {
					self.streams.push(opened(Some(send), Some(recv)));
				}
			}
			Action::AcceptUni => {
				if let Poll::Ready(Ok(recv)) = self.transport.poll_accept_uni(&waiter) {
					self.streams.push(opened(None, Some(recv)));
				}
			}
			Action::AcceptBi => {
				if let Poll::Ready(Ok((send, recv))) = self.transport.poll_accept_bi(&waiter) {
					self.streams.push(opened(Some(send), Some(recv)));
				}
			}
			Action::Write { stream, data } => self.write(stream, &data),
			Action::Message { stream, kind, body } => {
				let mut data = Vec::new();
				let mut encoder = Encoder::new(&mut data, self.form);
				let framed = match self.lite {
					true => encoder.varint(body.len() as u64),
					false => encoder.varint(kind as u64),
				};
				framed.expect("a small varint encodes");
				if !self.lite {
					data.extend_from_slice(&(body.len() as u16).to_be_bytes());
				}
				data.extend_from_slice(&body);
				self.write(stream, &data);
			}
			Action::Finish { stream } => {
				if let Some(send) = self.stream(stream).and_then(|stream| stream.send.as_mut()) {
					let _ = send.finish();
				}
			}
			Action::Reset { stream, code } => {
				if let Some(send) = self.stream(stream).and_then(|stream| stream.send.as_mut()) {
					send.reset(code as u32);
				}
			}
			Action::Stop { stream, code } => {
				if let Some(recv) = self.stream(stream).and_then(|stream| stream.recv.as_mut()) {
					recv.stop(code as u32);
				}
			}
			Action::Read { stream } => {
				if let Some(recv) = self.stream(stream).and_then(|stream| stream.recv.as_mut()) {
					while let Poll::Ready(Ok(Some(_))) = recv.poll_read_chunk(&waiter, usize::MAX) {}
				}
			}
			Action::Datagram { data } => {
				let _ = self.transport.send_datagram(data.into(), Default::default());
			}
			Action::Advance { ticks } => {
				let advance = self.advance.min(TICK * ticks as u32);
				self.advance -= advance;
				kio::clock::sleep(advance).await;
			}
			Action::Close { code } => self.transport.close(code as u32, ""),
		}
	}

	/// The stream `index` picks, wrapping, if the peer has any.
	fn stream(&mut self, index: u8) -> Option<&mut Stream> {
		let count = self.streams.len();
		self.streams.get_mut(index as usize % count.max(1))
	}

	fn write(&mut self, stream: u8, mut data: &[u8]) {
		let Some(send) = self.stream(stream).and_then(|stream| stream.send.as_mut()) else {
			return;
		};
		while !data.is_empty() {
			match send.poll_write(&kio::Waiter::noop(), data) {
				Poll::Ready(Ok(written)) => data = &data[written..],
				_ => break,
			}
		}
	}
}

/// Run a protocol driver until it ends.
fn spawn<D: crate::time::Driver + Unpin + 'static>(mut driver: D) -> JoinHandle<()> {
	sim::spawn(async move {
		kio::clock::drive(move |now, waiter| driver.poll(now, waiter)).await;
	})
}

/// A client and server session over one in-memory pair, both driven.
async fn connect(client: Client, server: Server, version: Version) -> [Session; 2] {
	let (client_transport, server_transport) = memory::pair(transport(version));
	// Each driver runs as soon as its handshake resolves: a SETUP only reaches the wire once
	// its side's driver is polled.
	let (client, server) = futures::join!(
		async {
			let (session, driver) = client
				.connect(kio::clock::now(), client_transport)
				.await
				.expect("the bystander's client handshake failed");
			spawn(driver);
			session
		},
		async {
			let (session, driver) = server
				.accept(kio::clock::now(), server_transport)
				.await
				.expect("the bystander's server handshake failed");
			spawn(driver);
			session
		},
	);
	[client, server]
}

/// A broadcast published to the relay over one session and read back over another.
struct Bystander {
	track: track::Producer,
	sent: RefCell<Vec<Vec<Bytes>>>,
	read: Rc<RefCell<Vec<Vec<Bytes>>>>,
	_route: origin::Route,
	_sessions: [Session; 4],
	_reader: JoinHandle<()>,
}

impl Bystander {
	async fn new(relay: &origin::Producer, version: Version) -> Self {
		let server = || {
			Server::new()
				.with_versions(version.into())
				.with_publisher(relay.consume())
				.with_subscriber(relay.clone())
		};

		let publisher = produce(2);
		let route = publisher
			.route(BYSTANDER, origin::RouteOptions::default())
			.expect("the bystander's route");
		// No max age, so no group is superseded: the reader must get every one.
		let track = route
			.broadcast()
			.create_track(TRACK, track::Info::default().with_max_age(None))
			.expect("the bystander's track");
		route.announce().expect("the bystander's announce");
		let client = Client::new()
			.with_versions(version.into())
			.with_publisher(publisher.consume());
		let [a, b] = connect(client, server(), version).await;

		let subscriber = produce(3);
		let client = Client::new()
			.with_versions(version.into())
			.with_subscriber(subscriber.clone());
		let [c, d] = connect(client, server(), version).await;

		let read = Rc::<RefCell<Vec<Vec<Bytes>>>>::default();
		let reader = sim::spawn(read_track(
			subscriber.consume(),
			crate::Path::from_static(BYSTANDER),
			read.clone(),
		));

		let bystander = Self {
			track,
			sent: Default::default(),
			read,
			_route: route,
			_sessions: [a, b, c, d],
			_reader: reader,
		};
		bystander.publish();
		for _ in 0..1000 {
			if bystander.delivered() {
				return bystander;
			}
			kio::clock::sleep(SETTLE).await;
		}
		panic!("the bystander's first group never arrived without a peer");
	}

	/// Write the next group: two frames of the same size every time, so groups charge the
	/// cache alike, holding the key so a misplaced one shows.
	fn publish(&self) {
		let mut sent = self.sent.borrow_mut();
		let key = sent.len() as u64;
		let mut group = self.track.append_group().expect("the bystander's group");
		let mut frames = Vec::new();
		for frame in 0..2u8 {
			let mut payload = key.to_be_bytes().repeat(8);
			payload.push(frame);
			let payload = Bytes::from(payload);
			group.write_frame(None, payload.clone()).expect("the bystander's frame");
			frames.push(payload);
		}
		group.finish().expect("the bystander's group finish");
		sent.push(frames);
	}

	fn delivered(&self) -> bool {
		*self.read.borrow() == *self.sent.borrow()
	}
}

/// Subscribe to [`TRACK`] of the broadcast at `path`, recording each group's frames.
async fn read_track(origin: origin::Consumer, path: crate::Path, read: Rc<RefCell<Vec<Vec<Bytes>>>>) {
	let Ok(mut following) = origin.follow(path, None) else {
		return;
	};
	let Ok(Some(broadcast)) = following.available().await else {
		return;
	};
	read_broadcast(broadcast, read).await;
}

async fn read_broadcast(broadcast: broadcast::Consumer, read: Rc<RefCell<Vec<Vec<Bytes>>>>) {
	let Ok(track) = broadcast.track(TRACK) else {
		return;
	};
	// Never skip a superseded group, so every one the publisher wrote must arrive.
	let every = track::Subscription::default().with_delay(track::Delay::default().with_max(None));
	let Ok(mut subscriber) = track.subscribe(every).await else {
		return;
	};
	while let Ok(Some(mut group)) = subscriber.recv_group().await {
		let mut frames = Vec::new();
		while let Ok(Some(frame)) = group.read_frame().await {
			frames.push(frame.payload.clone());
		}
		read.borrow_mut().push(frames);
	}
}

/// An origin with a fixed hop, so the run does not depend on a random one.
fn produce(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(hop.try_into().expect("a nonzero hop")));
	spawn(driver);
	producer
}

/// What the relay's origin holds.
#[derive(Debug, PartialEq, Eq)]
struct Held {
	pool: u64,
	origin: origin::Footprint,
}

fn held(origin: &origin::Producer, driver: &RefCell<origin::Driver>) -> Held {
	Held {
		pool: origin.config().pool.used(),
		origin: driver.borrow().footprint(),
	}
}

/// The relay under test: one origin, with the bystander already flowing through it.
struct Relay {
	version: Version,
	origin: origin::Producer,
	driver: Rc<RefCell<origin::Driver>>,
	bystander: Rc<Bystander>,
	baseline: Held,
	_driving: JoinHandle<()>,
}

/// The relay's session with the peer, and what the relay runs alongside it.
struct Victim {
	task: JoinHandle<()>,
	accepted: Rc<std::cell::Cell<bool>>,
	held: memory::Held,
	/// Reads every broadcast the peer announces, as the relay's other sessions would.
	watcher: JoinHandle<()>,
	readers: Rc<RefCell<Vec<JoinHandle<()>>>>,
}

impl Relay {
	async fn new(version: Version) -> Self {
		let mut config = origin::Config::new(1.try_into().expect("a nonzero hop"));
		// The cache's idle expiry is the longest wait in the drain, so keep it short.
		config.pool = cache::Pool::new(cache::Config::default().with_expiry(EXPIRY));
		let (origin, driver) = origin::Producer::new(config);
		// A relay carries every peer's routes on to the others.
		let origin = origin.with_proxy(true);
		let driver = Rc::new(RefCell::new(driver));
		let driving = {
			let driver = driver.clone();
			sim::spawn(async move {
				kio::clock::drive(move |now, waiter| driver.borrow_mut().poll(now, waiter)).await;
			})
		};
		let bystander = Rc::new(Bystander::new(&origin, version).await);
		let baseline = held(&origin, &driver);
		Self {
			version,
			origin,
			driver,
			bystander,
			baseline,
			_driving: driving,
		}
	}

	fn held(&self) -> Held {
		held(&self.origin, &self.driver)
	}

	/// Accept the peer's session as a relay would: it may publish only beneath [`PEER`], and
	/// subscribe to anything.
	fn serve(&self, versions: &[Version], transport: memory::Session) -> Victim {
		let publish = self
			.origin
			.scope(PEER, &Patterns::from(Pattern::all()))
			.expect("the peer's scope");
		let server = Server::new()
			.with_versions(versions.to_vec().into())
			.with_publisher(self.origin.consume())
			.with_subscriber(publish);

		let held = transport.held();
		let accepted = Rc::new(std::cell::Cell::new(false));
		let task = {
			let accepted = accepted.clone();
			sim::spawn(async move {
				let Ok((session, driver)) = server.accept(kio::clock::now(), transport).await else {
					return;
				};
				accepted.set(true);
				let mut driver = driver;
				futures::join!(
					async move {
						session.closed().await;
					},
					kio::clock::drive(move |now, waiter| crate::time::Driver::poll(&mut driver, now, waiter)),
				);
			})
		};

		let readers = Rc::<RefCell<Vec<JoinHandle<()>>>>::default();
		let watcher = {
			let readers = readers.clone();
			let origin = self.origin.consume();
			sim::spawn(async move {
				let mut routes = origin.routes();
				while let Some(event) = routes.next().await {
					let path = event.announce.prefix;
					// A restart is a new instance at the path, which the reader before it never
					// follows onto.
					let started = matches!(event.kind, origin::RouteKind::Start | origin::RouteKind::Restart);
					if !started || !path.as_str().starts_with(PEER) {
						continue;
					}
					let read = Rc::default();
					readers
						.borrow_mut()
						.push(sim::spawn(read_track(origin.clone(), path, read)));
				}
			})
		};

		Victim {
			task,
			accepted,
			held,
			watcher,
			readers,
		}
	}

	/// Once the peer has left, run the clock until the relay holds what it did for the
	/// bystander alone, then check the bystander read everything.
	async fn check(self, victim: Victim) -> bool {
		victim.watcher.abort();
		for reader in victim.readers.borrow().iter() {
			reader.abort();
		}
		self.bystander.publish();

		let deadline = kio::clock::now().saturating_add(DRAIN);
		loop {
			let held = self.held();
			let done = victim.task.is_finished() && !victim.held.session() && victim.held.streams() == 0;
			let delivered = self.bystander.read.borrow().len() >= self.bystander.sent.borrow().len();
			if done && delivered && held == self.baseline {
				break;
			}
			assert!(
				kio::clock::now() < deadline,
				"{:?}: {DRAIN:?} after the peer left, the relay holds {held:?} rather than {:?}, \
				 its session finished: {}, its transport held: {}, streams toward it: {}, \
				 the bystander read {} of {} groups",
				self.version,
				self.baseline,
				victim.task.is_finished(),
				victim.held.session(),
				victim.held.streams(),
				self.bystander.read.borrow().len(),
				self.bystander.sent.borrow().len(),
			);
			kio::clock::sleep(POLL).await;
		}

		let sent = self.bystander.sent.borrow();
		let read = self.bystander.read.borrow();
		assert!(
			*read == *sent,
			"{:?}: the bystander read {} groups, not the {} it published, or not as published",
			self.version,
			read.len(),
			sent.len(),
		);

		victim.accepted.get()
	}
}

/// One way a well-behaved client uses the relay, recorded as a seed.
#[derive(Clone, Copy)]
enum Scenario {
	/// Announce a broadcast and serve a group to the relay's reader.
	Publish,
	/// Subscribe to the bystander's broadcast and read a group.
	Subscribe,
	/// Fetch one of the bystander's groups.
	Fetch,
}

/// A transcript per version and [`Scenario`], recorded from a real client, so the fuzzer
/// starts deep in the session instead of at SETUP. The kind is the version's index.
pub(super) fn seeds() -> Vec<Seed> {
	let mut seeds = Vec::new();
	for (target, lite) in [("session_lite", true), ("session_ietf", false)] {
		for (index, version) in versions(lite).into_iter().enumerate() {
			for scenario in [Scenario::Publish, Scenario::Subscribe, Scenario::Fetch] {
				let mut data = vec![index as u8];
				for action in record(version, scenario) {
					action.encode(&mut data);
				}
				seeds.push(Seed {
					target,
					kind: index as u8,
					data,
				});
			}
		}
	}
	seeds
}

/// Run `scenario` as a real client against a relay, returning what it did on the wire.
fn record(version: Version, scenario: Scenario) -> Vec<Action> {
	sim::run_seeded(SEED, async move {
		let relay = Relay::new(version).await;
		let (transport, victim) = memory::pair(transport(version));
		let victim = relay.serve(&versions(version.is_lite()), victim);

		let log = Arc::new(Mutex::new(Log {
			actions: Vec::new(),
			streams: 0,
			at: kio::clock::now(),
		}));
		let recorder = Recorder {
			inner: transport,
			log: log.clone(),
		};

		let local = produce(4);
		let client = Client::new()
			.with_versions(version.into())
			.with_publisher(local.consume())
			.with_subscriber(local.clone());
		let timeout = Duration::from_secs(1);
		if let Ok((session, driver)) = client.connect(kio::clock::now(), recorder).await {
			let driving = spawn(driver);
			match scenario {
				Scenario::Publish => {
					let route = local
						.route("a", origin::RouteOptions::default())
						.expect("the client's route");
					let mut track = route
						.broadcast()
						.create_track(TRACK, track::Info::default())
						.expect("the client's track");
					route.announce().expect("the client's announce");
					let subscribed = async { while let Ok(None) = track.subscription_changed().await {} };
					let _ = kio::clock::timeout(timeout, subscribed).await;
					let mut group = track.append_group().expect("the client's group");
					group.write_frame(None, Bytes::from_static(b"frame")).expect("a frame");
					group.finish().expect("the client's group finish");
					kio::clock::sleep(timeout).await;
				}
				Scenario::Subscribe => {
					let read = Rc::default();
					let reading = read_track(local.consume(), crate::Path::from_static(BYSTANDER), read);
					let _ = kio::clock::timeout(timeout, reading).await;
				}
				Scenario::Fetch => {
					let fetch = async {
						let mut following = local.consume().follow(BYSTANDER, None).ok()?;
						let broadcast = following.available().await.ok()??;
						let track = broadcast.track(TRACK).ok()?;
						let mut group = track.fetch_group(0, None).await.ok()?;
						while let Ok(Some(_)) = group.read_frame().await {}
						Some(())
					};
					let _ = kio::clock::timeout(timeout, fetch).await;
				}
			}
			drop(session);
			driving.abort();
		}

		relay.check(victim).await;

		std::mem::take(&mut log.lock().expect("the log").actions)
	})
}

/// What a [`Recorder`] has seen, as the [`Action`]s that replay it.
struct Log {
	actions: Vec<Action>,
	/// Streams opened or accepted so far, which is the next one's index.
	streams: usize,
	/// When the last [`Action::Advance`] left the clock.
	at: kio::clock::Instant,
}

impl Log {
	fn push(&mut self, action: Action) {
		let ticks = kio::clock::now().saturating_duration_since(self.at).as_nanos() / TICK.as_nanos();
		if ticks > 0 {
			let ticks = ticks.min(u8::MAX as u128) as u8;
			self.actions.push(Action::Advance { ticks });
			self.at = self.at.saturating_add(TICK * ticks as u32);
		}
		// Consecutive reads of one stream replay as one.
		if matches!(action, Action::Read { .. }) && self.actions.last() == Some(&action) {
			return;
		}
		self.actions.push(action);
	}

	/// Record a new stream, returning its index.
	fn stream(&mut self, action: Action) -> u8 {
		self.push(action);
		let index = self.streams as u8;
		self.streams += 1;
		index
	}
}

type Shared = Arc<Mutex<Log>>;

fn log(log: &Shared) -> std::sync::MutexGuard<'_, Log> {
	log.lock().expect("the recorder's log")
}

/// A transport that records what a well-behaved client does on it.
struct Recorder {
	inner: memory::Session,
	log: Shared,
}

/// A stream half that records what its client does with it.
struct Recorded<S> {
	inner: S,
	stream: u8,
	log: Shared,
}

impl poll::Session for Recorder {
	type SendStream = Recorded<memory::SendStream>;
	type RecvStream = Recorded<memory::RecvStream>;
	type Error = memory::Error;

	fn poll_accept_uni(&self, waiter: &kio::Waiter) -> Poll<Result<Self::RecvStream, Self::Error>> {
		self.inner.poll_accept_uni(waiter).map_ok(|inner| Recorded {
			inner,
			stream: log(&self.log).stream(Action::AcceptUni),
			log: self.log.clone(),
		})
	}

	fn poll_accept_bi(&self, waiter: &kio::Waiter) -> Poll<Result<poll::BiStreams<Self>, Self::Error>> {
		self.inner.poll_accept_bi(waiter).map_ok(|(send, recv)| {
			let stream = log(&self.log).stream(Action::AcceptBi);
			self.pair(stream, send, recv)
		})
	}

	fn poll_open_uni(
		&self,
		waiter: &kio::Waiter,
		options: poll::SendOptions<'_, poll::Ungrouped>,
	) -> Poll<Result<Self::SendStream, Self::Error>> {
		self.inner.poll_open_uni(waiter, options).map_ok(|inner| Recorded {
			inner,
			stream: log(&self.log).stream(Action::OpenUni),
			log: self.log.clone(),
		})
	}

	fn poll_open_bi(
		&self,
		waiter: &kio::Waiter,
		options: poll::SendOptions<'_, poll::Ungrouped>,
	) -> Poll<Result<poll::BiStreams<Self>, Self::Error>> {
		self.inner.poll_open_bi(waiter, options).map_ok(|(send, recv)| {
			let stream = log(&self.log).stream(Action::OpenBi);
			self.pair(stream, send, recv)
		})
	}

	fn send_group(&self, _: i64) -> Option<poll::Ungrouped> {
		None
	}

	fn send_datagram(
		&self,
		payload: Bytes,
		options: poll::SendOptions<'_, poll::Ungrouped>,
	) -> Result<(), Self::Error> {
		log(&self.log).push(Action::Datagram { data: payload.to_vec() });
		self.inner.send_datagram(payload, options)
	}

	fn poll_recv_datagram(&self, waiter: &kio::Waiter) -> Poll<Result<Bytes, Self::Error>> {
		self.inner.poll_recv_datagram(waiter)
	}

	fn max_datagram_size(&self) -> usize {
		self.inner.max_datagram_size()
	}

	fn protocol(&self) -> Option<&str> {
		self.inner.protocol()
	}

	fn address(&self) -> crate::transport::Address {
		self.inner.address()
	}

	fn close(&self, code: u32, reason: &str) {
		log(&self.log).push(Action::Close { code: code as u8 });
		self.inner.close(code, reason);
	}

	fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Self::Error> {
		self.inner.poll_closed(waiter)
	}

	fn receive_window(&self) -> Option<crate::transport::ReceiveWindow> {
		self.inner.receive_window()
	}

	fn export_keying_material(&self, label: &[u8], context: &[u8]) -> Option<[u8; 32]> {
		self.inner.export_keying_material(label, context)
	}
}

impl Recorder {
	fn pair(
		&self,
		stream: u8,
		send: memory::SendStream,
		recv: memory::RecvStream,
	) -> (Recorded<memory::SendStream>, Recorded<memory::RecvStream>) {
		let log = || self.log.clone();
		(
			Recorded {
				inner: send,
				stream,
				log: log(),
			},
			Recorded {
				inner: recv,
				stream,
				log: log(),
			},
		)
	}
}

impl poll::SendStream for Recorded<memory::SendStream> {
	type Error = memory::Error;
	type Group = poll::Ungrouped;

	fn poll_write(&mut self, waiter: &kio::Waiter, buf: &[u8]) -> Poll<Result<usize, Self::Error>> {
		let written = std::task::ready!(self.inner.poll_write(waiter, buf))?;
		log(&self.log).push(Action::Write {
			stream: self.stream,
			data: buf[..written].to_vec(),
		});
		Poll::Ready(Ok(written))
	}

	fn set_priority(&mut self, priority: i64) {
		self.inner.set_priority(priority);
	}

	fn set_group(&mut self, group: &Self::Group) {
		self.inner.set_group(group);
	}

	fn finish(&mut self) -> Result<(), Self::Error> {
		log(&self.log).push(Action::Finish { stream: self.stream });
		self.inner.finish()
	}

	fn reset(&mut self, code: u32) {
		log(&self.log).push(Action::Reset {
			stream: self.stream,
			code: code as u8,
		});
		self.inner.reset(code);
	}

	fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Result<(), Self::Error>> {
		self.inner.poll_closed(waiter)
	}

	fn acknowledged(&self) -> Option<bool> {
		self.inner.acknowledged()
	}
}

impl poll::RecvStream for Recorded<memory::RecvStream> {
	type Error = memory::Error;
	type Lease<'a> = <memory::RecvStream as poll::RecvStream>::Lease<'a>;

	fn poll_lease(&mut self, waiter: &kio::Waiter, want: usize) -> Poll<Result<Self::Lease<'_>, Self::Error>> {
		let lease = std::task::ready!(self.inner.poll_lease(waiter, want));
		log(&self.log).push(Action::Read { stream: self.stream });
		Poll::Ready(lease)
	}

	fn set_priority(&mut self, priority: i64) {
		self.inner.set_priority(priority);
	}

	fn stop(&mut self, code: u32) {
		log(&self.log).push(Action::Stop {
			stream: self.stream,
			code: code as u8,
		});
		self.inner.stop(code);
	}

	fn poll_reset(&mut self, waiter: &kio::Waiter) -> Poll<Self::Error> {
		self.inner.poll_reset(waiter)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A run is decided by its input alone, so a transcript recorded twice is the same bytes,
	/// and a failing input replays exactly.
	#[test]
	fn recording_replays() {
		let record = || seeds().into_iter().map(|seed| seed.data).collect::<Vec<_>>();
		assert_eq!(record(), record());
	}
}
