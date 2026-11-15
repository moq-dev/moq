#![cfg(feature = "async")]

//! Lite-07 routes across a tiered cluster, end to end on real sessions with time mocked.
//!
//! Two regions each run two cores and two edges. Every edge dials both cores of its
//! region and marks them `upstream`, the cores of a region peer with each other, and
//! each core peers with its counterpart in the other region over a priced backbone link.
//! Publishers push to an edge of one region, and readers sit on an edge of the other.

use crate::route_change::{Delivery, produce_origin, read, timed};
use crate::support;

use std::time::Duration;

use futures::{StreamExt, channel::mpsc};
use moq_net::{Error, Instance, Pattern, Patterns, Timestamp, Version, auth, origin, track};
use support::Kind;
use support::harness::{MockConnectOptions, MockPair, connect_mock, now, spawn};

/// How long a relay holds a route no neighbour answers for.
const HOLD: Duration = Duration::from_secs(10);

/// How long a closed session's far end is doubted.
const DOUBT: Duration = Duration::from_secs(1);

/// Frames per group.
const FRAMES: u64 = 4;

fn lite07() -> Version {
	"moq-lite-07-wip".parse().unwrap()
}

/// Let every route settle, doubts and holds included.
async fn quiesce() {
	kio::clock::sleep(HOLD + DOUBT).await;
}

/// Wait long enough for messages in flight to land, without reaching the doubt.
async fn land() {
	kio::clock::sleep(Duration::from_millis(100)).await;
}

/// Close a session at both ends, as a dropped connection does.
fn cut(pair: &MockPair) {
	pair.server.abort(Error::Cancel);
	pair.client.abort(Error::Cancel);
}

/// `dialer` connects to `acceptor`, each publishing and subscribing everything, the way
/// relays link. The dialer marks the link `link` on its side, ranks it `preference`, and
/// prices it `cost`; the acceptor sees a peer.
async fn join(
	dialer: &origin::Producer,
	link: origin::Link,
	acceptor: &origin::Producer,
	cost: u64,
	preference: i8,
) -> MockPair {
	let dialer = dialer.clone().with_link(link);
	let acceptor = acceptor.clone().with_link(origin::Link::Peer);
	let mut options = MockConnectOptions::new(lite07());
	options.client_publish = Some(dialer.consume().with_hidden(true));
	options.client_subscribe = Some(dialer.with_preference(origin::Preference::new(preference)));
	options.server_publish = Some(acceptor.consume().with_hidden(true));
	options.server_subscribe = Some(acceptor);
	options.cost = Some(cost);
	connect_mock(options).await
}

/// A client `publisher` pushes everything it publishes to `relay`, over a link it prices
/// `cost`.
async fn push(publisher: &origin::Producer, relay: &origin::Producer, cost: u64) -> MockPair {
	let mut options = MockConnectOptions::new(lite07());
	options.client_publish = Some(publisher.consume());
	options.server_subscribe = Some(relay.clone());
	options.cost = Some(cost);
	connect_mock(options).await
}

/// Relays and the sessions between them, each filed under both ends' hops.
#[derive(Default)]
struct Links(Vec<(u64, u64, MockPair)>);

impl Links {
	/// `dialer` connects to `acceptor`; see [`join`].
	async fn join(
		&mut self,
		dialer: &origin::Producer,
		link: origin::Link,
		acceptor: &origin::Producer,
		cost: u64,
		preference: i8,
	) {
		let pair = join(dialer, link, acceptor, cost, preference).await;
		self.0.push((dialer.hop().get(), acceptor.hop().get(), pair));
	}

	/// Close the session between `a` and `b`, and forget it.
	fn cut(&mut self, a: &origin::Producer, b: &origin::Producer) {
		let (a, b) = (a.hop().get(), b.hop().get());
		let between = |(x, y, _): &(u64, u64, MockPair)| (*x, *y) == (a, b) || (*x, *y) == (b, a);
		let (_, _, pair) = self.0.iter().find(|link| between(link)).expect("no such link");
		cut(pair);
		self.0.retain(|link| !between(link));
	}

	/// `node` dies: every session it holds closes.
	fn kill(&mut self, node: &origin::Producer) {
		let hop = node.hop().get();
		let touches = |(x, y, _): &(u64, u64, MockPair)| *x == hop || *y == hop;
		self.0.iter().filter(|link| touches(link)).for_each(|(_, _, pair)| cut(pair));
		self.0.retain(|link| !touches(link));
	}
}

