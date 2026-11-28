#![cfg(feature = "async")]

//! Failover measured on a live broadcast while next hops change, on lite-07.
//!
//! A publisher writes a frame every [`STEP`] while relays lose and gain the sessions its
//! route crosses. Eventual route recovery alone does not show a move is seamless, so each
//! fixture asserts on the frames its readers get as well as on what their route streams
//! show, and reports what the moves cost on the wire: per-path retractions (End or
//! Offline), re-announcements (Start or Restart after the first), and the announce-stream
//! messages each session sent, which carry every ROUTE, DEAD, and claim batch.
//!
//! The mesh fixtures port moq-dev/moq#4644's live-graph withdrawal and failover tests,
//! which ran on lite-06 under a hold-down, to lite-07's route rule.

use crate::WithMax as _;
use crate::mesh_withdraw::{PRODUCTION, PRODUCTION_LATENCY_MS};
use crate::path_routes::{DEBOUNCE, HOLD, lite07};
use crate::route_change::{payload, produce_origin, timed};
use crate::support;
use crate::support::EveryRange;

use std::collections::BTreeMap;
use std::time::Duration;

use futures::channel::mpsc;
use moq_net::{Error, Prefixes, Timestamp, group, origin, stats, track};
use support::Kind;
use support::harness::{MockConnectOptions, MockPair, connect_mock, peer_with_latency};

/// How often a publisher writes a frame.
const STEP: Duration = Duration::from_millis(20);

/// How often a publisher writes a frame to the mesh's 33 readers: often enough that a
/// reader's gap shows, rarely enough that the mesh fixtures stay cheap.
const MESH_STEP: Duration = Duration::from_millis(100);

/// Frames per group.
const FRAMES: u64 = 5;

/// One change a node showed at a path: how long after the start, its kind, and whether a
/// usable route reached the publisher.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Seen {
	at: Duration,
	kind: Kind,
	available: bool,
}

/// Every change a node shows at a path, from now on.
struct Watch {
	rx: mpsc::UnboundedReceiver<Seen>,
}

impl Watch {
	fn new(node: &origin::Producer, path: &str, start: kio::clock::Instant) -> Self {
		let path = path.to_string();
		let mut routes = node.consume().routes();
		let (tx, rx) = mpsc::unbounded();
		kio::clock::sim::spawn(async move {
			while let Some(event) = routes.next().await {
				if event.announce.prefix.as_str() != path {
					continue;
				}
				let kind = match event.kind {
					origin::RouteKind::Start => Kind::Start,
					origin::RouteKind::Update => Kind::Update,
					origin::RouteKind::Restart => Kind::Restart,
					origin::RouteKind::End => Kind::End,
					origin::RouteKind::Offline => Kind::Offline,
					kind => panic!("unexpected route kind {kind:?}"),
				};
				let seen = Seen {
					at: kio::clock::now().saturating_duration_since(start),
					kind,
					available: event.announce.route.available(),
				};
				if tx.unbounded_send(seen).is_err() {
					break;
				}
			}
		});
		Self { rx }
	}

	/// What the node showed since the last call.
	fn seen(&mut self) -> Vec<Seen> {
		std::iter::from_fn(|| self.rx.try_recv().ok()).collect()
	}
}

/// What a route stream showed of one path: retractions, re-announcements, updates, and the
/// longest stretch it went without a usable route.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Churn {
	retractions: usize,
	reannouncements: usize,
	updates: usize,
	dark: Duration,
}

impl Churn {
	/// Tally `seen`, which follows a moment the path was shown available, until `end`.
	fn of(seen: &[Seen], end: Duration) -> Self {
		let mut churn = Self::default();
		let mut dark_since = None;
		for seen in seen {
			match seen.kind {
				Kind::End | Kind::Offline => churn.retractions += 1,
				Kind::Start | Kind::Restart => churn.reannouncements += 1,
				Kind::Update => churn.updates += 1,
			}
			let live = seen.available && !matches!(seen.kind, Kind::End | Kind::Offline);
			match (live, dark_since) {
				(false, None) => dark_since = Some(seen.at),
				(true, Some(since)) => {
					churn.dark = churn.dark.max(seen.at - since);
					dark_since = None;
				}
				_ => {}
			}
		}
		if let Some(since) = dark_since {
			churn.dark = churn.dark.max(end.saturating_sub(since));
		}
		churn
	}
}

/// A broadcast's `video` track, written one frame every step, each payload naming the
/// frame's index.
struct Feed {
	track: track::Producer,
	/// How often it writes a frame.
	step: Duration,
	group: Option<group::Producer>,
	/// When each frame was written, by index, since the start.
	written: Vec<Duration>,
	start: kio::clock::Instant,
}

impl Feed {
	fn new(track: track::Producer, start: kio::clock::Instant, step: Duration) -> Self {
		Self {
			track,
			step,
			group: None,
			written: Vec::new(),
			start,
		}
	}

	/// Write `count` frames, one every step.
	async fn run(&mut self, count: u64) {
		for _ in 0..count {
			let index = self.written.len() as u64;
			let timestamp = Timestamp::from_millis(index * self.step.as_millis() as u64).unwrap();
			let frame = payload(index / FRAMES, index % FRAMES);
			match self.group.as_mut() {
				Some(group) if !index.is_multiple_of(FRAMES) => group.write_frame(timestamp, frame).unwrap(),
				_ => {
					if let Some(group) = self.group.take() {
						group.finish().unwrap();
					}
					// Each group starts at its first frame.
					self.group = Some(self.track.append_group(timestamp, frame).unwrap());
				}
			}
			self.written
				.push(kio::clock::now().saturating_duration_since(self.start));
			kio::clock::sleep(self.step).await;
		}
	}
}

/// A frame a reader got, by index, and when; or the error that ended its subscription.
type Got = moq_net::Result<(u64, Duration)>;

/// Read every frame of `video` at `path` through `node`, following it from the start.
async fn reader(node: &origin::Producer, path: &str, start: kio::clock::Instant) -> mpsc::UnboundedReceiver<Got> {
	reader_of(node, path, "video", start).await
}

