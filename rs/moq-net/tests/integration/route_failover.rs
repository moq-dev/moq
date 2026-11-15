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
//! which ran on lite-06 under a hold-down, to the path rule.

use crate::mesh_withdraw::{PRODUCTION, PRODUCTION_LATENCY_MS};
use crate::route_change::{payload, produce_origin, timed};
use crate::support;

use std::collections::BTreeMap;
use std::time::Duration;

use futures::channel::mpsc;
use moq_net::{Error, Pattern, Patterns, Timestamp, Version, group, origin, stats, track};
use support::Kind;
use support::harness::{MockConnectOptions, MockPair, connect_mock, peer_with_latency};

fn lite07() -> Version {
	"moq-lite-07-wip".parse().unwrap()
}

/// How often a publisher writes a frame.
const STEP: Duration = Duration::from_millis(20);

/// How often a publisher writes a frame to the mesh's 33 readers: often enough that a
/// reader's gap shows, rarely enough that the mesh fixtures stay cheap.
const MESH_STEP: Duration = Duration::from_millis(100);

/// Frames per group.
const FRAMES: u64 = 5;

/// How long a closed session's far end is doubted.
const DOUBT: Duration = Duration::from_secs(1);

/// How long a relay holds a route no neighbour answers for.
const HOLD: Duration = Duration::from_secs(10);

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
			if index.is_multiple_of(FRAMES) {
				if let Some(group) = self.group.take() {
					group.close().unwrap();
				}
				self.group = Some(self.track.append_group(index / FRAMES).unwrap());
			}
			let group = self.group.as_mut().unwrap();
			let timestamp = Timestamp::from_millis(index * self.step.as_millis() as u64).unwrap();
			group
				.write_frame(timestamp, payload(index / FRAMES, index % FRAMES))
				.unwrap();
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
	let remote = node.consume().request(path, None).await.unwrap();
	let prefs = track::Subscription::default().with_delay(track::Delay::default().with_max(Duration::from_secs(60)));
	let mut sub = remote.track("video").unwrap().subscribe(prefs).await.unwrap();
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

/// How long the moves in [`next_hop_moves_keep_a_live_reader`] take to land: past the
/// origin's update hold, and the new next hop's upstream latency both ways.
const MOVE: u64 = 75;

/// `P` publishes `live` to `A` and `B`, and the relay `R` pulls both, preferring `A`. A
/// reader on `D`, one hop past `R`, follows `live` while `R`'s route to `P` moves four
/// times between next hops, each a move to a usable route with the same instance:
///
/// 1. `C` joins, cheaper than `A`, though its own upstream to `P` lags 150 ms each way,
///    so its ROUTE reaches `R` well after `A`'s;
/// 2. `C`'s session to `R` closes, and `R` falls back to `A`, whose path avoids `C`;
/// 3. `A`'s session to `R` closes, and `R` falls back to `B`;
/// 4. `A` reconnects, and `R` moves back to it.
///
/// Proves the reader gets every frame once, in order, with its subscription intact; that
/// `R` and `D` each show the moves as updates in place, never a retraction, a restart, or
/// an unavailable route; and that each move costs `D` one ROUTE message.
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
	assert_eq!(sent.get("r-d"), Some(&4), "one ROUTE message per move: {sent:?}");
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
	let _c_reads_other = c
		.consume()
		.scope("other", &Patterns::from(Pattern::all()))
		.unwrap()
		.routes();
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
/// reader on a relay whose route stayed usable gets every frame.
///
/// Pins behaviour the contract's continuity goal contradicts, pending a decision: every
/// route left crosses the publisher's relay, the far end of each closed link, which every
/// relay that heard a DEAD doubts for [`DOUBT`]. Each such relay holds the broadcast with
/// no usable route, its neighbours are in the same state and answer with nothing, so the
/// holds end and the relays let go before the doubt lifts: the broadcast goes Offline,
/// then starts again once the doubt lifts, after the origin's two-second quarantine
/// (moq-dev/moq#5375). The router alone does the same: Unavailable, Offline, Start.
#[kio::test]
async fn live_mesh_link_failure_keeps_presence() {
	let version = lite07();
	let nodes: Vec<_> = (1..=34).map(crate::mesh_withdraw::produce_origin).collect();
	let mut links: Vec<Option<MockPair>> = Vec::new();
	for (&(a, b), &ms) in PRODUCTION.iter().zip(PRODUCTION_LATENCY_MS) {
		let latency = Duration::from_millis(ms);
		links.push(Some(peer_with_latency(version, &nodes[a], &nodes[b], latency).await));
	}

	let mut worst = Duration::ZERO;
	let (mut dark_relays, mut retracting, mut lost_readers) = (0, 0, 0);
	for publisher in PUBLISHERS {
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
		feed.run(((HOLD + DOUBT).as_millis() / MESH_STEP.as_millis()) as u64)
			.await;
		// The last frames cross the mesh.
		kio::clock::sleep(Duration::from_secs(1)).await;
		let end = kio::clock::now().saturating_duration_since(start);

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
		dark_relays += dark.len();
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

		drop(broadcast);
		for &index in &cut_links {
			let (a, b) = PRODUCTION[index];
			let latency = Duration::from_millis(PRODUCTION_LATENCY_MS[index]);
			links[index] = Some(peer_with_latency(version, &nodes[a], &nodes[b], latency).await);
		}
		kio::clock::sleep(HOLD + DOUBT).await;
	}
	println!(
		"live mesh link failure over {} publishers: {dark_relays} relays went unavailable, {retracting} retracted and \
		 announced again, {lost_readers} readers lost their subscription, longest without a path {worst:?}",
		PUBLISHERS.len()
	);
}