/// One change a node showed at a path.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Seen {
	kind: Kind,
	available: bool,
	cost: u64,
	instance: Option<Instance>,
	at: kio::clock::Instant,
}

/// Every change a node shows at one path, the current route included.
struct Watch {
	rx: mpsc::UnboundedReceiver<Seen>,
}

impl Watch {
	async fn new(node: &origin::Producer, path: &'static str) -> Self {
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
				let route = &event.announce.route;
				let live = matches!(kind, Kind::Start | Kind::Update | Kind::Restart);
				let seen = Seen {
					kind,
					available: route.available(),
					cost: route.cost().value(),
					instance: live.then(|| route.instance().clone()),
					at: kio::clock::now(),
				};
				if tx.unbounded_send(seen).is_err() {
					break;
				}
			}
		});
		land().await;
		Self { rx }
	}

	/// What the node showed since the last call.
	fn seen(&mut self) -> Vec<Seen> {
		std::iter::from_fn(|| self.rx.try_recv().ok()).collect()
	}
}

fn kinds(seen: &[Seen]) -> Vec<Kind> {
	seen.iter().map(|seen| seen.kind).collect()
}

fn tagged(name: &str, group: u64, frame: u64) -> Vec<u8> {
	format!("{name}:{group}.{frame}").into_bytes()
}

/// The next frame the reader reports, failing on an error or a hang.
async fn next(rx: &mut mpsc::UnboundedReceiver<Delivery>) -> (u64, Vec<u8>) {
	let (group, frame) = kio::clock::timeout(Duration::from_secs(10), rx.next())
		.await
		.expect("reader hung")
		.expect("reader ended");
	let (_, payload) = frame.unwrap_or_else(|err| panic!("group {group} failed: {err}"));
	(group, payload)
}

/// Read group `key` in full, as `name` wrote it.
async fn read_group(rx: &mut mpsc::UnboundedReceiver<Delivery>, name: &str, key: u64) {
	for frame in 0..FRAMES {
		assert_eq!(next(rx).await, (key, tagged(name, key, frame)));
	}
}

/// Subscribe `video` at `path` on `node`, reading every group in full.
async fn subscribe(node: &origin::Producer, path: &str) -> (moq_net::broadcast::Consumer, mpsc::UnboundedReceiver<Delivery>) {
	let broadcast = (node.consume().follow(path, None).unwrap().available().await)
		.unwrap()
		.unwrap();
	let prefs = track::Subscription::default().with_delay(track::Delay::default().with_max(Duration::from_secs(60)));
	let sub = broadcast.track("video").unwrap().subscribe(prefs).await.unwrap();
	(broadcast, read(sub))
}

/// One publisher of a broadcast, writing frames tagged with its name.
struct Publisher {
	name: &'static str,
	_origin: origin::Producer,
	route: origin::Route,
	track: track::Producer,
	link: MockPair,
}

impl Publisher {
	/// Publish `path` on a fresh origin `hop`, under `instance` at epoch 0 when given,
	/// pushed to `edge` over a link priced `cost`.
	async fn new(
		name: &'static str,
		hop: u64,
		path: &str,
		instance: Option<&Instance>,
		edge: &origin::Producer,
		cost: u64,
	) -> Self {
		let origin = produce_origin(hop);
		let mut options = origin::RouteOptions::hidden();
		if let Some(instance) = instance {
			options = options.with_instance(instance.clone(), 0);
		}
		let route = origin.route(path, options).unwrap();
		let track = route.broadcast().create_track("video", timed()).unwrap();
		route.announce().unwrap();
		let link = push(&origin, edge, cost).await;
		Self {
			name,
			_origin: origin,
			route,
			track,
			link,
		}
	}

	/// Write group `key` in full.
	fn group(&self, key: u64) {
		let mut group = self.track.create_group(moq_net::group::Info { start: key }).unwrap();
		for frame in 0..FRAMES {
			let timestamp = Timestamp::from_micros(1_000_000 + key * 100_000 + frame * 1_000).unwrap();
			group.write_frame(timestamp, tagged(self.name, key, frame)).unwrap();
		}
		group.close().unwrap();
	}
}

/// The cost of a backbone link between regions.
const BACKBONE: u64 = 100;

/// What an edge pays for its second core, so its first is preferred.
const SECOND_CORE: u64 = 5;

/// What a route from a publisher pushing to edge `A[0]` costs at edge `B[0]`, through the
/// first core of each region: the push, the edge's first core, the backbone, and the far
/// edge's first core.
const VIA_FIRST_CORES: u64 = 1 + 1 + BACKBONE + 1;