/// Read every frame of `name` at `path` through `node`, following it from the start in
/// order: a reader joining with a backlog asks for it oldest first, as [`Liveness`] judges.
async fn reader_of(
	node: &origin::Producer,
	path: &str,
	name: &str,
	start: kio::clock::Instant,
) -> mpsc::UnboundedReceiver<Got> {
	let remote = node.consume().request(path, None).await.unwrap();
	let prefs = track::Subscription::default()
		.with_max(Duration::from_secs(60))
		.ordered(track::Order::Ascending);
	let mut sub = remote.track(name).unwrap().subscribe(prefs).await.unwrap();
	let (tx, rx) = mpsc::unbounded();
	kio::clock::sim::spawn(async move {
		let _keep = remote;
		loop {
			let mut group = match sub.recv_group().await {
				Ok(Some(group)) => group,
				Ok(None) => return,
				Err(err) => {
					let _ = tx.unbounded_send(Err(err));
					return;
				}
			};
			loop {
				match group.read_frame().await {
					Ok(Some(frame)) => {
						let at = kio::clock::now().saturating_duration_since(start);
						let text = String::from_utf8(frame.payload.to_vec()).unwrap();
						let (key, index) = text.split_once('.').unwrap();
						let index = key.parse::<u64>().unwrap() * FRAMES + index.parse::<u64>().unwrap();
						if tx.unbounded_send(Ok((index, at))).is_err() {
							return;
						}
					}
					Ok(None) => break,
					Err(err) => {
						let _ = tx.unbounded_send(Err(err));
						return;
					}
				}
			}
		}
	});
	rx
}

/// What a reader got of a feed: frames it missed, frames out of order or twice, the error
/// that ended it, and the longest any frame took to reach it.
#[derive(Debug, Default)]
struct Liveness {
	lost: u64,
	misordered: u64,
	ended: Option<Error>,
	worst: Duration,
}

impl Liveness {
	fn of(rx: &mut mpsc::UnboundedReceiver<Got>, feed: &Feed) -> Self {
		let mut live = Self::default();
		let mut next = 0;
		while let Ok(got) = rx.try_recv() {
			match got {
				Ok((index, at)) => {
					match index.cmp(&next) {
						std::cmp::Ordering::Less => live.misordered += 1,
						std::cmp::Ordering::Equal => next += 1,
						std::cmp::Ordering::Greater => {
							live.lost += index - next;
							next = index + 1;
						}
					}
					live.worst = live.worst.max(at.saturating_sub(feed.written[index as usize]));
				}
				Err(err) => live.ended = Some(err),
			}
		}
		live.lost += feed.written.len() as u64 - next;
		live
	}

	/// Every frame arrived, once and in order, and the subscription stands.
	fn seamless(&self) -> bool {
		self.lost == 0 && self.misordered == 0 && self.ended.is_none()
	}
}

/// Have `to` pull everything `from` publishes, over a link `to` prices at `cost`, counting
/// what `from` sends under `name`.
struct Links {
	registry: stats::Registry,
}

impl Links {
	fn new() -> Self {
		Self {
			registry: stats::Registry::new(Default::default()),
		}
	}

	fn options(&self, name: &'static str, from: &origin::Producer, to: &origin::Producer) -> MockConnectOptions {
		let mut options = MockConnectOptions::new(lite07());
		options.server_publish = Some(from.consume());
		options.client_subscribe = Some(to.clone());
		options.server_stats = self
			.registry
			.tier(stats::Tier::new(moq_net::Path::from_static(name)))
			.session(stats::SessionInfo::new(moq_net::Path::from_static("")));
		options
	}

	async fn link(&self, name: &'static str, from: &origin::Producer, to: &origin::Producer, cost: u64) -> MockPair {
		let mut options = self.options(name, from, to);
		options.cost = Some(cost);
		connect_mock(options).await
	}

	/// Frames each link sent, by name.
	fn frames(&self) -> BTreeMap<String, u64> {
		let mut frames = BTreeMap::new();
		for (tier, role, traffic) in self.registry.snapshot().traffic() {
			if role == stats::Role::Publisher {
				*frames.entry(tier.as_str().to_string()).or_default() += traffic.frames;
			}
		}
		frames
	}

	/// Track subscriptions each link served, by name.
	fn subscriptions(&self) -> BTreeMap<String, u64> {
		let mut subscriptions = BTreeMap::new();
		for (tier, role, traffic) in self.registry.snapshot().traffic() {
			if role == stats::Role::Publisher {
				*subscriptions.entry(tier.as_str().to_string()).or_default() += traffic.subscriptions_started;
			}
		}
		subscriptions
	}

	/// Track subscriptions each link serves now, by name.
	fn open(&self) -> BTreeMap<String, u64> {
		let mut open = BTreeMap::new();
		for (tier, role, traffic) in self.registry.snapshot().traffic() {
			if role == stats::Role::Publisher {
				*open.entry(tier.as_str().to_string()).or_default() += traffic.active_subscriptions();
			}
		}
		open
	}

	/// Control messages of every kind each link sent, by name.
	fn control(&self) -> BTreeMap<String, u64> {
		let mut sent = BTreeMap::new();
		for (tier, _, totals) in self.registry.snapshot().control() {
			*sent.entry(tier.as_str().to_string()).or_default() += totals.tx_messages;
		}
		sent
	}

	/// Announce-stream messages each link sent, by name.
	fn sent(&self) -> BTreeMap<String, u64> {
		let mut sent = BTreeMap::new();
		for (tier, class, totals) in self.registry.snapshot().control() {
			if class == stats::Message::Announce {
				*sent.entry(tier.as_str().to_string()).or_default() += totals.tx_messages;
			}
		}
		sent
	}
}

/// What each link sent between `before` and `after`.
fn delta(before: &BTreeMap<String, u64>, after: &BTreeMap<String, u64>) -> BTreeMap<String, u64> {
	(after.iter())
		.map(|(name, count)| (name.clone(), count - before.get(name).copied().unwrap_or(0)))
		.filter(|(_, count)| *count > 0)
		.collect()
}

/// Close a session at both ends, as a dropped connection does.
fn cut(pair: &MockPair) {
	pair.server.abort(Error::Cancel);
	pair.client.abort(Error::Cancel);
}

/// How long the moves in [`next_hop_moves_keep_a_live_reader`] take to land: past the new
/// next hop's upstream latency both ways.
const MOVE: u64 = 75;

