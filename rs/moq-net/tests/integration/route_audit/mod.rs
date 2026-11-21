#![cfg(feature = "async")]

//! The cases a 2026-11-15 audit of lite-07's route layer found neither a model nor a test
//! for, on real sessions with time mocked.
//!
//! Every case reads like an application: a publisher writes one group of one frame every
//! [`CADENCE`], and a [`Reader`] on a relay follows the path, reads each group, and
//! follows again when its broadcast ends. What a failure costs is its [`Report`]: the
//! longest the reader went without a new group past the cadence, and whatever it missed,
//! read twice, or read from another publisher's stream within one broadcast. The PR that
//! added these reports each number; nothing here judges how long is too long.

mod contest;
mod domains;
mod failures;
mod ingest;

use crate::path_routes::{DEBOUNCE, HOLD, cut, land, lite07};
use crate::route_change::{produce_origin, timed};
use crate::support;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{
	Arc,
	atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use futures::channel::mpsc;
use moq_net::{Instance, Timestamp, Version, group, origin, track};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

/// How often a publisher writes a group.
pub(super) const CADENCE: Duration = Duration::from_millis(100);

/// The path every case publishes.
pub(super) const PATH: &str = "live";

/// The track every case reads.
const TRACK: &str = "video";

/// How a link between two relays is configured.
#[derive(Clone, Copy, Debug)]
pub(super) struct Join {
	/// What the dialer prices the link at.
	pub cost: u64,
	/// How the dialer marks the link.
	pub link: origin::Link,
	/// The acceptor confines the dialer as a local client, so the session carries claims
	/// across a scoped grant rather than routes.
	pub scoped: bool,
}

impl Join {
	/// A routing link between peers at `cost`.
	pub fn peer(cost: u64) -> Self {
		Self {
			cost,
			link: origin::Link::Peer,
			scoped: false,
		}
	}
}

/// One session between two relays.
struct Link {
	/// The dialer, then the acceptor.
	ends: [usize; 2],
	cost: u64,
	pair: MockPair,
}

/// Relays by index and the sessions between them.
pub(super) struct Mesh {
	pub relays: Vec<origin::Producer>,
	links: Vec<Link>,
	/// What each closed session carried: the uni streams relay `from` opened to relay `to`.
	closed: Vec<(usize, usize, Vec<Vec<u8>>)>,
}

impl Mesh {
	/// `count` relays, numbered from hop 100 so endpoints keep the low hops.
	pub fn new(count: usize) -> Self {
		Self {
			relays: (0..count as u64).map(|i| produce_origin(100 + i)).collect(),
			links: Vec::new(),
			closed: Vec::new(),
		}
	}

	/// Relay `a` dials relay `b` over lite-07, configured by `join`.
	pub async fn join(&mut self, a: usize, b: usize, join: Join) {
		let dialer = self.relays[a].clone().with_link(join.link);
		let acceptor = match join.scoped {
			true => self.relays[b].clone(),
			false => self.relays[b].clone().with_link(origin::Link::Peer),
		};
		let mut options = MockConnectOptions::new(lite07());
		options.client_publish = Some(dialer.consume().with_hidden(true));
		options.client_subscribe = Some(dialer);
		options.server_publish = Some(acceptor.consume().with_hidden(true));
		options.server_subscribe = Some(acceptor);
		options.cost = Some(join.cost);
		let pair = connect_mock(options).await;
		self.links.push(Link {
			ends: [a, b],
			cost: join.cost,
			pair,
		});
	}

	/// The session between `a` and `b`, either way round.
	pub fn link(&self, a: usize, b: usize) -> &MockPair {
		let link = (self.links.iter())
			.find(|link| link.ends == [a, b] || link.ends == [b, a])
			.unwrap_or_else(|| panic!("no link {a}-{b}"));
		&link.pair
	}

	/// Relay `relay`'s end of its session with `other`.
	pub fn session(&self, relay: usize, other: usize) -> &moq_net::Session {
		let pair = self.link(relay, other);
		let dialed = self.links.iter().any(|link| link.ends == [relay, other]);
		if dialed { &pair.client } else { &pair.server }
	}

	/// Close the session between `a` and `b` at both ends, and forget it.
	pub fn cut(&mut self, a: usize, b: usize) {
		self.close(|link| link.ends == [a, b] || link.ends == [b, a]);
	}

	/// Relay `relay` dies: every session it holds closes at once.
	pub fn kill(&mut self, relay: usize) {
		self.close(|link| link.ends.contains(&relay));
	}

	/// Close every session `which` picks at both ends, keeping what it carried.
	fn close(&mut self, which: impl Fn(&Link) -> bool) {
		let (closed, open) = std::mem::take(&mut self.links).into_iter().partition(which);
		self.links = open;
		for link in closed {
			cut(&link.pair);
			self.closed.extend(Self::carried(&link));
		}
	}

	/// The uni streams each direction of `link` opened so far, as `(from, to, streams)`.
	fn carried(link: &Link) -> [(usize, usize, Vec<Vec<u8>>); 2] {
		let [a, b] = link.ends;
		let streams = |transport: &crate::support::mock::MockSession| {
			(0..=u8::MAX).flat_map(|kind| transport.uni_writes(kind)).collect()
		};
		[
			(a, b, streams(&link.pair.client_transport)),
			(b, a, streams(&link.pair.server_transport)),
		]
	}

	/// The relays at the far end of each link `relay` holds.
	pub fn neighbours(&self, relay: usize) -> Vec<usize> {
		(self.links.iter())
			.filter_map(|link| match link.ends {
				[a, b] if a == relay => Some(b),
				[a, b] if b == relay => Some(a),
				_ => None,
			})
			.collect()
	}

	/// What each relay pays to reach `from` over the routing links up now: Dijkstra over
	/// the link costs, `None` where nothing reaches.
	pub fn distances(&self, from: usize) -> Vec<Option<u64>> {
		let mut distance = vec![None; self.relays.len()];
		let mut queue = std::collections::BinaryHeap::from([std::cmp::Reverse((0, from))]);
		while let Some(std::cmp::Reverse((cost, relay))) = queue.pop() {
			if distance[relay].is_some() {
				continue;
			}
			distance[relay] = Some(cost);
			for link in &self.links {
				let next = match link.ends {
					[a, b] if a == relay => b,
					[a, b] if b == relay => a,
					_ => continue,
				};
				if distance[next].is_none() {
					queue.push(std::cmp::Reverse((cost + link.cost, next)));
				}
			}
		}
		distance
	}

	/// Every group that crossed one direction of a link more than once, over every session
	/// between the two relays so far, as `(from, to, group, times)`. A group is known by its
	/// frame, `{tag}:{key}`, in the group streams the sending relay opened.
	pub fn doubled(&self) -> Vec<(usize, usize, String, usize)> {
		let open = self.links.iter().flat_map(Self::carried);
		let mut times = BTreeMap::<_, usize>::new();
		for (from, to, streams) in self.closed.iter().cloned().chain(open) {
			for stream in streams {
				for group in groups(&stream) {
					*times.entry((from, to, group)).or_default() += 1;
				}
			}
		}
		(times.into_iter())
			.filter(|(_, times)| *times > 1)
			.map(|((from, to, group), times)| (from, to, group, times))
			.collect()
	}
}

/// The groups `stream` carries, each known by its [`Feed`] frame's `{tag}:{key}`.
fn groups(stream: &[u8]) -> BTreeSet<String> {
	let digits = |from: usize| (stream[from..].iter()).take_while(|byte| byte.is_ascii_digit()).count();
	let mut found = BTreeSet::new();
	for start in 0..stream.len() {
		if start > 0 && stream[start - 1].is_ascii_uppercase() {
			continue;
		}
		let tag = (stream[start..].iter())
			.take_while(|byte| byte.is_ascii_uppercase())
			.count();
		let key = start + tag + 1;
		if tag == 0 || stream.get(key - 1) != Some(&b':') || digits(key) == 0 {
			continue;
		}
		let end = key + digits(key);
		if stream.get(end) == Some(&b':') && digits(end + 1) > 0 {
			found.insert(String::from_utf8_lossy(&stream[start..end]).into_owned());
		}
	}
	found
}

/// An endpoint publishing [`PATH`] with one [`TRACK`], pushed to relays as a client.
pub(super) struct Source {
	pub origin: origin::Producer,
	pub route: origin::Route,
	pub track: track::Producer,
}

impl Source {
	/// Publish [`PATH`] on a fresh origin `hop`, under `instance` at epoch 0 when given, or a
	/// minted one.
	pub fn new(hop: u64, instance: Option<&Instance>) -> Self {
		Self::on(produce_origin(hop), PATH, instance)
	}

	/// Publish `path` on `origin`, under `instance` at epoch 0 when given.
	pub fn on(origin: origin::Producer, path: &str, instance: Option<&Instance>) -> Self {
		let mut options = origin::RouteOptions::hidden();
		if let Some(instance) = instance {
			options = options.with_instance(instance.clone(), 0);
		}
		let route = origin.route(path, options).unwrap();
		let track = route.broadcast().create_track(TRACK, timed()).unwrap();
		route.announce().unwrap();
		Self { origin, route, track }
	}

	/// Push everything this source publishes to `relay` over `version`, priced `cost`.
	pub async fn push(&self, relay: &origin::Producer, version: Version, cost: u64) -> MockPair {
		let mut options = MockConnectOptions::new(version);
		options.client_publish = Some(self.origin.consume());
		options.server_subscribe = Some(relay.clone());
		options.cost = Some(cost);
		connect_mock(options).await
	}
}

/// A writer adding a group to its tracks every [`CADENCE`] until dropped.
pub(super) struct Feed {
	stop: Arc<AtomicBool>,
}

impl Feed {
	/// Write group `k` at `k` cadences from now, a frame tagged `tag:k`, to every one of
	/// `tracks`: one encoder, or synchronized encoders writing the same bytes.
	pub fn start(tracks: Vec<track::Producer>, tag: &'static str) -> Self {
		let stop = Arc::new(AtomicBool::new(false));
		let stopped = stop.clone();
		kio::clock::sim::spawn(async move {
			let mut key = 0u64;
			while !stopped.load(Ordering::Relaxed) {
				let start = key * CADENCE.as_millis() as u64;
				for track in &tracks {
					let mut group = track.create_group(group::Info { start }).unwrap().unwrap();
					let frame = payload(tag, key, kio::clock::now());
					group
						.write_frame(Timestamp::from_millis(start).unwrap(), frame)
						.unwrap();
					group.finish().unwrap();
				}
				key += 1;
				kio::clock::sleep(CADENCE).await;
			}
		});
		Self { stop }
	}
}

impl Drop for Feed {
	fn drop(&mut self) {
		self.stop.store(true, Ordering::Relaxed);
	}
}

/// A frame naming its writer, its group, and when it was written.
fn payload(tag: &str, key: u64, written: kio::clock::Instant) -> Vec<u8> {
	let micros = written.saturating_duration_since(kio::clock::Instant::ZERO).as_micros();
	format!("{tag}:{key}:{micros}").into_bytes()
}

/// One frame a reader got.
#[derive(Clone, Debug)]
pub(super) struct Got {
	pub at: kio::clock::Instant,
	/// Which broadcast the reader was on, counting from 1: a new one is a restart.
	pub run: usize,
	pub instance: Option<Instance>,
	pub key: u64,
	pub tag: String,
	/// When the publisher wrote it.
	pub written: kio::clock::Instant,
}

/// An application reading [`PATH`] on one node, following the next broadcast whenever
/// its track ends.
pub(super) struct Reader {
	rx: mpsc::UnboundedReceiver<Got>,
	got: Vec<Got>,
}

impl Reader {
	pub fn new(node: &origin::Producer) -> Self {
		let (tx, rx) = mpsc::unbounded();
		let consumer = node.consume();
		kio::clock::sim::spawn(async move {
			let mut follow = consumer.follow(PATH, None).unwrap();
			let mut run = 0;
			loop {
				let broadcast = match follow.available().await {
					Ok(Some(broadcast)) => broadcast,
					// Offline: the next call waits for whatever serves next.
					Ok(None) => continue,
					// Nothing here refuses, so the origin closed.
					Err(_) => return,
				};
				run += 1;
				let instance = broadcast.info().instance.clone();
				// From the live edge, and never skipping a group however late.
				let prefs = track::Subscription::default().with_delay(
					track::Delay::default()
						.with_min(Duration::ZERO)
						.with_max(Duration::from_secs(60)),
				);
				let Ok(mut sub) = broadcast.track(TRACK).unwrap().subscribe(prefs).await else {
					continue;
				};
				while let Ok(Some(mut group)) = sub.recv_group().await {
					while let Ok(Some(frame)) = group.read_frame().await {
						let text = String::from_utf8(frame.payload.to_vec()).unwrap();
						let [tag, key, micros] = text.splitn(3, ':').collect::<Vec<_>>()[..] else {
							panic!("a frame this module wrote: {text}");
						};
						let micros = Duration::from_micros(micros.parse().unwrap());
						let got = Got {
							at: kio::clock::now(),
							run,
							instance: instance.clone(),
							key: key.parse().unwrap(),
							tag: tag.to_string(),
							written: kio::clock::Instant::ZERO.saturating_add(micros),
						};
						if tx.unbounded_send(got).is_err() {
							return;
						}
					}
				}
			}
		});
		Self { rx, got: Vec::new() }
	}

	/// Every frame read so far.
	pub fn got(&mut self) -> &[Got] {
		while let Ok(got) = self.rx.try_recv() {
			self.got.push(got);
		}
		&self.got
	}

	/// What the reader saw from `since` on.
	pub fn report(&mut self, since: kio::clock::Instant) -> Report {
		let now = kio::clock::now();
		let got = self.got().to_vec();
		let mut report = Report::default();
		let before = got.iter().rev().find(|got| got.at < since);
		report.runs.extend(before.map(|got| got.run));
		let first = before.map(|got| got.run);
		let mut last = before.map_or(since, |got| got.at);
		let mut longest = Duration::ZERO;
		for got in got.iter().filter(|got| got.at >= since) {
			longest = longest.max(got.at.saturating_duration_since(last));
			// A restart reads the new broadcast's backlog, which says nothing of the failure.
			if first.is_none_or(|run| run == got.run) {
				report.lag = report.lag.max(got.at.saturating_duration_since(got.written));
			}
			last = got.at;
			report.after += 1;
		}
		longest = longest.max(now.saturating_duration_since(last));
		report.stall = longest.saturating_sub(CADENCE);
		report
			.runs
			.extend(got.iter().filter(|got| got.at >= since).map(|got| got.run));
		let settled = now.checked_sub(HOLD + DEBOUNCE).unwrap_or(since).max(since);
		report.current = got.iter().any(|got| got.written >= settled);
		for pair in got.windows(2) {
			let [before, after] = [&pair[0], &pair[1]];
			if before.run != after.run {
				continue;
			}
			match after.key.cmp(&(before.key + 1)) {
				std::cmp::Ordering::Less => report.repeated.push(after.key),
				std::cmp::Ordering::Greater => report.missed.extend(before.key + 1..after.key),
				std::cmp::Ordering::Equal => {}
			}
			if before.tag != after.tag || before.instance != after.instance {
				report.spliced.push(after.key);
			}
		}
		report
	}

	/// The tag of the last frame read.
	pub fn tag(&mut self) -> Option<String> {
		self.got().last().map(|got| got.tag.clone())
	}
}

/// What a reader saw across a failure.
#[derive(Clone, Debug, Default)]
pub(super) struct Report {
	/// The longest it went without a new group, past the cadence.
	pub stall: Duration,
	/// The longest a group read since took from its publisher, within the broadcast it had.
	pub lag: Duration,
	/// Frames read since the failure.
	pub after: usize,
	/// It read a group written after the last failure. Every case reports right after
	/// [`quiesce`](crate::path_routes::quiesce) following its last failure, so this is a
	/// group written in the [`HOLD`] and [`DEBOUNCE`] before the report.
	pub current: bool,
	/// The broadcasts it read from the last before the failure on: more than one is a restart.
	pub runs: BTreeSet<usize>,
	/// Groups skipped within one broadcast.
	pub missed: Vec<u64>,
	/// Groups read again within one broadcast.
	pub repeated: Vec<u64>,
	/// Groups read within one broadcast from another publisher or Instance.
	pub spliced: Vec<u64>,
}

impl Report {
	/// Read on across the failure in one broadcast, every group once and from one stream.
	pub fn seamless(&self) -> bool {
		self.current && self.runs.len() == 1 && self.clean()
	}

	/// Nothing missed, repeated, or spliced within a broadcast.
	pub fn clean(&self) -> bool {
		self.missed.is_empty() && self.repeated.is_empty() && self.spliced.is_empty()
	}
}

/// A route a node shows at [`PATH`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Shown {
	pub available: bool,
	pub cost: u64,
	pub instance: Instance,
}