/// Two regions `A` (0) and `B` (1), each with cores `[0]` and `[1]` and edges `[0]` and `[1]`.
struct Regions {
	cores: [[origin::Producer; 2]; 2],
	edges: [[origin::Producer; 2]; 2],
	links: Links,
}

impl Regions {
	/// Every edge dials both cores of its region, `[0]` at cost 1 and `[1]` at
	/// [`SECOND_CORE`], marking them upstream. The cores of a region peer at cost 1, and
	/// core `[i]` of `A` peers with core `[i]` of `B` at [`BACKBONE`].
	async fn new() -> Self {
		let cores = [[10, 11], [20, 21]].map(|hops| hops.map(produce_origin));
		let edges = [[12, 13], [22, 23]].map(|hops| hops.map(produce_origin));
		let mut links = Links::default();
		for region in 0..2 {
			for edge in &edges[region] {
				links.join(edge, origin::Link::Upstream, &cores[region][0], 1, 0).await;
				(links.join(edge, origin::Link::Upstream, &cores[region][1], SECOND_CORE, 0)).await;
			}
			(links.join(&cores[region][0], origin::Link::Peer, &cores[region][1], 1, 0)).await;
		}
		for (a, b) in cores[0].iter().zip(&cores[1]) {
			links.join(a, origin::Link::Peer, b, BACKBONE, 0).await;
		}
		Self { cores, edges, links }
	}

	/// Every relay, cores first.
	fn relays(&self) -> impl Iterator<Item = &origin::Producer> {
		self.cores.iter().chain(&self.edges).flatten()
	}
}

/// A publisher pushes `live` to an edge of `A`, and a reader on an edge of `B` reads it
/// through both regions' first cores. The publisher withdraws it: every relay, the far
/// edge included, sees it end once, with nothing shown before or after.
#[kio::test]
async fn a_far_edge_reads_a_claim_and_sees_its_withdrawal_once() {
	let regions = Regions::new().await;
	let publisher = Publisher::new("P", 1, "live", None, &regions.edges[0][0], 1).await;
	quiesce().await;

	let mut watched = Vec::new();
	for relay in regions.relays() {
		watched.push(Watch::new(relay, "live").await);
	}
	let far = &regions.edges[1][0];
	let (_broadcast, mut rx) = subscribe(far, "live").await;
	publisher.group(0);
	read_group(&mut rx, "P", 0).await;

	let at_far = watched[6].seen();
	assert!(
		matches!(at_far.as_slice(), [Seen { kind: Kind::Start, available: true, cost: VIA_FIRST_CORES, .. }]),
		"the far edge reads through the first cores: {at_far:?}"
	);
	for watch in &mut watched {
		watch.seen();
	}

	publisher.route.withdraw();
	quiesce().await;
	for (relay, watch) in regions.relays().zip(&mut watched) {
		let seen = watch.seen();
		assert_eq!(kinds(&seen), [Kind::End], "relay {}: {seen:?}", relay.hop());
	}
}

/// A reader on an edge of `B` reads `live` through the first core of `A`, which then dies.
/// The second core of `A` reaches the publisher's edge on its own, so every relay moves
/// onto a path avoiding the dead core at once: the far edge sees the route updated in
/// place under the same instance, never restarted, ended, or offline, and its reader
/// reads every frame once.
#[kio::test]
async fn losing_a_core_keeps_the_far_edge_reading() {
	let mut regions = Regions::new().await;
	let publisher = Publisher::new("P", 1, "live", None, &regions.edges[0][0], 1).await;
	quiesce().await;

	let far = regions.edges[1][0].clone();
	let mut watch = Watch::new(&far, "live").await;
	let (broadcast, mut rx) = subscribe(&far, "live").await;
	publisher.group(0);
	read_group(&mut rx, "P", 0).await;
	let start = watch.seen();
	assert_eq!(start.len(), 1, "{start:?}");
	assert_eq!(start[0].cost, VIA_FIRST_CORES, "{start:?}");

	let dying = regions.cores[0][0].clone();
	regions.links.kill(&dying);
	land().await;
	publisher.group(1);
	read_group(&mut rx, "P", 1).await;
	quiesce().await;
	publisher.group(2);
	read_group(&mut rx, "P", 2).await;

	let seen = watch.seen();
	assert!(!seen.is_empty(), "the far edge's route moved off the dead core");
	assert!(
		seen.iter()
			.all(|seen| seen.kind == Kind::Update && seen.instance == start[0].instance),
		"updated in place under one instance: {seen:?}"
	);
	assert!(
		seen.last().unwrap().available && seen.last().unwrap().cost > VIA_FIRST_CORES,
		"settled on a longer path: {seen:?}"
	);
	land().await;
	assert!(rx.try_recv().is_err(), "trailing delivery");
	drop(broadcast);
}