/// `P` publishes `live` to `A` and `B`, and the relay `R` pulls both, preferring `A`. A
/// reader on `D`, one hop past `R`, follows `live` while `R`'s route to `P` moves four
/// times between next hops, each a move to a usable route with the same identity:
///
/// 1. `C` joins, cheaper than `A`, though its own upstream to `P` lags 150 ms each way,
///    so its ROUTE reaches `R` well after `A`'s;
/// 2. `C`'s session to `R` closes, and `R` falls back to `A`, whose path avoids `C`;
/// 3. `A`'s session to `R` closes, and `R` falls back to `B`;
/// 4. `A` reconnects, and `R` moves back to it.
///
/// Proves the reader gets every frame once, in order, with its subscription intact; that
/// `R` and `D` each show the moves as updates in place, never a retraction, a restart, or
/// an unavailable route; and that each move costs `D` one ANNOUNCE_UPDATE.
#[kio::test]
async fn next_hop_moves_keep_a_live_reader() {
	let links = Links::new();
	let [p, a, b, c, r, d] = [1, 2, 3, 4, 5, 6].map(produce_origin);
	let broadcast = p.route("live", origin::RouteOptions::hidden()).unwrap();
	let track = broadcast.broadcast().create_track("video", timed()).unwrap();
	broadcast.announce().unwrap();
	let _p_a = links.link("p-a", &p, &a, 1).await;
	let _p_b = links.link("p-b", &p, &b, 1).await;
	let a_r = links.link("a-r", &a, &r, 10).await;
	let _b_r = links.link("b-r", &b, &r, 100).await;
	let _r_d = links.link("r-d", &r, &d, 1).await;
	kio::clock::sleep(Duration::from_secs(1)).await;

	let start = kio::clock::now();
	let mut at_r = Watch::new(&r, "live", start);
	let mut at_d = Watch::new(&d, "live", start);
	let mut feed = Feed::new(track, start, STEP);
	let mut rx = reader(&d, "live", start).await;
	feed.run(MOVE).await;
	let before = links.sent();

	let mut options = links.options("p-c", &p, &c);
	options.latency = Duration::from_millis(150);
	let _p_c = connect_mock(options).await;
	let c_r = links.link("c-r", &c, &r, 1).await;
	feed.run(MOVE).await;
	cut(&c_r);
	feed.run(MOVE).await;
	cut(&a_r);
	feed.run(MOVE).await;
	let _a_r = links.link("a-r2", &a, &r, 10).await;
	feed.run(MOVE).await;
	kio::clock::sleep(Duration::from_secs(1)).await;

	let sent = delta(&before, &links.sent());
	let live = Liveness::of(&mut rx, &feed);
	let end = kio::clock::now().saturating_duration_since(start);
	let (seen_r, seen_d) = (at_r.seen(), at_d.seen());
	let (churn_r, churn_d) = (Churn::of(&seen_r[1..], end), Churn::of(&seen_d[1..], end));
	println!("next hop moves: reader {live:?}; R {churn_r:?}; D {churn_d:?}; sent {sent:?}");

	assert!(live.seamless(), "{live:?}");
	for (node, seen) in [("R", &seen_r), ("D", &seen_d)] {
		assert_eq!(seen[0].kind, Kind::Start, "{node}: {seen:?}");
		assert!(
			seen[1..].iter().all(|seen| seen.kind == Kind::Update && seen.available),
			"{node}: {seen:?}"
		);
		assert_eq!(seen.len(), 5, "{node}: one update per move: {seen:?}");
	}
	// `D` is told the claim, so each move is one ANNOUNCE_UPDATE and nothing else.
	assert_eq!(sent.get("r-d"), Some(&4), "one ANNOUNCE_UPDATE per move: {sent:?}");
	drop(broadcast);
}

/// `P` publishes `live` and `other`, and the relay `R` pulls both from `A`. `C` asks `P`
/// only for `other`, as a client does for what its readers watch, so its route to `P`
/// covers `other` alone. `C` then joins `R`, cheaper than `A`: its ROUTE to `P` reaches `R`
/// while its announce set carries no `live`. Readers on `R` and on `D`, one hop past it,
/// follow both broadcasts. Then `C`'s session to `R` closes.
///
/// Proves the new next hop moves only what it announced: `other` onto `C`, while `live`
/// stays on `A` untouched. Then `other`, served by `C`'s narrower route, fails over at once
/// to `A`'s wider one, which covers it all along: an update in place, never a retraction,
/// and every reader keeps every frame. Before the fix, `R`'s readers followed only the
/// narrowest route, so the session closing left them none until the node promoted `A`'s:
/// `R` showed `other` Offline, then Start again two seconds later, and its reader there
/// lost the subscription.
#[kio::test]
async fn a_narrower_next_hop_fails_over_to_a_wider_route() {
	const PATHS: [&str; 2] = ["live", "other"];
	let links = Links::new();
	let [p, a, c, r, d] = [1, 2, 3, 4, 5].map(produce_origin);
	let start = kio::clock::now();
	let mut feeds = Vec::new();
	let mut broadcasts = Vec::new();
	for path in PATHS {
		let broadcast = p.route(path, origin::RouteOptions::hidden()).unwrap();
		let track = broadcast.broadcast().create_track("video", timed()).unwrap();
		feeds.push(Feed::new(track, start, STEP));
		broadcast.announce().unwrap();
		broadcasts.push(broadcast);
	}
	let _p_a = links.link("p-a", &p, &a, 1).await;
	let _a_r = links.link("a-r", &a, &r, 10).await;
	let _r_d = links.link("r-d", &r, &d, 1).await;
	let mut options = links.options("p-c", &p, &c);
	options.client_discovers_all = false;
	let _p_c = connect_mock(options).await;
	let _c_reads_other = c.consume().scope("other", &Prefixes::all()).unwrap().routes();
	kio::clock::sleep(Duration::from_secs(1)).await;

	let mut watched = Vec::new();
	let mut readers = Vec::new();
	for (index, path) in PATHS.into_iter().enumerate() {
		for (name, node) in [("R", &r), ("D", &d)] {
			watched.push((path, name, Watch::new(node, path, start)));
			readers.push((index, name, reader(node, path, start).await));
		}
	}
	let run = async |feeds: &mut Vec<Feed>, count| {
		futures::future::join_all(feeds.iter_mut().map(|feed| feed.run(count))).await;
	};
	run(&mut feeds, MOVE).await;
	let before = links.sent();
	let c_r = links.link("c-r", &c, &r, 1).await;
	run(&mut feeds, MOVE).await;
	let joined = links.sent();
	cut(&c_r);
	run(&mut feeds, MOVE).await;
	kio::clock::sleep(Duration::from_secs(1)).await;
	let end = kio::clock::now().saturating_duration_since(start);

	let lives: Vec<_> = (readers.iter_mut())
		.map(|(index, node, rx)| (PATHS[*index], *node, Liveness::of(rx, &feeds[*index])))
		.collect();
	let seens: Vec<_> = (watched.iter_mut())
		.map(|(path, node, watch)| (*path, *node, watch.seen()))
		.collect();
	for ((path, node, live), (_, _, seen)) in lives.iter().zip(&seens) {
		println!(
			"narrower next hop, {path} at {node}: reader {live:?}; {:?}",
			Churn::of(&seen[1..], end)
		);
	}
	let sent = (delta(&before, &joined), delta(&joined, &links.sent()));
	println!("narrower next hop: sent joining {:?}, closing {:?}", sent.0, sent.1);

	for (path, node, live) in &lives {
		assert!(live.seamless(), "{path} at {node}: {live:?}");
	}
	for (path, node, seen) in &seens {
		assert_eq!(seen[0].kind, Kind::Start, "{path} at {node}: {seen:?}");
		assert!(
			seen[1..].iter().all(|seen| seen.kind == Kind::Update && seen.available),
			"{path} at {node}: {seen:?}"
		);
		let moves = match *path {
			// `C` never announced it, so nothing moved.
			"live" => 0,
			// Onto `C`, then back onto `A`.
			_ => 2,
		};
		assert_eq!(seen.len(), 1 + moves, "{path} at {node}: {seen:?}");
	}
	drop(broadcasts);
}