/// What `node` shows at [`PATH`] now, or `None` when it shows nothing.
pub(super) async fn shown(node: &origin::Producer) -> Option<Shown> {
	shown_at(node, PATH).await
}

/// What `node` shows at `path` now, or `None` when it shows nothing.
pub(super) async fn shown_at(node: &origin::Producer, path: &str) -> Option<Shown> {
	let mut routes = node.consume().routes();
	land().await;
	let mut shown = None;
	while let Some(event) = routes.try_next() {
		if event.announce.prefix.as_str() != path {
			continue;
		}
		let route = &event.announce.route;
		shown = match event.kind {
			origin::RouteKind::Start | origin::RouteKind::Update | origin::RouteKind::Restart => Some(Shown {
				available: route.available(),
				cost: route.cost().value(),
				instance: route.instance().clone(),
			}),
			origin::RouteKind::End | origin::RouteKind::Offline => None,
		};
	}
	shown
}

/// One change a node showed at [`PATH`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Seen {
	pub kind: support::Kind,
	pub available: bool,
	pub instance: Instance,
	pub at: kio::clock::Instant,
}

/// Every change a node shows at [`PATH`] past what it shows now.
pub(super) struct Watch {
	rx: mpsc::UnboundedReceiver<Seen>,
}

impl Watch {
	pub async fn new(node: &origin::Producer) -> Self {
		let mut routes = node.consume().routes();
		let (tx, rx) = mpsc::unbounded();
		kio::clock::sim::spawn(async move {
			while let Some(event) = routes.next().await {
				if event.announce.prefix.as_str() != PATH {
					continue;
				}
				let kind = match event.kind {
					origin::RouteKind::Start => support::Kind::Start,
					origin::RouteKind::Update => support::Kind::Update,
					origin::RouteKind::Restart => support::Kind::Restart,
					origin::RouteKind::End => support::Kind::End,
					origin::RouteKind::Offline => support::Kind::Offline,
				};
				let seen = Seen {
					kind,
					available: event.announce.route.available(),
					instance: event.announce.route.instance().clone(),
					at: kio::clock::now(),
				};
				if tx.unbounded_send(seen).is_err() {
					break;
				}
			}
		});
		let mut watch = Self { rx };
		land().await;
		watch.seen();
		watch
	}