/// Two publishers on the two edges of `A` serve `live` under one explicit instance at the
/// same epoch, the first cheaper. A reader on an edge of `B` reads the first's copy until
/// the first becomes unreachable, then reads on through the second's: the far edge keeps
/// one broadcast, sees the route updated in place rather than restarted, and its reader
/// takes the next group from the second with nothing lost or repeated.
#[kio::test]
async fn a_matching_epoch_takes_over_at_the_far_edge() {
	let regions = Regions::new().await;
	let instance = Instance::random();
	let first = Publisher::new("P1", 1, "live", Some(&instance), &regions.edges[0][0], 1).await;
	let second = Publisher::new("P2", 2, "live", Some(&instance), &regions.edges[0][1], 10).await;
	quiesce().await;

	let far = regions.edges[1][0].clone();
	let mut watch = Watch::new(&far, "live").await;
	let (broadcast, mut rx) = subscribe(&far, "live").await;
	first.group(0);
	second.group(0);
	read_group(&mut rx, "P1", 0).await;
	let start = watch.seen();
	assert_eq!(kinds(&start), [Kind::Start], "{start:?}");

	cut(&first.link);
	quiesce().await;
	second.group(1);
	read_group(&mut rx, "P2", 1).await;

	let seen = watch.seen();
	assert!(
		seen.iter()
			.all(|seen| seen.kind == Kind::Update && seen.instance.as_ref() == Some(&instance)),
		"one broadcast, updated in place: {seen:?}"
	);
	assert!(seen.last().is_some_and(|seen| seen.available), "{seen:?}");
	let now = (far.consume().follow("live", None).unwrap().available().await)
		.unwrap()
		.unwrap();
	assert!(now.is_clone(&broadcast), "the far edge kept its broadcast");
	land().await;
	assert!(rx.try_recv().is_err(), "trailing delivery");
	drop((first, second));
}

/// What the CDN's own link between its relays costs, well above the LAN's.
const CDN_INTERNAL: u64 = 10;

/// A local cluster beside a CDN: LAN relays `lan[0]` and `lan[1]` peer at cost 1, and each
/// dials its own CDN relay, `cdn[0]` and `cdn[1]`, marking it upstream, while the CDN's
/// relays peer at [`CDN_INTERNAL`]. Without the down-only flag the CDN would reach itself
/// through the cheaper LAN.
struct Lan {
	cdn: [origin::Producer; 2],
	lan: [origin::Producer; 2],
	links: Links,
}

impl Lan {
	async fn new() -> Self {
		Self::with_uplinks(origin::Link::Upstream).await
	}

	/// [`Self::new`], with each LAN relay marking its CDN link `uplink`.
	async fn with_uplinks(uplink: origin::Link) -> Self {
		let cdn = [30, 31].map(produce_origin);
		let lan = [32, 33].map(produce_origin);
		let mut links = Links::default();
		links.join(&cdn[0], origin::Link::Peer, &cdn[1], CDN_INTERNAL, 0).await;
		links.join(&lan[0], origin::Link::Peer, &lan[1], 1, 0).await;
		for (lan, cdn) in lan.iter().zip(&cdn) {
			links.join(lan, uplink, cdn, 1, 0).await;
		}
		Self { cdn, lan, links }
	}

	/// Every relay, the CDN's first.
	fn relays(&self) -> impl Iterator<Item = &origin::Producer> {
		self.cdn.iter().chain(&self.lan)
	}
}