/// The publishers the mesh fixtures try, as many as `partial_mesh_withdraw_retracts_once`
/// does: each costs a round of the whole mesh, about a second of debug-build CPU for a
/// withdrawal and four for a failure.
const PUBLISHERS: [usize; 5] = [5, 13, 22, 26, 32];

/// moq.pro's 34-relay mesh on lite-07 with no link latency: each relay in [`PUBLISHERS`]
/// publishes a broadcast, then withdraws it.
///
/// Ports moq-dev/moq#4644's `live_withdraw_retracts_once`, which accepted updates before
/// the retraction while a relay stepped through paths it held. The path rule drops every
/// route through the withdrawing node at once, so nothing steps: proves every relay shows
/// the withdrawal as one retraction and nothing else. With the links' estimated latency,
/// this is `mesh_withdraw::partial_mesh_withdraw_retracts_once`.
#[kio::test]
async fn live_mesh_withdraw_retracts_once_without_latency() {
	let nodes: Vec<_> = (1..=34).map(crate::mesh_withdraw::produce_origin).collect();
	let mut pairs = Vec::new();
	for &(a, b) in PRODUCTION {
		pairs.push(peer_with_latency(lite07(), &nodes[a], &nodes[b], Duration::ZERO).await);
	}
	let mut watched: Vec<_> = nodes
		.iter()
		.map(|node| support::watch(node.consume().routes()))
		.collect();

	for publisher in PUBLISHERS {
		let name = format!("room/{publisher}");
		let broadcast = nodes[publisher]
			.route(name.clone(), origin::RouteOptions::hidden())
			.unwrap();
		broadcast.announce().unwrap();
		kio::clock::sim::idle().await;
		for (relay, watched) in watched.iter_mut().enumerate() {
			let updates = support::drain(watched);
			assert_eq!(
				updates.get(&name).map(Vec::as_slice),
				Some(&[Kind::Start][..]),
				"publisher {publisher}, relay {relay}"
			);
		}

		drop(broadcast);
		kio::clock::sim::idle().await;
		let mut hunted = Vec::new();
		for (relay, watched) in watched.iter_mut().enumerate() {
			let updates = support::drain(watched);
			match updates.get(&name) {
				Some(kinds) if kinds == &[Kind::End] => {}
				kinds => hunted.push((relay, kinds.cloned())),
			}
		}
		assert!(hunted.is_empty(), "publisher {publisher}: {hunted:?}");
	}
}