	/// What the node showed since the last call.
	pub fn seen(&mut self) -> Vec<Seen> {
		std::iter::from_fn(|| self.rx.try_recv().ok()).collect()
	}
}

/// Bufferbloat on `pair`: its queue deepens evenly to `peak` over as long, then overflows,
/// and the session drops.
pub(super) async fn bloat(pair: &MockPair, peak: Duration) {
	const STEPS: u32 = 10;
	for step in 1..=STEPS {
		pair.client_transport.set_latency(peak * step / STEPS);
		kio::clock::sleep(peak / STEPS).await;
	}
	cut(pair);
}

/// Each reader's [`Report`] from `since` on, all printed under `case` before any is judged.
pub(super) fn reports(readers: &mut [Reader], since: kio::clock::Instant, case: &str) -> Vec<Report> {
	let reports: Vec<_> = readers.iter_mut().map(|reader| reader.report(since)).collect();
	for (i, report) in reports.iter().enumerate() {
		measured(&format!("{case}, reader R{i}"), report);
	}
	reports
}

/// Print a case's measurements, which the PR reports.
pub(super) fn measured(case: &str, report: &Report) {
	eprintln!(
		"measured {case}: stall {:?}, lag {:?}, runs {:?}, missed {:?}, repeated {:?}, spliced {:?}",
		report.stall, report.lag, report.runs, report.missed, report.repeated, report.spliced
	);
}

/// A ring of `count` relays, each linked to the next at cost 1.
pub(super) async fn ring(count: usize) -> Mesh {
	let mut mesh = Mesh::new(count);
	for i in 0..count {
		mesh.join(i, (i + 1) % count, Join::peer(1)).await;
	}
	mesh
}

/// A reader on every relay of `mesh`, each past its first group.
pub(super) async fn readers(mesh: &Mesh) -> Vec<Reader> {
	let readers = mesh.relays.iter().map(Reader::new).collect();
	kio::clock::sleep(CADENCE * 10).await;
	readers
}