/// A local publisher on `lan[1]` and a CDN publisher on `cdn[0]`. The LAN partitions, and
/// `lan[0]` reaches the local broadcast through the CDN under the same instance, then
/// moves back onto the LAN in place once it heals. Meanwhile `cdn[1]` reaches the CDN
/// publisher only over the CDN's own expensive link, never through the LAN.
///
/// PINNED, contradicting the contract ("a node ... holds ... showing the announcements as
/// unavailable"; "the reader's announcement stays, unavailable"): at the cut `lan[0]`'s
/// router holds the route through the doubt on `lan[1]`, but its origin shows the
/// broadcast Offline, then Start through the CDN once the doubt and the origin's
/// two-second retraction quarantine pass. The held claim's only home was the closed LAN
/// session's stream, which `route::Node::close` drops, and nothing re-homes it. The
/// reader's track ends with the error either way, as a held route ends a forwarded
/// subscription, so a fresh one reads on through the CDN.
#[kio::test]
async fn a_partitioned_lan_reaches_itself_through_the_cdn_and_heals() {
	let mut lan = Lan::new().await;
	let local = Publisher::new("O", 2, "local", None, &lan.lan[1], 1).await;
	let _cdn = Publisher::new("Q", 3, "cdn", None, &lan.cdn[0], 1).await;
	quiesce().await;

	let mut at_cdn = Watch::new(&lan.cdn[1], "cdn").await;
	let mut island = Watch::new(&lan.lan[0], "local").await;
	let (_broadcast, mut rx) = subscribe(&lan.lan[0], "local").await;
	local.group(0);
	read_group(&mut rx, "O", 0).await;
	let start = island.seen();
	assert!(
		matches!(start.as_slice(), [Seen { kind: Kind::Start, cost: 2, .. }]),
		"over the LAN: {start:?}"
	);
	let instance = start[0].instance.clone();

	let [lan0, lan1] = lan.lan.clone();
	let cut_at = kio::clock::now();
	lan.links.cut(&lan0, &lan1);
	quiesce().await;
	let partitioned = island.seen();
	// The local publisher's push, `lan[1]`'s uplink, the CDN's link, and `lan[0]`'s uplink.
	let through_cdn = 1 + 1 + CDN_INTERNAL + 1;
	let [offline, back] = partitioned.as_slice() else {
		panic!("the island let go and came back: {partitioned:?}");
	};
	assert!(
		offline.kind == Kind::Offline && offline.at == cut_at,
		"PINNED: let go at the cut, not held: {partitioned:?}"
	);
	assert!(
		back.kind == Kind::Start
			&& back.available
			&& back.cost == through_cdn
			&& back.instance == instance
			&& back.at.saturating_duration_since(cut_at) >= DOUBT,
		"through the CDN once the doubt lifts: {partitioned:?}"
	);
	let ended = kio::clock::timeout(Duration::from_secs(1), rx.next()).await;
	assert!(
		matches!(ended, Ok(Some((_, Err(_))))),
		"the held route ended the track: {ended:?}"
	);
	// A fresh subscription joins at the latest group, the one already read.
	let (_broadcast, mut rx) = subscribe(&lan.lan[0], "local").await;
	read_group(&mut rx, "O", 0).await;
	local.group(1);
	read_group(&mut rx, "O", 1).await;

	lan.links.join(&lan0, origin::Link::Peer, &lan1, 1, 0).await;
	quiesce().await;
	let healed = island.seen();
	assert!(
		matches!(healed.as_slice(), [Seen { kind: Kind::Update, available: true, cost: 2, .. }]),
		"back on the LAN in place: {healed:?}"
	);
	local.group(2);
	read_group(&mut rx, "O", 2).await;

	let cdn = at_cdn.seen();
	assert!(
		matches!(cdn.as_slice(), [Seen { kind: Kind::Start, cost, .. }] if *cost == 1 + CDN_INTERNAL),
		"the CDN's own link, throughout: {cdn:?}"
	);
}

/// The CDN's own link costs more than a detour through the LAN, but every route the LAN
/// learned from the CDN is down-only, so `cdn[1]` reaches the CDN publisher over that
/// link, and once it closes lets the broadcast go rather than reaching `cdn[0]` through
/// the LAN. The control leaves the LAN's uplinks unmarked: `cdn[1]` then takes the LAN
/// from the start and keeps it when the link closes.
#[kio::test]
async fn the_cdn_never_reaches_itself_through_the_lan() {
	for marked in [true, false] {
		let uplink = if marked { origin::Link::Upstream } else { origin::Link::Peer };
		let mut lan = Lan::with_uplinks(uplink).await;
		let _cdn = Publisher::new("Q", 3, "cdn", None, &lan.cdn[0], 1).await;
		quiesce().await;

		let mut watch = Watch::new(&lan.cdn[1], "cdn").await;
		let start = watch.seen();
		// The publisher's push, then the CDN's own link, or the LAN's three links.
		let cost = if marked { 1 + CDN_INTERNAL } else { 1 + 3 };
		assert!(
			matches!(start.as_slice(), [Seen { kind: Kind::Start, cost: c, .. }] if *c == cost),
			"marked {marked}: {start:?}"
		);
		let [cdn0, cdn1] = lan.cdn.clone();
		lan.links.cut(&cdn0, &cdn1);
		quiesce().await;
		let seen = watch.seen();
		let expected: &[Kind] = if marked { &[Kind::Offline] } else { &[] };
		assert_eq!(kinds(&seen), expected, "marked {marked}: {seen:?}");
	}
}