/// moq.pro's 34-relay mesh on lite-07 at its estimated latencies, a reader on every relay.
/// For each publisher in [`PUBLISHERS`], every link of its relay closes but the slowest,
/// while the broadcast stays live, and then the links come back.
///
/// Ports moq-dev/moq#4644's `live_link_failure_fails_over`, which bounded the longest any
/// relay went without a path by two hold-downs. Proves every relay serves the broadcast
/// again with at most one retraction and one re-announcement each (no hunting), and every
/// reader on a relay whose route stayed usable gets every frame, once and in order.
///
/// A relay whose own link closed moves at once onto a neighbour's
/// direct route, which may be stale too, until the news arrives. Measured over the five
/// publishers: 18 relays retracted and announced again, most within milliseconds, and the
/// readers on 3 of them lost their subscription.
///
/// A stale route resets a subscription `Unroutable` before its withdrawal arrives (3
/// readers' relays, all for publisher 22). That judges the route, not the track, so those
/// readers wait for the next route rather than end.
/// Run one publisher from [`PUBLISHERS`] against the 34-relay production mesh; split across
/// tests so nextest runs the five publishers in parallel.
async fn live_mesh_link_failure_for_publisher(publisher: usize) {
	let version = lite07();
	let nodes: Vec<_> = (1..=34).map(crate::mesh_withdraw::produce_origin).collect();
	let mut links: Vec<Option<MockPair>> = Vec::new();
	for (&(a, b), &ms) in PRODUCTION.iter().zip(PRODUCTION_LATENCY_MS) {
		let latency = Duration::from_millis(ms);
		links.push(Some(peer_with_latency(version, &nodes[a], &nodes[b], latency).await));
	}

	let name = format!("room/{publisher}");
	let start = kio::clock::now();
	let broadcast = nodes[publisher]
		.route(name.clone(), origin::RouteOptions::hidden())
		.unwrap();
	let track = broadcast.broadcast().create_track("video", timed()).unwrap();
	let mut feed = Feed::new(track, start, MESH_STEP);
	broadcast.announce().unwrap();
	kio::clock::sleep(Duration::from_secs(1)).await;
	let mut watched: Vec<_> = nodes.iter().map(|node| Watch::new(node, &name, start)).collect();
	let mut readers = Vec::new();
	for (relay, node) in nodes.iter().enumerate() {
		if relay != publisher {
			readers.push((relay, reader(node, &name, start).await));
		}
	}
	feed.run(5).await;

	// Cut every link of the publisher's relay but its slowest.
	let touching: Vec<_> = (0..PRODUCTION.len())
		.filter(|&index| PRODUCTION[index].0 == publisher || PRODUCTION[index].1 == publisher)
		.collect();
	let kept = *touching
		.iter()
		.max_by_key(|&&index| PRODUCTION_LATENCY_MS[index])
		.unwrap();
	let cut_links: Vec<_> = touching.into_iter().filter(|&index| index != kept).collect();
	for &index in &cut_links {
		cut(links[index].as_ref().unwrap());
		links[index] = None;
	}
	feed.run(((HOLD + DEBOUNCE).as_millis() / MESH_STEP.as_millis()) as u64)
		.await;
	// The last frames cross the mesh.
	kio::clock::sleep(Duration::from_secs(1)).await;
	let end = kio::clock::now().saturating_duration_since(start);

	let mut worst = Duration::ZERO;
	let mut retracting = 0;
	let mut dark = Vec::new();
	for (relay, watch) in watched.iter_mut().enumerate() {
		let seen = watch.seen();
		assert!(
			seen.first()
				.is_some_and(|seen| seen.kind == Kind::Start && seen.available),
			"publisher {publisher}, relay {relay}: {seen:?}"
		);
		let churn = Churn::of(&seen[1..], end);
		assert!(
			churn.retractions <= 1 && churn.reannouncements <= churn.retractions,
			"publisher {publisher}, relay {relay} hunted: {seen:?}"
		);
		let last = seen.last().unwrap();
		assert!(
			last.available && !matches!(last.kind, Kind::End | Kind::Offline),
			"publisher {publisher}, relay {relay}: {seen:?}"
		);
		worst = worst.max(churn.dark);
		retracting += churn.retractions;
		if seen[1..]
			.iter()
			.any(|seen| !seen.available || seen.kind == Kind::Offline)
		{
			dark.push(relay);
		}
	}
	let mut lost_readers = 0;
	for (relay, rx) in &mut readers {
		let live = Liveness::of(rx, &feed);
		if !live.seamless() {
			lost_readers += 1;
			assert!(
				dark.contains(relay),
				"publisher {publisher}: relay {relay} kept a usable route but its reader got {live:?}"
			);
		}
	}
	println!(
		"live mesh link failure for publisher {publisher}: {} relays went unavailable, {retracting} retracted and \
		 announced again, {lost_readers} readers lost their subscription, longest without a path {worst:?}",
		dark.len()
	);
}

#[kio::test]
async fn live_mesh_link_failure_keeps_presence_p5() {
	live_mesh_link_failure_for_publisher(PUBLISHERS[0]).await;
}

#[kio::test]
async fn live_mesh_link_failure_keeps_presence_p13() {
	live_mesh_link_failure_for_publisher(PUBLISHERS[1]).await;
}

#[kio::test]
async fn live_mesh_link_failure_keeps_presence_p22() {
	live_mesh_link_failure_for_publisher(PUBLISHERS[2]).await;
}

#[kio::test]
async fn live_mesh_link_failure_keeps_presence_p26() {
	live_mesh_link_failure_for_publisher(PUBLISHERS[3]).await;
}

#[kio::test]
async fn live_mesh_link_failure_keeps_presence_p32() {
	live_mesh_link_failure_for_publisher(PUBLISHERS[4]).await;
}

/// Seeded random meshes of 5 to 8 relays at random link latencies, the publisher on relay
/// 0 and a reader on every other, each cut one or two links at once. These are the seeds
/// a sweep of 400 found with a reader ending on a relay that stayed available.
///
/// In each, a relay's front moves as the news of the cut arrives, for a moment onto a
/// stale route whose next hop already lost its own: that hop refuses the fresh track, or
/// resets a copy that delivered, `Unroutable`. Proves the refusal judges the route, not the
/// track: the front waits for the next route, and no reader on a relay that stayed
/// available ends, nor waits for good on any relay.
#[kio::test]
async fn a_stale_route_refusing_a_track_ends_no_reader() {
	for seed in [13, 17, 92, 114] {
		stale_route_case(seed).await;
	}
}

async fn stale_route_case(seed: u64) {
	let mut rng = support::Rng(seed);
	let n = rng.range(5, 8) as usize;
	let nodes: Vec<_> = (1..=n as u64).map(crate::mesh_withdraw::produce_origin).collect();
	let mut edges = Vec::new();
	for i in 1..n {
		edges.push((rng.pick(i), i));
	}
	for _ in 0..rng.range(1, n as u64) {
		let (a, b) = (rng.pick(n), rng.pick(n));
		if a != b && !edges.contains(&(a.min(b), a.max(b))) {
			edges.push((a.min(b), a.max(b)));
		}
	}
	let mut links = Vec::new();
	for &(a, b) in &edges {
		let latency = Duration::from_millis(rng.range(1, 80));
		links.push(Some(peer_with_latency(lite07(), &nodes[a], &nodes[b], latency).await));
	}
	let start = kio::clock::now();
	let broadcast = nodes[0].route("live", origin::RouteOptions::hidden()).unwrap();
	let track = broadcast.broadcast().create_track("video", timed()).unwrap();
	let mut feed = Feed::new(track, start, MESH_STEP);
	broadcast.announce().unwrap();
	kio::clock::sleep(Duration::from_secs(1)).await;
	let mut watched: Vec<_> = nodes.iter().map(|node| Watch::new(node, "live", start)).collect();
	let mut readers = Vec::new();
	for (relay, node) in nodes.iter().enumerate().skip(1) {
		readers.push((relay, reader(node, "live", start).await));
	}
	feed.run(5).await;
	// A relay whose own session closed may have lost its only route for a moment, too
	// short for its announcement to show it.
	let mut severed = vec![false; n];
	for _ in 0..rng.range(1, 2) {
		let edge = rng.pick(edges.len());
		if let Some(pair) = links[edge].take() {
			cut(&pair);
			severed[edges[edge].0] = true;
			severed[edges[edge].1] = true;
		}
	}
	feed.run(30).await;
	kio::clock::sleep(Duration::from_secs(1)).await;

	let dark: Vec<_> = (watched.iter_mut())
		.map(|watch| {
			watch.seen()[1..]
				.iter()
				.any(|seen| !seen.available || matches!(seen.kind, Kind::End | Kind::Offline))
		})
		.collect();
	for (relay, rx) in &mut readers {
		let live = Liveness::of(rx, &feed);
		match &live.ended {
			// A relay that lost every route may end its readers.
			Some(_) => assert!(
				dark[*relay] || severed[*relay],
				"seed {seed}: relay {relay} stayed available: {live:?}"
			),
			None => assert!(live.seamless(), "seed {seed}: relay {relay}: {live:?}"),
		}
	}
	drop(broadcast);
}