/// Every request a claim hears, by path.
fn heard(claim: origin::Claim) -> mpsc::UnboundedReceiver<String> {
	let (tx, rx) = mpsc::unbounded();
	kio::clock::sim::spawn(async move {
		while let Some(request) = claim.requested().await {
			if tx.unbounded_send(request.path().to_string()).is_err() {
				return;
			}
		}
	});
	rx
}

/// One origin claims `live` from `lan[1]` and another `live/a` from `cdn[0]`. On every
/// relay a path under `live/a` resolves to the narrower claim and one under `live/b` to
/// the wider, and each request reaches the claim it resolved to.
#[kio::test]
async fn overlapping_wildcards_resolve_to_the_most_specific_claim_everywhere() {
	let lan = Lan::new().await;
	let wide = produce_origin(4);
	let narrow = produce_origin(5);
	let wide_claim = wide.claim("live", origin::RouteOptions::default()).unwrap();
	let narrow_claim = narrow.claim("live/a", origin::RouteOptions::default()).unwrap();
	let (wide_instance, narrow_instance) = (wide_claim.instance(), narrow_claim.instance());
	let mut wide_heard = heard(wide_claim);
	let mut narrow_heard = heard(narrow_claim);
	let _pushes = [push(&wide, &lan.lan[1], 1).await, push(&narrow, &lan.cdn[0], 1).await];
	quiesce().await;

	let mut subscribing = Vec::new();
	for relay in lan.relays() {
		for (under, instance) in [("a", &narrow_instance), ("b", &wide_instance)] {
			let path = format!("live/{under}/{}", relay.hop());
			let broadcast = relay.consume().request(path.as_str(), None).await.unwrap();
			assert_eq!(broadcast.info().instance.as_ref(), Some(instance), "{path}");
			// Reading a track sends the request on to the claim.
			let track = broadcast.track("video").unwrap();
			subscribing.push(kio::clock::sim::spawn(async move { track.subscribe(None).await.map(drop) }));
		}
	}
	land().await;
	let mut wide_paths: Vec<_> = std::iter::from_fn(|| wide_heard.try_recv().ok()).collect();
	let mut narrow_paths: Vec<_> = std::iter::from_fn(|| narrow_heard.try_recv().ok()).collect();
	wide_paths.sort();
	narrow_paths.sort();
	let expected = |under: &str| {
		let mut paths: Vec<_> = (lan.relays()).map(|relay| format!("live/{under}/{}", relay.hop())).collect();
		paths.sort();
		paths
	};
	assert_eq!(narrow_paths, expected("a"));
	assert_eq!(wide_paths, expected("b"));
	drop(subscribing);
}

/// The contract's accepted cost of one selection per publisher and prefix: a relay whose
/// preferred route is down-only exports nothing, so a neighbour cannot use an exportable
/// route that relay also holds.
///
/// A local publisher on `L1`, whose uplink is `C1`. `L2` reaches it over the LAN at
/// `lan_cost` and through its uplink `C2`, which hears it from `C1`, and `L2` also dials
/// a far CDN relay `C3`, linked to `C2` at 50. At a LAN cost of 13 `L2` prefers the
/// down-only route through `C2` (13 against 14), and `C3` hauls the broadcast across the
/// CDN at 62 although `L2`'s LAN route would reach it at 15. At 11 `L2` prefers the LAN,
/// exports it, and `C3` takes it at 13. Pinned, not endorsed: the epic accepts it, and
/// ranking local peers above a backup uplink avoids it.
#[kio::test]
async fn a_preferred_down_only_route_hides_the_exportable_alternative() {
	for (lan_cost, at_far) in [(13, 1 + 1 + CDN_INTERNAL + 50), (11, 1 + 11 + 1)] {
		let [c1, c2, c3] = [30, 31, 34].map(produce_origin);
		let [l1, l2] = [32, 33].map(produce_origin);
		let mut links = Links::default();
		links.join(&c1, origin::Link::Peer, &c2, CDN_INTERNAL, 0).await;
		links.join(&c2, origin::Link::Peer, &c3, 50, 0).await;
		links.join(&l1, origin::Link::Upstream, &c1, 1, 0).await;
		links.join(&l2, origin::Link::Upstream, &c2, 1, 0).await;
		links.join(&l2, origin::Link::Upstream, &c3, 1, 0).await;
		links.join(&l1, origin::Link::Peer, &l2, lan_cost, 0).await;
		let _local = Publisher::new("O", 2, "local", None, &l1, 1).await;
		quiesce().await;

		let mut watch = Watch::new(&c3, "local").await;
		let seen = watch.seen();
		assert!(
			matches!(seen.as_slice(), [Seen { kind: Kind::Start, cost, .. }] if *cost == at_far),
			"LAN at {lan_cost}: {seen:?}"
		);
	}
}

/// Two local publishers serve `live`, each under its own minted instance: `O1` on `lan[1]`
/// and, priced to stand by, `O2` on `lan[0]`. `O1` becomes unreachable, and every relay
/// moves the path onto `O2`'s instance as a Restart, never as an Update and never through
/// an End.
#[kio::test]
async fn an_epochless_replacement_restarts_everywhere() {
	let lan = Lan::new().await;
	let first = Publisher::new("O1", 6, "live", None, &lan.lan[1], 1).await;
	let second = Publisher::new("O2", 7, "live", None, &lan.lan[0], 10).await;
	quiesce().await;

	let mut watched = Vec::new();
	for relay in lan.relays() {
		let mut watch = Watch::new(relay, "live").await;
		let start = watch.seen();
		assert!(
			matches!(start.as_slice(), [Seen { kind: Kind::Start, .. }]),
			"relay {}: {start:?}",
			relay.hop()
		);
		watched.push((watch, start[0].instance.clone()));
	}

	cut(&first.link);
	quiesce().await;
	for (relay, (watch, before)) in lan.relays().zip(&mut watched) {
		let seen = watch.seen();
		let hop = relay.hop();
		assert!(
			seen.iter().all(|seen| seen.kind != Kind::End && seen.kind != Kind::Offline),
			"relay {hop}: {seen:?}"
		);
		let restart = seen.iter().position(|seen| seen.kind == Kind::Restart);
		let Some(restart) = restart else {
			panic!("relay {hop}: no restart: {seen:?}");
		};
		assert!(
			seen[restart].available && seen[restart].instance != *before,
			"relay {hop}: {seen:?}"
		);
		assert!(
			seen[restart..].iter().all(|later| later.instance == seen[restart].instance),
			"relay {hop}: {seen:?}"
		);
	}
	drop(second);
}

/// `dialer` dials `acceptor` as an uplink, the way [`join`] does, with the acceptor taking
/// the tokens the dialer presents through the returned requests.
async fn tokened_uplink(dialer: &origin::Producer, acceptor: &origin::Producer) -> (MockPair, auth::Requests) {
	let version = lite07();
	let mut transport = moq_net::transport::memory::Config::default();
	transport.protocol = Some(version.alpn().to_string());
	let (client_transport, server_transport) = support::mock::create_mock_session_pair(transport);
	let dialer = dialer.clone().with_link(origin::Link::Upstream);
	let acceptor = acceptor.clone().with_link(origin::Link::Peer);
	let client = moq_net::Client::new()
		.with_versions(version.into())
		.with_publisher(dialer.consume().with_hidden(true))
		.with_subscriber(dialer.clone());
	let server = moq_net::Server::new()
		.with_versions(version.into())
		.with_publisher(acceptor.consume().with_hidden(true))
		.with_subscriber(acceptor);
	let client_fut = async {
		let (session, driver) = (client.connect(now(), client_transport.clone()).await).expect("client handshake");
		spawn(driver);
		session
	};
	let server_fut = async {
		let mut handshake = (server.accept_request(now(), server_transport.clone()).await).expect("server handshake");
		let requests = handshake.requests();
		let (session, driver) = handshake.ok().await.expect("server accept");
		spawn(driver);
		(session, requests)
	};
	let (client, (server, requests)) = futures::join!(client_fut, server_fut);
	let pair = MockPair {
		client,
		server,
		client_transport,
		server_transport,
		client_discovery: Some(dialer.consume().routes()),
	};
	(pair, requests)
}

/// A grant of the `publish` subtrees, or of everything for `**`, subscribing to
/// everything and lapsing at `expires`.
fn grant(publish: &[&str], expires: Option<kio::clock::Instant>) -> auth::Grant {
	let mut grant = auth::Grant::all();
	grant.publish = (publish.iter())
		.map(|prefix| match *prefix {
			"**" => Pattern::all(),
			prefix => Pattern::subtree(prefix).unwrap(),
		})
		.collect::<Patterns>();
	grant.expires = expires;
	grant
}

/// Whether a route only moved in place: each change an Update that keeps it available.
/// Refiling it under another identity or route prefix is an Update, never a let go.
fn moved(seen: &[Seen]) -> bool {
	seen.iter().all(|seen| seen.kind == Kind::Update && seen.available)
}