/// `P` publishes `live` to `U` and `B`, and `D` pulls both, preferring `U`, whose link takes
/// two seconds each way; `E` pulls `D`. `U`'s link to `P` times out and comes back as soon
/// as `U` let the claim go, so `U` serves `live` again faster than its news reaches `D`.
///
/// `U` resets the subscription below it `Unroutable` only once it lost every route, and it
/// sends that loss to `D` on the same session as a retraction, which the announce cursor
/// neither holds nor cancels when `U` announces again. Proves `D`'s front hears the
/// retraction and moves on, whichever arrives first, so the wait on a stranded track ends:
/// readers on `D` and on `E`, two hops below `U`, get every frame once, in order.
#[kio::test]
async fn an_upstream_flapping_faster_than_its_news_strands_no_reader() {
	let links = Links::new();
	let [p, u, b, d, e] = [1, 2, 3, 4, 5].map(produce_origin);
	let p_u = links.link("p-u", &p, &u, 1).await;
	let _p_b = links.link("p-b", &p, &b, 1).await;
	let mut options = links.options("u-d", &u, &d);
	options.cost = Some(1);
	options.latency = Duration::from_secs(2);
	let _u_d = connect_mock(options).await;
	let _b_d = links.link("b-d", &b, &d, 100).await;
	let _d_e = links.link("d-e", &d, &e, 1).await;
	let broadcast = p.route("live", origin::RouteOptions::hidden()).unwrap();
	let track = broadcast.broadcast().create_track("video", timed()).unwrap();
	broadcast.announce().unwrap();
	kio::clock::sleep(Duration::from_secs(10)).await;

	let start = kio::clock::now();
	let mut at_u = Watch::new(&u, "live", start);
	let mut feed = Feed::new(track, start, STEP);
	let mut readers = [
		("D", reader(&d, "live", start).await),
		("E", reader(&e, "live", start).await),
	];
	feed.run(MOVE).await;
	p_u.server.abort(Error::IdleTimeout);
	p_u.client.abort(Error::IdleTimeout);
	// Long enough for `U` to let the claim go, and no longer.
	kio::clock::sleep(DEBOUNCE + STEP).await;
	let _p_u = links.link("p-u2", &p, &u, 1).await;
	feed.run(MOVE * 4).await;
	kio::clock::sleep(Duration::from_secs(5)).await;

	// Unavailable first, which is an update in place.
	let seen: Vec<_> = (at_u.seen().into_iter())
		.filter(|seen| seen.kind != Kind::Update)
		.collect();
	let kinds: Vec<_> = seen.iter().map(|seen| seen.kind).collect();
	assert_eq!(kinds, [Kind::Start, Kind::Offline, Kind::Start], "U flapped: {seen:?}");
	assert!(
		seen[2].at - seen[1].at < Duration::from_secs(2),
		"faster than the news: {seen:?}"
	);
	for (node, rx) in &mut readers {
		let live = Liveness::of(rx, &feed);
		assert!(live.seamless(), "{node}: {live:?}");
		assert!(live.worst > Duration::from_secs(2), "{node} read through U: {live:?}");
	}
	drop(broadcast);
}

/// `P` publishes `live` to `A` and `B`, and the relay `R` reads it through `A`. A cheaper
/// link from `B` then moves `R`'s route to `B` while the publisher pauses for `pause`
/// between two groups, so the new route has nothing new to deliver for that long.
///
/// Proves the move's cost is bounded by evidence, not time: `R` keeps `A`'s copy until
/// `B`'s delivers a group the reader lacks, however long that takes, and then `A`'s stops.
/// The two routes overlap by at most the group in progress and the one after, each on its
/// own link, so no frame crosses a link twice, and the reader gets every frame once, in
/// order.
#[kio::test]
async fn a_move_overlaps_the_routes_until_the_new_one_delivers() {
	for pause in [Duration::ZERO, Duration::from_secs(30)] {
		let links = Links::new();
		let [p, a, b, r] = [1, 2, 3, 4].map(produce_origin);
		let broadcast = p.route("live", origin::RouteOptions::hidden()).unwrap();
		let track = broadcast.broadcast().create_track("video", timed()).unwrap();
		broadcast.announce().unwrap();
		let _p_a = links.link("p-a", &p, &a, 1).await;
		let _p_b = links.link("p-b", &p, &b, 1).await;
		let _a_r = links.link("a-r", &a, &r, 10).await;
		kio::clock::sleep(Duration::from_secs(1)).await;

		let start = kio::clock::now();
		let mut feed = Feed::new(track, start, STEP);
		let mut rx = reader(&r, "live", start).await;
		// Into the third group, so the move lands mid-group.
		feed.run(2 * FRAMES + 2).await;
		let _b_r = links.link("b-r", &b, &r, 1).await;
		feed.run(FRAMES - 2).await;
		kio::clock::sleep(pause).await;
		feed.run(4 * FRAMES).await;
		let overlap = links.frames();
		feed.run(4 * FRAMES).await;
		kio::clock::sleep(Duration::from_secs(1)).await;

		let frames = links.frames();
		let live = Liveness::of(&mut rx, &feed);
		println!("a move pausing {pause:?}: frames per link {frames:?}, reader {live:?}");
		assert!(live.seamless(), "{pause:?}: {live:?}");
		assert_eq!(frames["a-r"], overlap["a-r"], "{pause:?}: A's copy stopped: {frames:?}");
		let written = feed.written.len() as u64;
		for link in ["a-r", "b-r"] {
			assert!(
				frames[link] <= written,
				"{pause:?}: {link} carried a frame twice: {frames:?}"
			);
		}
		let twice = frames["a-r"] + frames["b-r"] - written;
		assert!(
			twice <= 2 * FRAMES,
			"{pause:?}: {twice} frames on both links: {frames:?}"
		);
		drop(broadcast);
	}
}