/// A LAN relay `L` dials its uplink `C1`, which grants its connection credential publish
/// rights to `local` alone, and `C1` peers with `C2`. A local publisher on `L` serves
/// `local/x`, inside that scope, and `other/z`, outside it. `L` presents a second token
/// that `C1` grants publish rights to `**`, until the token expires and `C1` revokes it.
/// The union crosses the scoped boundary each way: `other/z` appears at both CDN relays
/// once the wide token lands and goes Offline once it expires, as the narrowed floor
/// cannot certify a withdrawal outside `local`. `local/x` only moves in place across both
/// changes, refiled from a qualified identity to a bare one and back with no End or
/// Offline, and still ends when its publisher withdraws it. Widening to the root once
/// let `local/x` go at `C1` while its request reopened above `local`.
#[kio::test]
async fn a_grant_union_crosses_the_scoped_boundary_each_way() {
	let [c1, c2, l] = [30, 31, 32].map(produce_origin);
	let mut links = Links::default();
	links.join(&c1, origin::Link::Peer, &c2, CDN_INTERNAL, 0).await;
	let (uplink, mut requests) = tokened_uplink(&l, &c1).await;
	// The connection credential's grant, set the way a relay sets it: moq-net widens this
	// scope by every grant the acceptor issues, and floors certify only inside it.
	uplink.server.auth().authorize(&grant(&["local"], None));
	let wide_expires = kio::clock::now().saturating_add(Duration::from_secs(30));
	kio::clock::sim::spawn(async move {
		let mut held = Vec::new();
		while let Some(request) = requests.next().await {
			match request.token().as_ref() {
				b"wide" => {
					let issued = request.accept(grant(&["**"], Some(wide_expires)));
					// Revoking a token's grant once it expires is the acceptor's job.
					kio::clock::sim::spawn(async move {
						kio::clock::sleep_until(wide_expires).await;
						issued.revoke(moq_net::SessionError::Expired, "expired");
					});
				}
				_ => held.push(request.accept(grant(&["local"], None))),
			}
		}
	});
	let publisher = produce_origin(2);
	let inside = publisher.route("local/x", origin::RouteOptions::hidden()).unwrap();
	inside.announce().unwrap();
	let _push = push(&publisher, &l, 1).await;
	quiesce().await;

	let mut watched = Vec::new();
	for relay in [&c1, &c2] {
		watched.push((Watch::new(relay, "local/x").await, Watch::new(relay, "other/z").await));
	}
	for (inside, outside) in &mut watched {
		let (inside, outside) = (inside.seen(), outside.seen());
		assert!(
			matches!(inside.as_slice(), [Seen { kind: Kind::Start, available: true, .. }]) && outside.is_empty(),
			"scoped: inside {inside:?} outside {outside:?}"
		);
	}

	let wide = uplink.client.auth().add("wide").unwrap();
	wide.answered().await.unwrap();
	// A dialer announcing outside its union closes its session, so `other/z` waits on the
	// wide grant.
	let outside = publisher.route("other/z", origin::RouteOptions::hidden()).unwrap();
	outside.announce().unwrap();
	kio::clock::sleep(Duration::from_secs(5)).await;
	for (inside, outside) in &mut watched {
		let (inside, outside) = (inside.seen(), outside.seen());
		assert!(
			moved(&inside) && matches!(outside.as_slice(), [Seen { kind: Kind::Start, available: true, .. }]),
			"widened: inside {inside:?} outside {outside:?}"
		);
	}

	// The narrowed floor certifies nothing outside `local`, so `other/z` is let go
	// (Offline), not ended: its publisher never withdrew it.
	kio::clock::sleep_until(wide_expires).await;
	quiesce().await;
	for (inside, outside) in &mut watched {
		let (inside, outside) = (inside.seen(), kinds(&outside.seen()));
		assert!(
			moved(&inside) && outside == [Kind::Offline],
			"expired: inside {inside:?} outside {outside:?}"
		);
	}
	assert_eq!(uplink.client_transport.close_reason(), None, "the session outlived the token");

	// Inside the scope the floor still certifies a withdrawal.
	inside.withdraw();
	kio::clock::sleep(Duration::from_secs(1)).await;
	for (inside, outside) in &mut watched {
		let (inside, outside) = (kinds(&inside.seen()), outside.seen());
		assert!(inside == [Kind::End] && outside.is_empty(), "withdrawn: {inside:?}");
	}
	drop((wide, uplink, inside, outside, links));
}