/// Pins a duplicate subscription, a decision on moq-dev/moq#8881. A worker `W` claims
/// `pool` and answers a request for `pool/p` with the exact route, under the claim's
/// identity; `R` pulls `W`, and `E1`, `E2`, and `E3` pull `R`. `E1` asks for `pool/p`
/// through the claim, a request naming `W`'s route at depth 1; once the answer's route
/// arrives, `E1` moves onto it, now naming depth 2, and `R` serves that from another front,
/// which asks `W` again: one Identity and track, two subscriptions on the one link,
/// differing only in depth, and `E2` and `E3`, asking after the answer, join the second.
///
/// `W` sends the route and its answer to the first subscription at once, on two streams,
/// and `R` may read either first. When the route wins, the move lands before the first
/// copy delivers anything, and the first closes at once. When the answer wins, the first
/// copy is spliced, and like any move's overlap it closes once the new copy delivers a
/// group the reader lacks, here `W`'s next. Either way one stays open. A fix serves both
/// depths from one subscription, so the move costs none.
#[kio::test]
async fn residual_two_depths_of_one_claim_subscribe_twice() {
	use crate::claim_continues::{Worker, settle};
	let links = Links::new();
	let worker = Worker::new(1, "pool", "w");
	let [r, e1, e2, e3] = [2, 3, 4, 5].map(produce_origin);
	let _w_r = links.link("w-r", &worker.origin, &r, 1).await;
	let _r_e1 = links.link("r-e1", &r, &e1, 1).await;
	let _r_e2 = links.link("r-e2", &r, &e2, 1).await;
	let _r_e3 = links.link("r-e3", &r, &e3, 1).await;
	settle().await;
	let mut subscribed = Vec::new();
	for node in [&e1, &e2, &e3] {
		let broadcast = node.consume().request("pool/p", None).await.unwrap();
		subscribed.push(broadcast.track("video").unwrap().subscribe(None).await.unwrap());
		settle().await;
		assert_eq!(links.subscriptions()["w-r"], 2, "{:?}", links.subscriptions());
		assert!(links.open()["w-r"] <= 2, "{:?}", links.open());
	}
	worker.write();
	settle().await;
	assert_eq!(links.subscriptions()["w-r"], 2, "{:?}", links.subscriptions());
	assert_eq!(links.open()["w-r"], 1, "the first closed: {:?}", links.open());
	assert_eq!(worker.served(), 1);
}

/// `P` publishes `live` through `X` to `A` and through `Y` to `B`, and through `C` to
/// both at a higher cost. `A` and `B` link to each other cheaply, at 50 ms each way, so
/// each prefers the other's route over `C`'s. Two unrelated links close at once, `X`'s to
/// `A` and `Y`'s to `B`: each relay knows only its own lost hop, so `A` moves onto `B`'s
/// direct route and `B` onto `A`'s, both stale, and the routes loop until the news
/// crosses. Readers on `A` and `B` follow `video` from before the cut, and readers asking
/// for `audio`, which nobody read yet, join while the loop stands.
///
/// Proves the loop formed (each relay asked the other), that it ends with the routes: once
/// they settle on `C`, every reader gets every frame once, in order, and no request is left
/// between `A` and `B`.
#[kio::test]
async fn a_request_into_a_routing_loop_follows_the_routes_out() {
	let links = Links::new();
	let [p, x, y, c, a, b] = [1, 2, 3, 4, 5, 6].map(produce_origin);
	let broadcast = p.route("live", origin::RouteOptions::hidden()).unwrap();
	let video = broadcast.broadcast().create_track("video", timed()).unwrap();
	let audio = broadcast.broadcast().create_track("audio", timed()).unwrap();
	broadcast.announce().unwrap();
	let _p_x = links.link("p-x", &p, &x, 1).await;
	let _p_y = links.link("p-y", &p, &y, 1).await;
	let _p_c = links.link("p-c", &p, &c, 1).await;
	let x_a = links.link("x-a", &x, &a, 1).await;
	let y_b = links.link("y-b", &y, &b, 1).await;
	let mut between = Vec::new();
	for (name, from, to) in [("a-b", &a, &b), ("b-a", &b, &a)] {
		let mut options = links.options(name, from, to);
		options.cost = Some(1);
		options.latency = Duration::from_millis(50);
		between.push(connect_mock(options).await);
	}
	let _c_a = links.link("c-a", &c, &a, 10).await;
	let _c_b = links.link("c-b", &c, &b, 10).await;
	kio::clock::sleep(Duration::from_secs(1)).await;

	let start = kio::clock::now();
	let mut feeds = [Feed::new(video, start, STEP), Feed::new(audio, start, STEP)];
	let run = async |feeds: &mut [Feed; 2], count| {
		let [video, audio] = feeds;
		futures::join!(video.run(count), audio.run(count));
	};
	let mut readers = Vec::new();
	for (name, node) in [("A", &a), ("B", &b)] {
		readers.push((name, 0, reader_of(node, "live", "video", start).await));
	}
	run(&mut feeds, MOVE).await;
	cut(&x_a);
	cut(&y_b);
	kio::clock::sleep(Duration::from_millis(5)).await;
	// Asked while the loop stands, so their subscriptions answer only once it ends.
	let asked = async {
		let at_a = reader_of(&a, "live", "audio", start).await;
		let at_b = reader_of(&b, "live", "audio", start).await;
		[("A", 1, at_a), ("B", 1, at_b)]
	};
	let (asked, ()) = futures::join!(asked, run(&mut feeds, MOVE));
	readers.extend(asked);
	kio::clock::sleep(Duration::from_secs(1)).await;

	let subscriptions = links.subscriptions();
	let open = links.open();
	println!("a request into a loop: subscriptions {subscriptions:?}, open {open:?}");
	for link in ["a-b", "b-a"] {
		assert!(
			subscriptions.get(link).is_some_and(|count| *count > 0),
			"{link} looped: {subscriptions:?}"
		);
		assert_eq!(open[link], 0, "{link} left a request: {open:?}");
	}
	for (name, index, rx) in &mut readers {
		let live = Liveness::of(rx, &feeds[*index]);
		assert!(live.seamless(), "{name}, track {index}: {live:?}");
	}
	drop(between);
	drop(broadcast);
}

/// A moq-transport relay that announces back a name it learned from us, which is
/// unsupported (the multicast epic's item 10): its wire carries no path or Identity, so the
/// name looks like a nearer publisher. `R` pulls `live` from its publisher `P` over a link
/// it ranks below the moq-transport ones, and publishes to `M`, which `N` pulls. `O`, a
/// relay of another make, claims `live` toward `R` and answers by reading it from `N`,
/// naming nobody it came through. `R`'s reader of `live` follows `O`, whose request comes
/// back to `R` through `M`, and `R` sends it to `O` again, where it joins the request
/// already waiting.
///
/// Proves the loop costs bounded work and touches nothing else: the reader of `live`
/// stays pending, no message or subscription is sent once it formed, `O` is asked once,
/// and the other broadcasts on both sides of it read on.
#[kio::test]
async fn a_name_announced_back_over_moq_transport_waits_without_retrying() {
	let links = Links::new();
	let transport: moq_net::Version = "moq-transport-18".parse().unwrap();
	let [p, r, m, n, o] = [1, 2, 3, 4, 5].map(produce_origin);
	let live = p.route("live", origin::RouteOptions::hidden()).unwrap();
	live.broadcast().create_track("video", timed()).unwrap();
	live.announce().unwrap();
	let mut options = links.options("p-r", &p, &r);
	options.client_subscribe = Some(r.clone().with_priority(origin::Priority::new(-1)));
	let _p_r = connect_mock(options).await;
	kio::clock::sleep(Duration::from_secs(1)).await;
	let mut pairs = Vec::new();
	for (name, from, to) in [("r-m", &r, &m), ("m-n", &m, &n), ("o-r", &o, &r)] {
		let mut options = links.options(name, from, to);
		options.version = transport;
		pairs.push(connect_mock(options).await);
	}
	let claim = o.claim("live", origin::RouteOptions::default()).unwrap();
	let from = n.consume();
	let asked = std::rc::Rc::new(std::cell::Cell::new(0));
	let counted = asked.clone();
	drop(kio::clock::sim::spawn(async move {
		while let Some(request) = claim.requested().await {
			counted.set(counted.get() + 1);
			match from.request("live", None).await {
				Ok(broadcast) => request.serve(broadcast),
				Err(err) => request.reject(err),
			}
		}
	}));
	let at_r = r.route("at-r", origin::RouteOptions::hidden()).unwrap();
	let at_o = o.route("at-o", origin::RouteOptions::hidden()).unwrap();
	let mut feeds = Vec::new();
	let start = kio::clock::now();
	for route in [&at_r, &at_o] {
		let track = route.broadcast().create_track("video", timed()).unwrap();
		route.announce().unwrap();
		feeds.push(Feed::new(track, start, STEP));
	}
	kio::clock::sleep(Duration::from_secs(1)).await;

	let mut others = [reader(&m, "at-r", start).await, reader(&r, "at-o", start).await];
	let looped = r.consume().request("live", None).await.unwrap();
	let mut looped = looped.track("video").unwrap().subscribe(None);
	let run = async |feeds: &mut Vec<Feed>, count| {
		futures::future::join_all(feeds.iter_mut().map(|feed| feed.run(count))).await;
	};
	run(&mut feeds, MOVE).await;
	let (control, subscriptions) = (links.control(), links.subscriptions());
	run(&mut feeds, 10 * MOVE).await;

	assert!(
		kio::clock::timeout(Duration::from_millis(1), &mut looped)
			.await
			.is_err(),
		"the looped request stays pending"
	);
	assert_eq!(links.control(), control, "nothing sent once the loop formed");
	assert_eq!(links.subscriptions(), subscriptions);
	assert_eq!(asked.get(), 1, "O asked once");
	for (rx, feed) in others.iter_mut().zip(&feeds) {
		let live = Liveness::of(rx, feed);
		assert!(live.seamless(), "{live:?}");
	}
	drop((pairs, live, at_r, at_o));
}

/// `P` publishes `live` and `R` pulls it. `P` ends its track `Dropped` while `live` stays
/// announced, as a binding releasing a handle before its finish does. That says nothing of a
/// route, so readers at `P` and at `R` both end at once rather than wait for the table to
/// move, which nothing does while `live` stands: only `Unroutable` reports a route losing
/// the content.
#[kio::test]
async fn a_track_released_while_announced_ends_every_reader() {
	let links = Links::new();
	let [p, r] = [1, 2].map(produce_origin);
	let broadcast = p.route("live", origin::RouteOptions::hidden()).unwrap();
	let track = broadcast.broadcast().create_track("video", timed()).unwrap();
	broadcast.announce().unwrap();
	let _p_r = links.link("p-r", &p, &r, 1).await;
	kio::clock::sleep(Duration::from_secs(1)).await;

	let start = kio::clock::now();
	let mut feed = Feed::new(track.clone(), start, STEP);
	let mut at_p = reader(&p, "live", start).await;
	let mut at_r = reader(&r, "live", start).await;
	feed.run(2 * FRAMES).await;
	track.abort(Error::Dropped).unwrap();
	kio::clock::sleep(Duration::from_secs(1)).await;
	let ended =
		|rx: &mut mpsc::UnboundedReceiver<Got>| std::iter::from_fn(|| rx.try_recv().ok()).find_map(|got| got.err());
	assert!(ended(&mut at_p).is_some(), "the publisher's own reader ends");
	assert!(ended(&mut at_r).is_some(), "the next relay's reader ends too");
	drop(broadcast);
}
