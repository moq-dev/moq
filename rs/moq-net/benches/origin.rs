//! Fan-out benchmarks for the origin: its route table, the announce cursors
//! watching it, request resolution, the handoff between local sources, and the
//! delivery through a front.
//!
//! Every shape is swept over both axes, publishers (routes) and subscribers
//! (cursors), so a cost that grows with the size of the table rather than with
//! the tree around the touched path shows up as a slope. The table is a trie
//! keyed by path segment: an announcement visits only the cursors on the walk
//! down to its prefix and beneath it. Competing routes at that prefix are
//! ranked once per change, then each cursor selects its first visible entry.
//!
//! `narrow*` put a session in front of the origin, since a narrowing is
//! enforced by the session serving it: every subscription holds a gate on the
//! session's grant, and a narrowing wakes each one. The baseline is a session
//! that never narrows.
//!
//! A withdrawal wave is swept over the broadcasts it withdraws and the cursors
//! polling through it, and a route churning beneath an unchanged winner over the
//! table and its cursors.
//!
//! An equal-cost pool is swept the same way, over its members and the paths it
//! already serves, and over those paths and the cursors watching the pool.
//!
//! One-frame groups through a front with a single route are swept over readers and the
//! groups each holds open, which is what a reader records per group it hands out.
//!
//! A route swap on one front is swept over its tracks and the copies each track
//! still holds from earlier routes, and a front retiring over the fronts around it.
//! Readers draining a swapped track are swept over their count, the groups every
//! route delivered, and the routes each reader still holds a copy of.
//!
//! A claim broader than its grant is filed at each of the grant's prefixes beneath it: one
//! is filed and dropped, swept over those prefixes and the claims already filed at each.
//!
//! A claim's answer, and its retraction moving the claim to a fresh identity, is swept
//! over the paths the claim already answered and the cursors watching it.
//!
//! Peer claims land through the `fuzz` module, as a session lands them.
//!
//! Run with `cargo bench -p moq-net --features fuzz --bench origin`.

#[path = "../tests/integration/support/mod.rs"]
mod support;

use std::fmt;
use std::task::Poll;
use std::time::Duration;

use futures::FutureExt;
use moq_net::{Identity, Pattern, Prefix, Prefixes, Timestamp, auth, broadcast, origin, track};
use support::harness::{MockConnectOptions, MockPair, connect_mock};
use swole::bench;

/// A case's parameters, and the name it runs under.
#[derive(Clone)]
struct Case<T> {
	name: String,
	of: T,
}

impl<T> fmt::Display for Case<T> {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		f.write_str(&self.name)
	}
}

/// Name each of `values` by `name`.
fn cases<T: Copy>(values: impl IntoIterator<Item = T>, name: impl Fn(T) -> String) -> Vec<Case<T>> {
	values.into_iter().map(|of| Case { name: name(of), of }).collect()
}

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> track::Info {
	track::Info::default()
		.with_max_age(None)
		.with_timescale(moq_net::Timescale::MILLI)
}

/// Where the group at `key` starts on a [`timed`] track: groups start strictly in key order.
fn at(key: u64) -> Timestamp {
	Timestamp::from_millis(key).unwrap()
}

/// `(publishers, subscribers)` shapes for the fan-out benchmarks.
const SHAPES: [(usize, usize); 3] = [(100, 10), (1_000, 100), (1_000, 1_000)];

/// `(duplicates, subscribers)` shapes for one *contended* prefix: how many
/// routes cover the same path, against how many cursors watch it. Includes
/// sparse fanout, a live fleet's mesh width, and larger contended tables.
const CONTENDED: [(usize, usize); 6] = [(1, 1), (30, 1), (30, 30), (60, 60), (240, 60), (240, 240)];

/// [`SHAPES`], named `<publishers>p_<subscribers>s`.
fn shapes() -> Vec<Case<(usize, usize)>> {
	cases(SHAPES, |(p, s)| format!("{p}p_{s}s"))
}

/// An origin with `publishers` broadcasts under `room/` and `subscribers`
/// cursors watching everything. The handles are held: dropping a broadcast
/// retracts its route, and dropping a cursor unregisters it.
struct Fanout {
	producer: origin::Producer,
	consumer: origin::Consumer,
	driver: origin::Driver,
	_publishers: Vec<origin::Route>,
	subscribers: Vec<origin::Routes>,
}

fn fanout(publishers: usize, subscribers: usize) -> Fanout {
	let (producer, driver) = origin::Producer::new(origin::Config::default());
	let consumer = producer.consume();
	let publishers = (0..publishers)
		.map(|i| {
			producer
				.route(format!("room/{i}"), moq_net::origin::RouteOptions::default())
				.unwrap()
		})
		.collect();
	let mut subscribers: Vec<origin::Routes> = (0..subscribers).map(|_| consumer.routes()).collect();
	// Drain the replay so each iteration measures only what it adds.
	for cursor in &mut subscribers {
		while cursor.next().now_or_never().flatten().is_some() {}
	}
	Fanout {
		producer,
		consumer,
		driver,
		_publishers: publishers,
		subscribers,
	}
}

fn fleet(case: Case<(usize, usize)>) -> Fanout {
	fanout(case.of.0, case.of.1)
}

/// One publish delivered to every subscriber, then its retraction: the
/// announcement fans out to `subscribers` cursors regardless of `publishers`.
#[bench(args = shapes(), setup = fleet)]
fn announce(fleet: &mut Fanout) {
	let handle = fleet
		.producer
		.route("room/incoming", moq_net::origin::RouteOptions::default())
		.unwrap();
	for cursor in &mut fleet.subscribers {
		cursor.next().now_or_never().flatten().expect("announce delivered");
	}
	drop(handle);
	for cursor in &mut fleet.subscribers {
		cursor.next().now_or_never().flatten().expect("retract delivered");
	}
}

/// A grant of `prefixes` prefixes beneath `a`, `claims` claims already filed at each, and
/// one root cursor watching.
struct Filing {
	cursor: origin::Routes,
	_held: Vec<origin::Claim>,
	scoped: origin::Producer,
	_driver: origin::Driver,
	_producer: origin::Producer,
}

fn filing_cases() -> Vec<Case<(u64, usize)>> {
	cases(
		[1, 16, 256].into_iter().flat_map(|p| [1, 16, 256].map(|c| (p, c))),
		|(p, c)| format!("{p}prefixes/{c}claims"),
	)
}

fn filed(case: Case<(u64, usize)>) -> Filing {
	let (prefixes, claims) = case.of;
	let (producer, driver) = origin::Producer::new(origin::Config::default());
	let grant: Prefixes = (0..prefixes)
		.map(|i| Prefix::try_from(format!("a/p{i}").as_str()).unwrap())
		.collect();
	let scoped = producer.scope("", &grant).unwrap();
	let held = (0..claims)
		.map(|_| scoped.claim("a", moq_net::origin::RouteOptions::default()).unwrap())
		.collect();
	let mut cursor = producer.consume().routes();
	while cursor.next().now_or_never().flatten().is_some() {}
	Filing {
		cursor,
		_held: held,
		scoped,
		_driver: driver,
		_producer: producer,
	}
}

/// A claim at `a` through a grant of `prefixes` prefixes beneath it, filed at each, then
/// dropped, with one root cursor watching, beside `claims` claims already filed at each prefix.
/// A cost that grows with the claims at a prefix, rather than with the places one is filed at,
/// shows up as a slope over `claims`.
#[bench(args = filing_cases(), setup = filed, items = |case: Case<(u64, usize)>| case.of.0)]
fn filing(filing: &mut Filing) {
	let claim = filing
		.scoped
		.claim("a", moq_net::origin::RouteOptions::default())
		.unwrap();
	while filing.cursor.next().now_or_never().flatten().is_some() {}
	drop(claim);
	while filing.cursor.next().now_or_never().flatten().is_some() {}
}

/// `routes` claims, and `readers` cursors filtered to `**/*.chat`, listing or covering.
struct Covering {
	cursors: Vec<origin::Routes>,
	_consumer: origin::Consumer,
	_routes: Vec<origin::Claim>,
	_driver: origin::Driver,
	producer: origin::Producer,
	covering: bool,
}

fn covering_cases() -> Vec<Case<(usize, usize, bool)>> {
	let shapes = [10, 1_000].into_iter().flat_map(|routes| {
		[1, 100]
			.into_iter()
			.flat_map(move |readers| [false, true].map(|c| (routes, readers, c)))
	});
	cases(shapes, |(routes, readers, covering)| {
		format!("{routes}routes_{readers}readers_covering{covering}")
	})
}

fn covered(case: Case<(usize, usize, bool)>) -> Covering {
	let (routes, readers, covering) = case.of;
	let (producer, driver) = origin::Producer::new(origin::Config::default());
	let routes = (0..routes)
		.map(|i| {
			producer
				.claim(format!("room/{i}/mic.hang"), moq_net::origin::RouteOptions::default())
				.unwrap()
		})
		.collect();
	let consumer = producer
		.consume()
		.filter(&"**/*.chat".parse::<Pattern>().unwrap())
		.unwrap();
	let consumer = if covering { consumer.covering() } else { consumer };
	let mut cursors: Vec<_> = (0..readers).map(|_| consumer.routes()).collect();
	for cursor in &mut cursors {
		while cursor.next().now_or_never().flatten().is_some() {}
	}
	Covering {
		cursors,
		_consumer: consumer,
		_routes: routes,
		_driver: driver,
		producer,
		covering,
	}
}

/// Matched and overlap-only route churn, swept over table size and watching cursors,
/// for listing cursors and covering ones.
#[bench(args = covering_cases(), setup = covered)]
fn announce_covering(covering: &mut Covering) {
	for (name, delivered) in [("room/live.chat", true), ("room/mic.hang", covering.covering)] {
		let route = covering
			.producer
			.claim(name, moq_net::origin::RouteOptions::default())
			.unwrap();
		for cursor in &mut covering.cursors {
			assert_eq!(cursor.next().now_or_never().flatten().is_some(), delivered);
		}
		drop(route);
		for cursor in &mut covering.cursors {
			assert_eq!(cursor.next().now_or_never().flatten().is_some(), delivered);
		}
	}
}

/// `routes` routes, and `readers` cursors stalled on their initial replay.
struct Pending {
	_cursors: Vec<origin::Routes>,
	_routes: Vec<origin::Route>,
	_driver: origin::Driver,
	producer: origin::Producer,
	key: u64,
}

impl Pending {
	/// Add a route and cancel it before any cursor sees it.
	fn churn(&mut self) {
		self.key += 1;
		drop(
			self.producer
				.route(format!("a/{}", self.key), moq_net::origin::RouteOptions::default())
				.unwrap(),
		);
	}
}

fn pending_cases() -> Vec<Case<(usize, usize)>> {
	cases(
		[10, 1_000]
			.into_iter()
			.flat_map(|routes| [1, 100].map(|readers| (routes, readers))),
		|(routes, readers)| format!("{routes}routes_{readers}readers"),
	)
}

fn stalled(case: Case<(usize, usize)>) -> Pending {
	let (routes, readers) = case.of;
	let (producer, driver) = origin::Producer::new(origin::Config::default());
	let routes_held = (0..routes)
		.map(|i| {
			producer
				.route(format!("z/{i}"), moq_net::origin::RouteOptions::default())
				.unwrap()
		})
		.collect();
	let cursors = (0..readers).map(|_| producer.consume().routes()).collect();
	let mut pending = Pending {
		_cursors: cursors,
		_routes: routes_held,
		_driver: driver,
		producer,
		key: 0,
	};
	// A cursor that stalled through a churn still replays only the table.
	let mut probe = pending.producer.consume().routes();
	pending.churn();
	assert_eq!(std::iter::from_fn(|| probe.try_next()).count(), routes);
	pending
}

/// Cancel unseen additions while every cursor stalls on its initial replay.
#[bench(args = pending_cases(), setup = stalled)]
fn pending_churn(pending: &mut Pending) {
	pending.churn();
}

/// `publishers` peer routes about to withdraw, and `subscribers` cursors watching.
struct Withdraw {
	cursors: Vec<origin::Routes>,
	routes: Vec<origin::Claim>,
	_driver: origin::Driver,
	_producer: origin::Producer,
	publishers: usize,
}

fn withdraw_cases() -> Vec<Case<(usize, usize)>> {
	cases([(100, 1), (1_000, 1), (10_000, 1), (1_000, 10)], |(p, s)| {
		format!("{p}p_{s}s")
	})
}

fn withdrawing(case: Case<(usize, usize)>) -> Withdraw {
	let (publishers, subscribers) = case.of;
	let (producer, mut driver) = origin::Producer::new(origin::Config::default());
	// Retractions read the driver's last clock reading, which stays put below.
	driver
		.poll(moq_net::time::Instant::now(), &kio::Waiter::noop())
		.unwrap();
	let routes = (0..publishers)
		.map(|i| {
			moq_net::fuzz::requests_route(
				&producer,
				moq_net::Path::new(&format!("room/{i}")).unwrap(),
				peer_route(9, 1),
			)
			.unwrap()
		})
		.collect();
	let mut cursors: Vec<_> = (0..subscribers).map(|_| producer.consume().routes()).collect();
	for cursor in &mut cursors {
		while cursor.next().now_or_never().flatten().is_some() {}
	}
	Withdraw {
		cursors,
		routes,
		_driver: driver,
		_producer: producer,
		publishers,
	}
}

/// A peer carrying `publishers` broadcasts withdraws them all before the
/// driver's clock moves again, then `subscribers` cursors drain the retractions.
/// Each retraction quarantines its prefix until a fresh clock reading, so a cost
/// per earlier retraction shows up as a slope over publishers.
#[bench(args = withdraw_cases(), setup = withdrawing)]
fn withdraw(mut withdraw: Withdraw) -> Withdraw {
	drop(std::mem::take(&mut withdraw.routes));
	for cursor in &mut withdraw.cursors {
		for _ in 0..withdraw.publishers {
			cursor.next().now_or_never().flatten().expect("retract delivered");
		}
	}
	withdraw
}

/// Two peers carrying the same broadcasts, and the cursors watching them.
struct Wave {
	cursors: Vec<origin::Routes>,
	far: Vec<origin::Claim>,
	near: Vec<origin::Claim>,
	_driver: origin::Driver,
	_producer: origin::Producer,
}

fn wave_cases() -> Vec<Case<(usize, usize)>> {
	cases(
		[1_000, 10_000].into_iter().flat_map(|b| [1, 20, 100].map(|s| (b, s))),
		|(b, s)| format!("{b}b_{s}s"),
	)
}

fn waving(case: Case<(usize, usize)>) -> Wave {
	let (broadcasts, subscribers) = case.of;
	let identity: Identity = "01900000-0000-7000-8000-000000000001".parse().unwrap();
	// One identity, so the winner moving between the peers is an update, which is held.
	let route = |chain: &[u64]| moq_net::fuzz::route_info(chain.iter().copied(), 0, Some((identity.clone(), 0)));
	let (producer, mut driver) = origin::Producer::new(origin::Config::default());
	// The holds read the driver's last clock reading, which stays put below.
	driver
		.poll(moq_net::time::Instant::now(), &kio::Waiter::noop())
		.unwrap();
	let peer = |chain: &[u64]| -> Vec<origin::Claim> {
		(0..broadcasts)
			.map(|i| {
				moq_net::fuzz::requests_route(
					&producer,
					moq_net::Path::new(&format!("room/{i}")).unwrap(),
					route(chain),
				)
				.unwrap()
			})
			.collect()
	};
	let near = peer(&[9, 2]);
	let far = peer(&[9, 3, 4]);
	let mut cursors: Vec<_> = (0..subscribers).map(|_| producer.consume().routes()).collect();
	for cursor in &mut cursors {
		while cursor.try_next().is_some() {}
	}
	Wave {
		cursors,
		far,
		near,
		_driver: driver,
		_producer: producer,
	}
}

/// Two peers carry the same `broadcasts` of one publisher, and both withdraw them
/// a broadcast at a time, every cursor polled after each withdrawal as a session's
/// task would be. The first peer's withdrawals queue held updates; the second's, in
/// the opposite order, make each ready behind the held rest. A poll that walks what
/// it holds back shows up as a slope over broadcasts, multiplied by the cursors.
#[bench(args = wave_cases(), setup = waving, items = |case: Case<(usize, usize)>| 2 * case.of.0 * case.of.1)]
fn withdraw_wave(mut wave: Wave) -> Wave {
	for route in std::mem::take(&mut wave.near) {
		drop(route);
		for cursor in &mut wave.cursors {
			assert!(cursor.try_next().is_none(), "the update is held");
		}
	}
	for route in std::mem::take(&mut wave.far).into_iter().rev() {
		drop(route);
		for cursor in &mut wave.cursors {
			cursor.try_next().expect("retract delivered");
		}
	}
	wave
}

/// `broadcasts` peer routes, and `subscribers` cursors watching them.
struct Churn {
	cursors: Vec<origin::Routes>,
	_routes: Vec<origin::Claim>,
	producer: origin::Producer,
	_driver: origin::Driver,
}

impl Churn {
	/// A route that never wins arriving at `room/0` and leaving again.
	fn churn(&self) {
		drop(
			moq_net::fuzz::requests_route(&self.producer, moq_net::Path::from_static("room/0"), peer_route(2, 5))
				.unwrap(),
		)
	}
}

fn churning(case: Case<(usize, usize)>) -> Churn {
	let (broadcasts, subscribers) = case.of;
	let (producer, driver) = origin::Producer::new(origin::Config::default());
	let routes = (0..broadcasts)
		.map(|i| {
			moq_net::fuzz::requests_route(
				&producer,
				moq_net::Path::new(&format!("room/{i}")).unwrap(),
				peer_route(1, 1),
			)
			.unwrap()
		})
		.collect();
	let mut cursors: Vec<_> = (0..subscribers).map(|_| producer.consume().routes()).collect();
	for cursor in &mut cursors {
		while cursor.try_next().is_some() {}
	}
	let mut churn = Churn {
		cursors,
		_routes: routes,
		producer,
		_driver: driver,
	};
	churn.churn();
	for cursor in &mut churn.cursors {
		assert!(cursor.try_next().is_none(), "the winner is unchanged");
	}
	churn
}

/// A route that never wins arriving at one broadcast's prefix and leaving again,
/// with `broadcasts` in the table and `subscribers` cursors watching. Each change
/// revisits every cursor presenting the prefix and finds its winner unchanged,
/// so a cost per cursor shows up as a slope over subscribers, and one per route
/// in the table as a slope over broadcasts.
#[bench(args = wave_cases(), setup = churning)]
fn route_churn(churn: &Churn) {
	churn.churn();
}

/// `publishers` routes under `.svc/p0`, and a cursor per project mounting it.
struct Mounted {
	cursors: Vec<origin::Routes>,
	_publishers: Vec<origin::Route>,
	_driver: origin::Driver,
	producer: origin::Producer,
}

fn mounted(case: Case<(usize, usize)>) -> Mounted {
	let (publishers, subscribers) = case.of;
	let (producer, driver) = origin::Producer::new(origin::Config::default());
	let publishers = (0..publishers)
		.map(|i| {
			producer
				.route(format!(".svc/p0/{i}"), moq_net::origin::RouteOptions::default())
				.unwrap()
		})
		.collect();
	let mut cursors: Vec<origin::Routes> = (0..subscribers)
		.map(|project| {
			producer
				.mount(format!("p{project}/.svc"), ".svc/p0")
				.unwrap()
				.scope(format!("p{project}"), &Prefixes::all())
				.unwrap()
				.consume()
				.with_hidden(true)
				.routes()
		})
		.collect();
	for cursor in &mut cursors {
		while cursor.next().now_or_never().flatten().is_some() {}
	}
	Mounted {
		cursors,
		_publishers: publishers,
		_driver: driver,
		producer,
	}
}

/// `announce` read through mounts: `publishers` routes under the fleet-wide
/// `.svc/p0`, watched by `subscribers` project sessions that each mount it at
/// their own `<project>/.svc`. Every mount aliases the one target, the worst
/// case, so each announcement reaches every mounted cursor. Compare against
/// `announce` for what a mount adds per cursor.
#[bench(args = shapes(), setup = mounted)]
fn announce_mounted(mounted: &mut Mounted) {
	let handle = mounted
		.producer
		.route(".svc/p0/incoming", moq_net::origin::RouteOptions::default())
		.unwrap();
	for cursor in &mut mounted.cursors {
		cursor.next().now_or_never().flatten().expect("announce delivered");
	}
	drop(handle);
	for cursor in &mut mounted.cursors {
		cursor.next().now_or_never().flatten().expect("retract delivered");
	}
}

/// Every project's stats on every node, and a cursor per peer scoped to one project.
struct Fleet {
	_cursors: Vec<origin::Routes>,
	_routes: Vec<origin::Route>,
	_consumer: origin::Consumer,
	_driver: origin::Driver,
	producer: origin::Producer,
}

fn fleet_cases() -> Vec<Case<(usize, usize, usize)>> {
	// (projects, nodes, peers). The middle row is a live fleet's shape.
	cases([(1, 8, 4), (4, 30, 30), (8, 30, 60)], |(p, n, c)| {
		format!("{p}p_{n}n_{c}c")
	})
}

fn stats_fleet(case: Case<(usize, usize, usize)>) -> Fleet {
	let (projects, nodes, peers) = case.of;
	let (producer, driver) = origin::Producer::new(origin::Config::default());
	let consumer = producer.consume();
	let routes = (0..projects)
		.flat_map(|project| (0..nodes).map(move |node| (project, node)))
		.map(|(project, node)| {
			producer
				.route(
					format!(".stats/p{project}/node/edge{node}"),
					moq_net::origin::RouteOptions::default(),
				)
				.unwrap()
		})
		.collect();
	let cursors = (0..peers)
		.map(|peer| {
			let prefixes = Prefixes::from(Prefix::try_from(format!(".stats/p{}", peer % projects)).unwrap());
			consumer.scope("", &prefixes).unwrap().routes()
		})
		.collect();
	Fleet {
		_cursors: cursors,
		_routes: routes,
		_consumer: consumer,
		_driver: driver,
		producer,
	}
}

/// A relay's own stats fan-out: `.stats/<project>/node/<node>` for every project
/// on every node, watched by one cursor per peer, each scoped to one project.
/// Cursors differ in scope, so none can be collapsed, and an announcement under
/// one project must not touch the peers watching another.
#[bench(args = fleet_cases(), setup = stats_fleet)]
fn announce_fleet(fleet: &Fleet) {
	let handle = fleet
		.producer
		.route(".stats/p0/node/incoming", moq_net::origin::RouteOptions::default())
		.unwrap();
	drop(handle);
}

/// [`CONTENDED`], named `<duplicates>d_<subscribers>s`.
fn contended() -> Vec<Case<(usize, usize)>> {
	cases(CONTENDED, |(d, s)| format!("{d}d_{s}s"))
}

/// `duplicates` peer routes at [`PATH`], and `subscribers` cursors watching it.
struct Duplicate {
	cursors: Vec<origin::Routes>,
	_routes: Vec<origin::Claim>,
	_consumer: origin::Consumer,
	_driver: origin::Driver,
	producer: origin::Producer,
	duplicates: usize,
}

fn duplicated(case: Case<(usize, usize)>) -> Duplicate {
	let (duplicates, subscribers) = case.of;
	// No hold, so a call times the ranking rather than the origin batching it.
	let config = origin::Config::default().with_update_hold(Duration::ZERO);
	let (producer, driver) = origin::Producer::new(config);
	let consumer = producer.consume();
	// Every peer announces the one path, each under its own hop chain so
	// the entries are distinct routes rather than one re-priced in place.
	let routes = (1..=duplicates)
		.map(|peer| {
			moq_net::fuzz::requests_route(
				&producer,
				moq_net::Path::new(PATH).unwrap(),
				peer_route(peer as u64, INCUMBENT_COST),
			)
			.unwrap()
		})
		.collect();
	let mut cursors: Vec<origin::Routes> = (0..subscribers)
		.map(|_| consumer.clone().with_hidden(true).routes())
		.collect();
	for cursor in &mut cursors {
		while cursor.next().now_or_never().flatten().is_some() {}
	}
	Duplicate {
		cursors,
		_routes: routes,
		_consumer: consumer,
		_driver: driver,
		producer,
		duplicates,
	}
}

/// One route flapping at a prefix that `duplicates` others already cover.
///
/// `announce_fleet` gives every path a single announcer, which is the shape a
/// publisher produces. A mesh produces the other one: the same path arrives
/// once per peer it can travel through, so a prefix carries one entry per peer.
/// The trie narrows a change to the touched prefix, but picking the winner
/// there shares the ranking across its watching cursors, so both are swept.
///
/// The arriving route is priced below every incumbent so it takes the prefix
/// outright: each cursor is told twice per call, once for the new winner
/// and once for the incumbent taking the prefix back when it retracts. Equal
/// costs would instead tie-break on a hash of the hop chain, which decides the
/// winner but is not what a reconnecting peer does.
#[bench(args = contended(), setup = duplicated)]
fn announce_duplicate(duplicate: &mut Duplicate) {
	let handle = moq_net::fuzz::requests_route(
		&duplicate.producer,
		moq_net::Path::new(PATH).unwrap(),
		peer_route(duplicate.duplicates as u64 + 1, INCUMBENT_COST - 1),
	)
	.unwrap();
	for cursor in &mut duplicate.cursors {
		cursor.next().now_or_never().flatten().expect("announce delivered");
	}
	drop(handle);
	for cursor in &mut duplicate.cursors {
		cursor.next().now_or_never().flatten().expect("incumbent restored");
	}
}

/// Incumbents split across two grants, a challenger filed through both, and cursors
/// scoped to one grant each.
struct Reprice {
	cursors: Vec<origin::Routes>,
	route: origin::Claim,
	_both: origin::Producer,
	_routes: Vec<origin::Claim>,
	_driver: origin::Driver,
	_producer: origin::Producer,
	challenger: u64,
	incumbent_cost: u64,
}

fn reprice_cases() -> Vec<Case<(u64, usize, usize)>> {
	let shapes = [INCUMBENT_COST, MAX_COST - 1]
		.into_iter()
		.flat_map(|cost| CONTENDED.map(|(d, s)| (cost, d, s)));
	cases(shapes, |(cost, d, s)| format!("{d}d_{s}s_cost{cost}"))
}

fn repriced(case: Case<(u64, usize, usize)>) -> Reprice {
	let (incumbent_cost, duplicates, subscribers) = case.of;
	// No hold, so a call times the ranking rather than the origin batching it.
	let config = origin::Config::default().with_update_hold(Duration::ZERO);
	let (producer, driver) = origin::Producer::new(config);
	let routes = (1..=duplicates)
		.map(|peer| {
			let scoped = producer
				.scope(
					"",
					&Prefixes::from(Prefix::try_from(format!("{PATH}/p{}", (peer - 1) % 2)).unwrap()),
				)
				.unwrap();
			moq_net::fuzz::requests_route(
				&scoped,
				moq_net::Path::new(PATH).unwrap(),
				peer_route(peer as u64, incumbent_cost),
			)
			.unwrap()
		})
		.collect();
	// Through both grants, so the challenger is filed beside the incumbents at each.
	let both = producer
		.scope(
			"",
			&(0..2)
				.map(|i| Prefix::try_from(format!("{PATH}/p{i}")).unwrap())
				.collect::<Prefixes>(),
		)
		.unwrap();
	let challenger = duplicates as u64 + 1;
	let route = moq_net::fuzz::requests_route(
		&both,
		moq_net::Path::new(PATH).unwrap(),
		peer_route(challenger, incumbent_cost + 1),
	)
	.unwrap();
	let mut cursors: Vec<origin::Routes> = (0..subscribers)
		.map(|peer| {
			producer
				.consume()
				.scope(
					"",
					&Prefixes::from(Prefix::try_from(format!("{PATH}/p{}", peer % 2)).unwrap()),
				)
				.unwrap()
				.with_hidden(true)
				.covering()
				.routes()
		})
		.collect();
	for cursor in &mut cursors {
		while cursor.next().now_or_never().flatten().is_some() {}
	}
	Reprice {
		cursors,
		route,
		_both: both,
		_routes: routes,
		_driver: driver,
		_producer: producer,
		challenger,
		incumbent_cost,
	}
}

/// Re-price a live mesh route with cursors watching different scopes.
/// Each claim is broader than its grant, so it is filed at each granted prefix and
/// contends there. The challenger alternates between winning and losing, so selection
/// must honor each cursor's scope rather than reuse one global winner.
#[bench(args = reprice_cases(), setup = repriced)]
fn reprice_duplicate(reprice: &mut Reprice) {
	let incumbent_cost = reprice.incumbent_cost;
	for cost in [incumbent_cost - 1, incumbent_cost + 1] {
		moq_net::fuzz::update_route(&reprice.route, peer_route(reprice.challenger, cost)).unwrap();
		for cursor in &mut reprice.cursors {
			// The winner switches between unnamed routes, which is another identity.
			let event = cursor.next().now_or_never().flatten().expect("winner changed");
			let moq_net::origin::RouteEvent {
				kind: moq_net::origin::RouteKind::Restart,
				announce: update,
				..
			} = event
			else {
				panic!("expected a restart: got {event:?}");
			};
			assert_eq!(
				update.route.cost(),
				moq_net::origin::Cost::new(cost.min(incumbent_cost))
			);
		}
	}
}

/// The contended path: one node's stats feed, which every peer in the mesh
/// carries a route to.
const PATH: &str = ".stats/p0/node/edge0";

/// What every incumbent route costs, leaving room for a cheaper challenger.
const INCUMBENT_COST: u64 = 2;

/// The wire ceiling a route cost saturates at: an incumbent just below it leaves room
/// for a costlier challenger.
const MAX_COST: u64 = (1 << 62) - 1;

/// A route as `peer` would have announced it: one hop, at `cost`.
fn peer_route(peer: u64, cost: u64) -> origin::RouteInfo {
	peer_identity(peer, cost, None)
}

/// [`peer_route`], serving `identity` when it names one.
fn peer_identity(peer: u64, cost: u64, identity: Option<Identity>) -> origin::RouteInfo {
	moq_net::fuzz::route_info([peer], cost, identity.map(|identity| (identity, 0)))
}

/// `fronts` remote fronts parked on their upstream request under a served prefix.
struct Fronts {
	waiter: kio::Waiter,
	_requests: Vec<kio::Pending<origin::Requesting>>,
	_served: origin::Claim,
	driver: origin::Driver,
	producer: origin::Producer,
}

fn parked_fronts(case: Case<usize>) -> Fronts {
	let fronts = case.of;
	let (producer, mut driver) = origin::Producer::new(origin::Config::default());
	let consumer = producer.consume();
	// Served but never answered: every request under it parks a front.
	let served = producer
		.claim("room", moq_net::origin::RouteOptions::default())
		.unwrap();
	let requests = (0..fronts)
		.map(|i| consumer.request(format!("room/{i}"), None))
		.collect();
	let waiter = kio::Waiter::noop();
	// Run each front once so it parks on its upstream request.
	driver.poll(moq_net::time::Instant::now(), &waiter).unwrap();
	Fronts {
		waiter,
		_requests: requests,
		_served: served,
		driver,
		producer,
	}
}

/// `fronts` held open, named `<fronts>f`.
fn front_cases() -> Vec<Case<usize>> {
	cases([100, 1_000, 10_000], |f| format!("{f}f"))
}

/// An announcement of an unrelated prefix with `fronts` remote fronts parked on
/// their upstream request. Each front watches only the routes covering its own
/// path, so none of them wakes; the driver poll after each change runs whatever
/// did. Sweeps fronts, so a per-front wake shows up as a slope.
#[bench(args = front_cases(), setup = parked_fronts)]
fn announce_fronts(fronts: &mut Fronts) {
	let handle = fronts
		.producer
		.route("other/incoming", moq_net::origin::RouteOptions::default())
		.unwrap();
	fronts
		.driver
		.poll(moq_net::time::Instant::now(), &fronts.waiter)
		.unwrap();
	drop(handle);
	fronts
		.driver
		.poll(moq_net::time::Instant::now(), &fronts.waiter)
		.unwrap();
}

/// Answer the claim's request for `path`, dropping any other it queued.
fn answer(claim: &origin::Claim, path: &str) -> origin::Route {
	loop {
		let request = claim
			.requested()
			.now_or_never()
			.expect("the request reached the claim")
			.unwrap();
		if request.path().as_str() == path {
			return request.route(moq_net::origin::RouteOptions::default()).unwrap();
		}
	}
}

/// A claim at `room` that answered `publishers` paths, and `subscribers` cursors watching.
struct Lend {
	cursors: Vec<origin::Routes>,
	answered: Vec<(kio::Pending<origin::Requesting>, origin::Route)>,
	waiter: kio::Waiter,
	claim: origin::Claim,
	consumer: origin::Consumer,
	driver: origin::Driver,
	_producer: origin::Producer,
	n: usize,
}

impl Lend {
	/// Request `path` and answer it through the claim.
	fn ask(&mut self, path: &str) -> (kio::Pending<origin::Requesting>, origin::Route) {
		let request = self.consumer.request(path, None);
		self.driver.poll(moq_net::time::Instant::now(), &self.waiter).unwrap();
		(request, answer(&self.claim, path))
	}
}

fn lending(case: Case<(usize, usize)>) -> Lend {
	let (publishers, subscribers) = case.of;
	let (producer, driver) = origin::Producer::new(origin::Config::default());
	let consumer = producer.consume();
	let claim = producer
		.claim("room", moq_net::origin::RouteOptions::default())
		.unwrap();
	let mut lend = Lend {
		cursors: Vec::new(),
		answered: Vec::with_capacity(publishers),
		waiter: kio::Waiter::noop(),
		claim,
		consumer,
		driver,
		_producer: producer,
		n: 0,
	};
	for i in 0..publishers {
		let answered = lend.ask(&format!("room/{i}"));
		lend.answered.push(answered);
	}
	lend.cursors = (0..subscribers).map(|_| lend.consumer.routes()).collect();
	for cursor in &mut lend.cursors {
		while cursor.next().now_or_never().flatten().is_some() {}
	}
	lend
}

/// A claim's answer delivered to every cursor as a start, then retracted, which moves the
/// claim to a fresh identity. Swept over the paths the claim already answered and the
/// cursors watching it, so finding the lending claim by walking the table rather than the
/// path's covering routes shows up as a slope.
#[bench(args = shapes(), setup = lending)]
fn lend(lend: &mut Lend) {
	lend.n += 1;
	let (request, output) = lend.ask(&format!("room/incoming{}", lend.n));
	for cursor in &mut lend.cursors {
		let event = cursor.next().now_or_never().flatten();
		assert!(
			matches!(
				event,
				Some(origin::RouteEvent {
					kind: origin::RouteKind::Start,
					..
				})
			),
			"{event:?}"
		);
	}
	drop(request);
	drop(output);
	for cursor in &mut lend.cursors {
		cursor.next().now_or_never().flatten().expect("retract delivered");
		cursor
			.next()
			.now_or_never()
			.flatten()
			.expect("the claim's advance delivered");
	}
}

/// One claim per announced path, exactly as a session lands a peer's.
struct Serve {
	dynamics: Vec<origin::Claim>,
	_driver: origin::Driver,
	_producer: origin::Producer,
}

fn serving(case: Case<usize>) -> Serve {
	let (producer, driver) = origin::Producer::new(origin::Config::default());
	let dynamics = (0..case.of)
		.map(|i| {
			producer
				.claim(
					format!(".stats/p{}/node/edge{i}", i % 8),
					moq_net::origin::RouteOptions::default(),
				)
				.unwrap()
		})
		.collect();
	Serve {
		dynamics,
		_driver: driver,
		_producer: producer,
	}
}

/// One serve sweep over the routes attached to a single announce stream.
///
/// `lite::subscriber`'s serve loop registers *one* waiter, the announce stream
/// machine's, on *every* attached route's request queue, then re-sweeps all of
/// them whenever it wakes. So the sweep fans in: a request arriving on any one
/// route, or one more announce landing during convergence, re-polls every other
/// route's queue, each taking its lock and re-registering the waiter. An idle
/// sweep that serves nothing still costs one lock per attached route.
///
/// A mesh makes `routes` large: a peer announcing `.stats/<project>/node/<node>`
/// for every project on every node attaches one route per path to one stream.
/// This measures a single sweep, so the convergence cost of a reconnecting peer
/// (one sweep per announce, over a table growing to `routes`) reads off it as
/// the sum.
#[bench(args = cases([30, 300, 3_000], |r| format!("{r}r")), setup = serving, items = |case: Case<usize>| case.of)]
fn serve_idle(serve: &Serve) {
	// A fresh waiter per sweep, so every poll pays a real registration: a
	// reused one would find itself still parked on each route and skip it.
	let waiter = kio::Waiter::noop();
	// Nothing is queued, so every poll parks again: the idle sweep.
	for dynamic in &serve.dynamics {
		assert!(dynamic.poll_requested(&waiter).is_pending());
	}
}

/// A new subscriber registering against `publishers` routes and draining the
/// replay: the one operation whose cost legitimately scales with what it watches.
#[bench(args = shapes(), setup = fleet)]
fn subscribe(fleet: &Fanout) -> usize {
	let mut cursor = fleet.consumer.routes();
	let mut replayed = 0;
	while let Some(origin::RouteEvent {
		kind: origin::RouteKind::Start,
		..
	}) = cursor.next().now_or_never().flatten()
	{
		replayed += 1;
	}
	assert_eq!(replayed, fleet._publishers.len());
	replayed
}

/// Resolving one broadcast among `publishers`: a local hit walks the table to
/// the exact path and joins the front serving it, and a miss under a broadcast
/// published above it walks the table to prove nothing serves it. Neither may
/// depend on `publishers`.
mod request {
	use swole::bench;

	use super::*;

	/// `publishers` routes under a broadcast published at `room`, and how many request each call.
	struct Requests {
		_covering: origin::Route,
		fleet: Fanout,
		waiter: kio::Waiter,
		requesters: usize,
	}

	fn request_cases() -> Vec<Case<(usize, usize)>> {
		cases(
			[100, 1_000].into_iter().flat_map(|p| [1, 8, 32].map(|r| (p, r))),
			|(p, r)| format!("{p}p_{r}r"),
		)
	}

	fn covered(case: Case<(usize, usize)>) -> Requests {
		let (publishers, requesters) = case.of;
		let fleet = fanout(publishers, 0);
		let covering = fleet
			.producer
			.route("room", moq_net::origin::RouteOptions::default())
			.unwrap();
		Requests {
			_covering: covering,
			fleet,
			waiter: kio::Waiter::noop(),
			requesters,
		}
	}

	#[bench(args = request_cases(), setup = covered, items = |case: Case<(usize, usize)>| case.of.1)]
	fn local(requests: &mut Requests) {
		let pending: Vec<_> = (0..requests.requesters)
			.map(|_| requests.fleet.consumer.request("room/0", None))
			.collect();
		requests
			.fleet
			.driver
			.poll(moq_net::time::Instant::now(), &requests.waiter)
			.unwrap();
		for pending in pending {
			pending
				.now_or_never()
				.expect("resolves once driven")
				.expect("local broadcast");
		}
	}

	#[bench(args = request_cases(), setup = covered, items = |case: Case<(usize, usize)>| case.of.1)]
	fn unroutable(requests: &mut Requests) {
		for _ in 0..requests.requesters {
			let result = requests
				.fleet
				.consumer
				.request("room/missing", None)
				.now_or_never()
				.expect("fails synchronously");
			assert!(matches!(result, Err(moq_net::Error::Unroutable)));
		}
	}
}

/// `fronts` fronts held open, and the path of one more local broadcast.
struct Retire {
	_held: Vec<broadcast::Consumer>,
	fleet: Fanout,
	waiter: kio::Waiter,
	path: String,
}

fn retiring(case: Case<usize>) -> Retire {
	let fronts = case.of;
	let mut fleet = fanout(fronts + 1, 0);
	let waiter = kio::Waiter::noop();
	let held: Vec<_> = (0..fronts)
		.map(|i| fleet.consumer.request(format!("room/{i}"), None))
		.collect();
	fleet.driver.poll(moq_net::time::Instant::now(), &waiter).unwrap();
	let held = held
		.into_iter()
		.map(|pending| pending.now_or_never().expect("resolved once driven").expect("local"))
		.collect();
	Retire {
		_held: held,
		fleet,
		waiter,
		path: format!("room/{fronts}"),
	}
}

/// A front minted, resolved, and retired once its holder leaves, among `fronts`
/// others held open. A retiring front leaves the table it shares with them, so a
/// cost that grows with the table rather than with the one path shows up as a slope.
#[bench(args = front_cases(), setup = retiring)]
fn retire(retire: &mut Retire) {
	let pending = retire.fleet.consumer.request(&retire.path, None);
	retire
		.fleet
		.driver
		.poll(moq_net::time::Instant::now(), &retire.waiter)
		.unwrap();
	drop(pending.now_or_never().expect("resolved once driven").expect("local"));
	// Nothing holds it and a local source keeps no track, so it retires here.
	retire
		.fleet
		.driver
		.poll(moq_net::time::Instant::now(), &retire.waiter)
		.unwrap();
}

/// `(members, paths)` shapes for an equal-cost pool: how many advertisers claim
/// one prefix, against how many paths beneath it are already being served.
const POOL: [usize; 3] = [4, 32, 256];
const POOL_PATHS: [usize; 3] = [100, 1_000, 10_000];

/// Announce cursors watching the pool, against [`POOL_PATHS`].
const POOL_CURSORS: [usize; 3] = [1, 100, 1_000];

/// Hop ids for pool members, clear of the origin's own.
const POOL_HOP: u64 = 1_000;

/// An origin where `members` equal-cost advertisers claim `pool`, already
/// serving `paths` requested paths beneath it, spread across the pool by the
/// hash on each path, and watched by `cursors` announce cursors. Every handle
/// is held so the fronts stay up.
struct Pool {
	producer: origin::Producer,
	driver: origin::Driver,
	_members: Vec<origin::Claim>,
	_producers: Vec<broadcast::Producer>,
	_consumers: Vec<broadcast::Consumer>,
	cursors: Vec<origin::Routes>,
}

impl Pool {
	/// Discard what the cursors were delivered, so each iteration measures only what it adds.
	fn drain(&mut self) {
		for cursor in &mut self.cursors {
			while cursor.next().now_or_never().flatten().is_some() {}
		}
	}
}

fn pool(members: usize, paths: usize, cursors: usize) -> Pool {
	let (producer, mut driver) = origin::Producer::new(origin::Config::new(std::num::NonZeroU64::new(1).unwrap()));
	let consumer = producer.consume();
	let cursors = (0..cursors).map(|_| consumer.routes()).collect();
	let members: Vec<_> = (0..members)
		.map(|i| {
			moq_net::fuzz::requests_route(
				&producer,
				moq_net::Path::from_static("pool"),
				pool_route(POOL_HOP + i as u64),
			)
			.unwrap()
		})
		.collect();
	let waiter = kio::Waiter::noop();

	let requests: Vec<_> = (0..paths)
		.map(|i| consumer.request(format!("pool/job-{i}"), None))
		.collect();
	driver.poll(moq_net::time::Instant::now(), &waiter).unwrap();
	let mut producers = Vec::with_capacity(paths);
	for member in &members {
		while let Poll::Ready(Some(request)) = member.poll_requested(&waiter) {
			let broadcast = broadcast::Info::new().produce();
			request.serve(&broadcast);
			producers.push(broadcast);
		}
	}
	assert_eq!(producers.len(), paths, "every path reached a member");
	driver.poll(moq_net::time::Instant::now(), &waiter).unwrap();
	let consumers = requests
		.into_iter()
		.map(|request| request.now_or_never().expect("resolved once driven").expect("served"))
		.collect();

	let mut pool = Pool {
		producer,
		driver,
		_members: members,
		_producers: producers,
		_consumers: consumers,
		cursors,
	};
	pool.drain();
	pool
}

/// A pool member's claim: one hop, at the same cost as every other member.
fn pool_route(hop: u64) -> origin::RouteInfo {
	peer_route(hop, 3)
}

/// A pool whose member changed identity, and the paths whose fronts it left stale.
struct Restart {
	pool: Pool,
	paths: Vec<String>,
}

fn restarting(case: Case<(usize, usize)>) -> Restart {
	let (fronts, restarting) = case.of;
	let pool = pool(1, fronts, 0);
	moq_net::fuzz::update_route(&pool._members[0], peer_identity(POOL_HOP, 3, Some(Identity::random()))).unwrap();
	Restart {
		pool,
		paths: (0..restarting).map(|i| format!("pool/job-{i}")).collect(),
	}
}

/// Re-requesting fronts an identity change left stale, swept over the cache around them and the
/// number restarted. Setup, route updates, and teardown stay outside the measurement.
#[bench(
	args = cases(
		[100, 1_000, 10_000].into_iter().flat_map(|f| [1, 10, 100].map(|r| (f, r))),
		|(f, r)| format!("{f}f_{r}r"),
	),
	setup = restarting,
	items = |case: Case<(usize, usize)>| case.of.1,
)]
fn restart_fronts(restart: Restart) -> (Vec<kio::Pending<origin::Requesting>>, Restart) {
	let consumer = restart.pool.producer.consume();
	let requests = restart.paths.iter().map(|path| consumer.request(path, None)).collect();
	(requests, restart)
}

/// A pool, and the hop of the member that joins and leaves it each call.
struct Churning {
	pool: Pool,
	waiter: kio::Waiter,
	hop: u64,
}

impl Churning {
	fn new(members: usize, paths: usize, cursors: usize) -> Self {
		Self {
			pool: pool(members, paths, cursors),
			waiter: kio::Waiter::noop(),
			hop: POOL_HOP + members as u64,
		}
	}

	/// A member joins and leaves again.
	fn churn(&mut self) {
		let joined = moq_net::fuzz::requests_route(
			&self.pool.producer,
			moq_net::Path::from_static("pool"),
			pool_route(self.hop),
		)
		.unwrap();
		self.pool
			.driver
			.poll(moq_net::time::Instant::now(), &self.waiter)
			.unwrap();
		drop(joined);
		self.pool
			.driver
			.poll(moq_net::time::Instant::now(), &self.waiter)
			.unwrap();
	}
}

fn members(case: Case<(usize, usize)>) -> Churning {
	Churning::new(case.of.0, case.of.1, 0)
}

/// A member joining and leaving a pool that already serves `paths` paths.
///
/// Every front under the prefix watches the routes covering it, so each one
/// re-selects on both changes, and each selection scans the pool: the cost is
/// paths times members by design, which this sweep makes visible. The fronts the
/// newcomer outranks request through it, and fall back when it leaves before
/// answering.
#[bench(
	args = cases(POOL.into_iter().flat_map(|m| POOL_PATHS.map(|p| (m, p))), |(m, p)| format!("{m}m_{p}p")),
	setup = members,
)]
fn pool_churn(churning: &mut Churning) {
	churning.churn();
}

fn watched_pool(case: Case<(usize, usize)>) -> Churning {
	Churning::new(32, case.of.0, case.of.1)
}

/// [`pool_churn`] at 32 members, watched by `cursors` announce cursors.
///
/// A front whose path the newcomer takes checks whether every cursor on the
/// prefix already restarted for it before renewing the prefix, so the moved
/// fronts times the cursors is the slope to watch.
#[bench(
	args = cases(POOL_PATHS.into_iter().flat_map(|p| POOL_CURSORS.map(|c| (p, c))), |(p, c)| format!("{p}p_{c}c")),
	setup = watched_pool,
)]
fn pool_cursors(churning: &mut Churning) {
	churning.churn();
	churning.pool.drain();
}

/// The incumbent a handoff replaces, and what the timed part leaves for teardown.
struct Round {
	incumbent: origin::Route,
	resolved: broadcast::Consumer,
	subscription: track::Subscriber,
	standby: Option<(origin::Route, broadcast::Consumer, track::Subscriber)>,
}

/// An origin with `publishers` unrelated broadcasts, its driver on the runtime.
struct Handoff {
	round: Option<Round>,
	_others: Vec<origin::Route>,
	consumer: origin::Consumer,
	producer: origin::Producer,
	runtime: tokio::runtime::Runtime,
}

fn handoff_origin(case: Case<usize>) -> Handoff {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap();
	let (producer, driver) = origin::Producer::new(origin::Config::default());
	// The driver runs whenever the runtime is entered below.
	runtime.spawn(moq_net::time::run(driver));
	let consumer = producer.consume();
	let others = (0..case.of)
		.map(|i| {
			producer
				.route(format!("room/{i}"), moq_net::origin::RouteOptions::default())
				.unwrap()
		})
		.collect();
	Handoff {
		round: None,
		_others: others,
		consumer,
		producer,
		runtime,
	}
}

/// Publish the incumbent and subscribe to it until its first group arrives.
fn incumbent(handoff: &mut Handoff) {
	let Handoff {
		round,
		consumer,
		producer,
		runtime,
		..
	} = handoff;
	*round = Some(runtime.block_on(async {
		let incumbent = producer
			.route("room/live", moq_net::origin::RouteOptions::default())
			.unwrap();
		let track = incumbent.broadcast().create_track("video", timed()).unwrap();
		let mut first = track.append_group(Timestamp::from_millis(0).unwrap()).unwrap();
		first.write_frame(Timestamp::ZERO, b"one".as_ref()).unwrap();
		first.finish().unwrap();

		let resolved = consumer.request("room/live", None).await.unwrap();
		let mut subscription = resolved.track("video").unwrap().subscribe(None).await.unwrap();
		subscription.recv_group().await.unwrap().expect("first group");
		Round {
			incumbent,
			resolved,
			subscription,
			standby: None,
		}
	}));
}

/// Close both broadcasts and wait for their fronts, so the next call starts fresh ones.
fn teardown(handoff: &mut Handoff) {
	let round = handoff.round.take().expect("a handoff ran");
	let (standby, replaced, replacement) = round.standby.expect("the standby took over");
	drop(round.subscription);
	drop(replacement);
	round.incumbent.broadcast().close();
	standby.broadcast().close();
	handoff.runtime.block_on(async {
		round.resolved.closed().await;
		replaced.closed().await;
	});
}

/// Publisher handoff at one path: a subscriber is reading from one local
/// source when a second announces at the same path and takes over (newest
/// wins). An open subscription stays on the old identity, so the subscriber
/// re-requests the broadcast. Measured from the standby's attach to the
/// re-request receiving its first group, with `publishers` unrelated
/// broadcasts in the table.
#[bench(
	args = cases([1, 1_000], |p| format!("{p}p")),
	setup = handoff_origin,
	before = incumbent,
	after = teardown,
)]
fn handoff(handoff: &mut Handoff) {
	let Handoff {
		round,
		consumer,
		producer,
		runtime,
		..
	} = handoff;
	let round = round.as_mut().expect("an incumbent is live");
	round.standby = Some(runtime.block_on(async {
		let standby = producer
			.route("room/live", moq_net::origin::RouteOptions::hidden())
			.unwrap();
		let track = standby.broadcast().create_track("video", timed()).unwrap();
		let mut second = track.create_group(moq_net::group::Info { start: 1 }).unwrap();
		second.write_frame(at(1), b"two".as_ref()).unwrap();
		second.finish().unwrap();
		// Announcing is what makes the standby a route the front can take.
		standby.announce().unwrap();
		let replaced = consumer.request("room/live", None).await.unwrap();
		let mut replacement = replaced.track("video").unwrap().subscribe(None).await.unwrap();
		let group = replacement.recv_group().await.unwrap().expect("standby group");
		// Rejoining the incumbent would replay its cached group 0 instead.
		assert_eq!(group.start, 1, "standby group");
		(standby, replaced, replacement)
	}));
}

/// `(routes, subscribers)` shapes for the narrowing benchmarks: [`SHAPES`] plus a
/// row that grows the routes alone, so each axis shows its own slope.
const NARROW_SHAPES: [(usize, usize); 4] = [(100, 10), (1_000, 10), (1_000, 100), (1_000, 1_000)];

/// A relay serving `routes` broadcasts to one client session subscribed to
/// `subscribers` of them. The first subscription is the one a narrowing to
/// `room/**` ends: its broadcast sits at `muted` instead.
struct Watching {
	pair: MockPair,
	tracks: Vec<track::Producer>,
	subscriptions: Vec<track::Subscriber>,
	/// The next group's key on every track.
	next: u64,
	_broadcasts: Vec<origin::Route>,
	_origins: [origin::Producer; 2],
}

fn watched_path(index: usize) -> String {
	match index {
		0 => "muted".to_string(),
		index => format!("room/{index}"),
	}
}

async fn watching(routes: usize, subscribers: usize) -> Watching {
	let spawn = |hop| {
		let (producer, driver) = origin::Producer::new(origin::Config::new(std::num::NonZeroU64::new(hop).unwrap()));
		support::harness::spawn(driver);
		producer
	};
	let relay = spawn(1);
	let received = spawn(2);

	let mut broadcasts = Vec::new();
	let mut tracks = Vec::new();
	for index in 0..routes {
		let broadcast = relay
			.route(watched_path(index), moq_net::origin::RouteOptions::default())
			.unwrap();
		tracks.push(broadcast.broadcast().create_track("video", timed()).unwrap());
		broadcasts.push(broadcast);
	}

	let mut options = MockConnectOptions::new("moq-lite-06".parse().unwrap());
	options.server_publish = Some(relay.consume());
	options.client_subscribe = Some(received.clone());
	let pair = connect_mock(options).await;

	let mut subscriptions = Vec::new();
	for index in 0..subscribers {
		let remote = received
			.consume()
			.follow(watched_path(index), None)
			.unwrap()
			.available()
			.await
			.unwrap()
			.unwrap();
		let subscription = remote.track("video").unwrap().subscribe(None).await.unwrap();
		subscriptions.push(subscription);
	}
	// One round, so every subscription is live end to end before anything is timed.
	let mut watching = Watching {
		pair,
		tracks,
		subscriptions,
		next: 0,
		_broadcasts: broadcasts,
		_origins: [relay, received],
	};
	watching.round().await;
	watching
}

impl Watching {
	/// Every subscribed track writes a group, and every subscription reads it.
	async fn round(&mut self) {
		let key = self.next;
		self.next += 1;
		for track in &self.tracks[..self.subscriptions.len()] {
			let mut group = track.append_group(Timestamp::from_millis(key).unwrap()).unwrap();
			group.write_frame(at(key), b"frame".as_ref()).unwrap();
			group.finish().unwrap();
		}
		for subscription in &mut self.subscriptions {
			subscription.recv_group().await.unwrap().expect("track ended");
		}
	}
}

fn runtime() -> tokio::runtime::Runtime {
	tokio::runtime::Builder::new_current_thread()
		.enable_time()
		.build()
		.unwrap()
}

/// Everything under `room/`, which leaves out only the `muted` subscription.
fn room() -> auth::Grant {
	let mut grant = auth::Grant::all();
	grant.subscribe = Prefix::try_from("room").unwrap().into();
	grant
}

/// [`NARROW_SHAPES`], named `<routes>r_<subscribers>s`.
fn narrow_shapes() -> Vec<Case<(usize, usize)>> {
	cases(NARROW_SHAPES, |(r, s)| format!("{r}r_{s}s"))
}

/// A live session and the runtime its drivers run on.
struct Session {
	watching: Watching,
	rt: tokio::runtime::Runtime,
}

fn session(case: Case<(usize, usize)>) -> Session {
	let rt = runtime();
	let watching = rt.block_on(watching(case.of.0, case.of.1));
	Session { watching, rt }
}

/// Steady-state delivery through a session's gates: one round across every
/// subscription, on a session that never narrows and on one narrowed to a grant
/// that still covers them all. Neither may depend on `routes`.
mod narrow_delivery {
	use swole::bench;

	use super::*;

	fn narrowed_session(case: Case<(usize, usize)>) -> Session {
		let session = session(case);
		// Covers every subscription but the muted one, which is not subscribed here.
		let mut grant = room();
		grant.subscribe.insert(Prefix::try_from("muted").unwrap());
		session.watching.pair.server.auth().authorize(&grant);
		session
	}

	#[bench(args = narrow_shapes(), setup = session, items = |case: Case<(usize, usize)>| case.of.1)]
	fn never(session: &mut Session) {
		session.rt.block_on(session.watching.round());
	}

	#[bench(args = narrow_shapes(), setup = narrowed_session, items = |case: Case<(usize, usize)>| case.of.1)]
	fn narrowed(session: &mut Session) {
		session.rt.block_on(session.watching.round());
	}
}

/// One narrowing on a live session: from `narrow` until the subscription it
/// excludes resets. It wakes every subscription's gate and re-checks every route
/// the session announced, so it may grow with the session's own `subscribers`
/// and `routes`, never with anything else on the origin.
#[bench(args = narrow_shapes(), setup = session)]
fn narrow(mut session: Session) -> Session {
	session.watching.pair.server.auth().authorize(&room());
	let ended = session.rt.block_on(session.watching.subscriptions[0].recv_group());
	assert!(ended.is_err(), "the muted subscription outlived the narrowing");
	session
}

/// `(tracks, readers per track)` shapes for [`relay`].
const RELAY: [(usize, usize); 4] = [(1, 1), (1, 100), (100, 1), (10, 100)];

/// Frames per group in [`relay`].
const RELAY_FRAMES: usize = 10;

/// A front serving `tracks` tracks, each read by `readers` subscriptions.
struct Relay {
	subscriptions: Vec<track::Subscriber>,
	sources: Vec<track::Producer>,
	_broadcast: origin::Route,
	_producer: origin::Producer,
	runtime: tokio::runtime::Runtime,
	next: u64,
}

fn relaying(case: Case<(usize, usize)>) -> Relay {
	let (tracks, readers) = case.of;
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap();
	let (producer, driver) = origin::Producer::new(origin::Config::default());
	runtime.spawn(moq_net::time::run(driver));
	let broadcast = producer
		.route("room/live", moq_net::origin::RouteOptions::default())
		.unwrap();
	let sources = (0..tracks)
		.map(|i| broadcast.broadcast().create_track(format!("{i}"), timed()).unwrap())
		.collect();
	let subscriptions = runtime.block_on(async {
		let resolved = producer.consume().request("room/live", None).await.unwrap();
		let mut subscriptions = Vec::new();
		for i in 0..tracks {
			let track = resolved.track(&format!("{i}")).unwrap();
			for _ in 0..readers {
				subscriptions.push(track.subscribe(None).await.unwrap());
			}
		}
		subscriptions
	});
	Relay {
		subscriptions,
		sources,
		_broadcast: broadcast,
		_producer: producer,
		runtime,
		next: 0,
	}
}

/// Steady-state delivery through a front: one group of [`RELAY_FRAMES`] frames per
/// track, read in full by every reader of the broadcast the front serves. Readers
/// drain the serving route's shared group cache directly, so measure both the track
/// and reader axes.
#[bench(
	args = cases(RELAY, |(t, r)| format!("{t}t_{r}r")),
	setup = relaying,
	items = |case: Case<(usize, usize)>| case.of.0 * case.of.1 * RELAY_FRAMES,
)]
fn relay(relay: &mut Relay) {
	let Relay {
		subscriptions,
		sources,
		runtime,
		next,
		..
	} = relay;
	runtime.block_on(async {
		let key = *next;
		*next += 1;
		for source in sources.iter() {
			let mut group = source.append_group(Timestamp::from_millis(key).unwrap()).unwrap();
			for _ in 0..RELAY_FRAMES {
				group.write_frame(at(key), b"frame".as_ref()).unwrap();
			}
			group.finish().unwrap();
		}
		for subscription in subscriptions.iter_mut() {
			let mut group = subscription.recv_group().await.unwrap().expect("group");
			for _ in 0..RELAY_FRAMES {
				group.read_frame().await.unwrap().expect("frame");
			}
		}
	});
}

/// `(readers, open)` shapes for [`passthrough`]: readers of one track, and the
/// groups each holds open at once.
const PASSTHROUGH: [(usize, usize); 4] = [(1, 1), (1, 64), (100, 1), (100, 64)];

/// A front that only ever had one route, `readers` subscriptions, and the groups each holds.
struct Passthrough {
	held: Vec<Vec<moq_net::group::Consumer>>,
	subscriptions: Vec<track::Subscriber>,
	source: track::Producer,
	_broadcast: origin::Route,
	_producer: origin::Producer,
	runtime: tokio::runtime::Runtime,
	open: usize,
	next: u64,
}

fn passing(case: Case<(usize, usize)>) -> Passthrough {
	let (readers, open) = case.of;
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap();
	let (producer, driver) = origin::Producer::new(origin::Config::default());
	runtime.spawn(moq_net::time::run(driver));
	let broadcast = producer
		.route("room/live", moq_net::origin::RouteOptions::default())
		.unwrap();
	let source = broadcast.broadcast().create_track("0", timed()).unwrap();
	let subscriptions = runtime.block_on(async {
		let resolved = producer.consume().request("room/live", None).await.unwrap();
		let track = resolved.track("0").unwrap();
		let mut subscriptions = Vec::with_capacity(readers);
		for _ in 0..readers {
			subscriptions.push(track.subscribe(None).await.unwrap());
		}
		subscriptions
	});
	Passthrough {
		held: (0..readers).map(|_| Vec::with_capacity(open)).collect(),
		subscriptions,
		source,
		_broadcast: broadcast,
		_producer: producer,
		runtime,
		open,
		next: 0,
	}
}

/// One-frame groups read through a front that only ever had one route, so each handed-out
/// group's per-reader record (kept for a later route to back it) is a visible share of the
/// cost. Each reader holds `open` groups at once, the live count that record tracks.
#[bench(
	args = cases(PASSTHROUGH, |(r, g)| format!("{r}r_{g}g")),
	setup = passing,
	items = |case: Case<(usize, usize)>| case.of.0 * case.of.1,
)]
fn passthrough(passthrough: &mut Passthrough) {
	let Passthrough {
		held,
		subscriptions,
		source,
		runtime,
		open,
		next,
		..
	} = passthrough;
	runtime.block_on(async {
		// Received as each is written: a reader behind the live edge skips ahead.
		for _ in 0..*open {
			let key = *next;
			*next += 1;
			let mut group = source.append_group(Timestamp::from_millis(key).unwrap()).unwrap();
			group.write_frame(at(key), b"frame".as_ref()).unwrap();
			group.finish().unwrap();
			for (subscription, held) in subscriptions.iter_mut().zip(held.iter_mut()) {
				held.push(subscription.recv_group().await.unwrap().expect("group"));
			}
		}
		for held in held.iter_mut() {
			for group in held.iter_mut() {
				group.read_frame().await.unwrap().expect("frame");
			}
			held.clear();
		}
	});
}

/// Stalled groups through a front: every track has an open group with no frame yet.
struct Parked {
	_subscriptions: Vec<track::Subscriber>,
	groups: Vec<moq_net::group::Consumer>,
	waiter: kio::Waiter,
	_writers: Vec<moq_net::group::Producer>,
	_sources: Vec<track::Producer>,
	_broadcast: origin::Route,
	_producer: origin::Producer,
	_runtime: tokio::runtime::Runtime,
}

fn parked_groups(case: Case<(usize, usize)>) -> Parked {
	let (tracks, readers) = case.of;
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap();
	let (producer, driver) = origin::Producer::new(origin::Config::default());
	runtime.spawn(moq_net::time::run(driver));
	let broadcast = producer
		.route("room/live", moq_net::origin::RouteOptions::default())
		.unwrap();
	let sources: Vec<_> = (0..tracks)
		.map(|i| {
			broadcast
				.broadcast()
				.create_track(format!("{i}"), moq_net::track::Info::default().with_max_age(None))
				.unwrap()
		})
		.collect();
	let writers = sources
		.iter()
		.map(|source| source.append_group(Timestamp::unscaled(0)).unwrap())
		.collect();
	let (subscriptions, groups) = runtime.block_on(async {
		let resolved = producer.consume().request("room/live", None).await.unwrap();
		let mut subscriptions = Vec::new();
		let mut groups = Vec::new();
		for i in 0..tracks {
			let track = resolved.track(&format!("{i}")).unwrap();
			for _ in 0..readers {
				let mut sub = track.subscribe(None).await.unwrap();
				groups.push(sub.recv_group().await.unwrap().unwrap());
				subscriptions.push(sub);
			}
		}
		(subscriptions, groups)
	});
	Parked {
		_subscriptions: subscriptions,
		groups,
		waiter: kio::Waiter::noop(),
		_writers: writers,
		_sources: sources,
		_broadcast: broadcast,
		_producer: producer,
		_runtime: runtime,
	}
}

/// Re-poll stalled groups through a front, including their subscription budget.
/// Sweep tracks and readers separately to expose unrelated table scans.
#[bench(
	args = cases([1, 100].into_iter().flat_map(|t| [1, 100].map(|r| (t, r))), |(t, r)| format!("{t}t_{r}r")),
	setup = parked_groups,
	items = |case: Case<(usize, usize)>| case.of.0 * case.of.1,
)]
fn parked(parked: &mut Parked) {
	for group in &mut parked.groups {
		assert!(group.poll_read_frame(&parked.waiter).is_pending());
	}
}

/// `(tracks, copies)` for [`copy_walk`]: tracks one front is serving, and
/// route copies each of those tracks still holds when the route swaps.
///
/// The single-copy points from 32 to 128 tracks show the per-track slope alone.
const COPY_WALK: [(usize, usize); 8] = [(1, 1), (1, 32), (32, 1), (64, 1), (128, 1), (8, 32), (32, 8), (64, 16)];

const COPY_PATH: &str = "room/live";

/// How long a splice may take before the bench treats it as stuck.
const COPY_WAIT: Duration = Duration::from_secs(5);

/// One generation at [`COPY_PATH`], held so its copies stay subscribed.
struct Generation {
	broadcast: origin::Route,
	tracks: Vec<track::Producer>,
}

impl Generation {
	/// Create a hidden generation of `identity` and leave a finished group on every
	/// track, so a reader that does look has something to resume.
	fn build(producer: &origin::Producer, tracks: usize, identity: &Identity) -> Self {
		let broadcast = producer
			.route(
				COPY_PATH,
				moq_net::origin::RouteOptions::hidden().with_identity(identity.clone(), 0),
			)
			.unwrap();
		let mut held = Vec::with_capacity(tracks);
		for i in 0..tracks {
			let track = broadcast.broadcast().create_track(format!("{i}"), timed()).unwrap();
			let mut written = track.append_group(Timestamp::from_millis(0).unwrap()).unwrap();
			written.write_frame(Timestamp::ZERO, b"f".as_ref()).unwrap();
			written.finish().unwrap();
			held.push(track);
		}
		Self {
			broadcast,
			tracks: held,
		}
	}

	/// Route the front to this generation without waiting.
	fn route(&self) {
		self.broadcast.announce().unwrap();
	}

	/// Route the front to this generation and wait until every track is spliced.
	///
	/// The wait ends on demand, which flips when the front subscribes the new
	/// copy, before its splice. That bounds the splice only on a current-thread
	/// runtime, where every local query is ready within the front poll that
	/// splices it.
	async fn announce(&self) {
		self.route();
		for track in &self.tracks {
			kio::clock::timeout(COPY_WAIT, support::wanted(track.demand()))
				.await
				.expect("route swap did not splice")
				.expect("replacement track closed");
		}
	}
}

/// A front serving [`COPY_PATH`], with `copies` route copies on each of its tracks.
struct CopyWalk {
	_producer: origin::Producer,
	_subscribers: Vec<track::Subscriber>,
	generations: Vec<Generation>,
	/// The generation the timed swap announces, built during setup.
	next: Option<Generation>,
}

/// Build a front with `copies` already spliced onto each track.
///
/// Subscribers are never polled. A poll drops a replaced copy once its groups
/// are delivered, and this measures the walk over the copies a swap still has.
/// The same is why every generation is kept: dropping one ends its copy.
async fn copy_rig(tracks: usize, copies: usize) -> CopyWalk {
	assert!(copies >= 1, "a front holds at least the serving copy");
	let (producer, driver) = origin::Producer::new(origin::Config::default());
	tokio::spawn(moq_net::time::run(driver));
	let identity = Identity::random();
	let first = Generation::build(&producer, tracks, &identity);
	first.route();
	let resolved = kio::clock::timeout(COPY_WAIT, producer.consume().request(COPY_PATH, None))
		.await
		.expect("front did not resolve")
		.expect("front refused");
	let mut subscribers = Vec::with_capacity(tracks);
	for i in 0..tracks {
		let track = resolved.track(&format!("{i}")).unwrap();
		let subscriber = kio::clock::timeout(COPY_WAIT, track.subscribe(None))
			.await
			.expect("track did not splice")
			.expect("subscribe failed");
		subscribers.push(subscriber);
	}
	let mut generations = Vec::with_capacity(copies + 1);
	generations.push(first);
	for _ in 1..copies {
		let generation = Generation::build(&producer, tracks, &identity);
		generation.announce().await;
		generations.push(generation);
	}
	CopyWalk {
		next: Some(Generation::build(&producer, tracks, &identity)),
		_producer: producer,
		_subscribers: subscribers,
		generations,
	}
}

impl CopyWalk {
	/// Swap in the prebuilt newer route at the same identity.
	///
	/// A local track's info is already known, so the front subscribes, walks
	/// every track, and each reader walks its copies in one driver turn.
	async fn swap(&mut self) {
		let generation = self.next.take().expect("one swap per setup");
		generation.announce().await;
		self.generations.push(generation);
	}
}

/// A front with its copies spliced, and the runtime it runs on.
struct Swap {
	rig: CopyWalk,
	runtime: tokio::runtime::Runtime,
}

fn swapping(case: Case<(usize, usize)>) -> Swap {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap();
	let rig = runtime.block_on(copy_rig(case.of.0, case.of.1));
	Swap { rig, runtime }
}

/// A route swap on one front. Setup holds `copies` per track and builds the
/// next generation; the announce and splice are measured.
///
/// The runtime must stay current-thread; see [`Generation::announce`].
#[bench(
	args = cases(COPY_WALK, |(t, c)| format!("{t}t_{c}c")),
	setup = swapping,
	items = |case: Case<(usize, usize)>| case.of.0,
)]
fn copy_walk(mut swap: Swap) -> Swap {
	swap.runtime.block_on(swap.rig.swap());
	swap
}

/// `(readers, retained, routes)` for [`resume_overlap`]: readers of one track,
/// the groups every route holds, and the routes it swapped through.
const RESUME_OVERLAP: [(usize, usize, usize); 17] = [
	(1, 1, 1),
	(1, 64, 1),
	(32, 64, 1),
	(1, 1, 2),
	(1, 64, 2),
	(1, 1024, 2),
	(8, 1, 2),
	(8, 64, 2),
	(8, 1024, 2),
	(32, 64, 2),
	(256, 64, 2),
	(32, 1024, 2),
	(1, 64, 8),
	(1, 64, 32),
	(32, 64, 8),
	(32, 64, 32),
	(256, 64, 8),
];

/// A track that swapped through its routes while its readers' local cap held every
/// group back, so each reader keeps a copy per route and every copy holds the same
/// groups, each replacement in reverse arrival order.
struct ResumeOverlap {
	_producer: origin::Producer,
	_generations: Vec<(origin::Route, track::Producer)>,
	readers: Vec<ResumeReader>,
}

impl ResumeOverlap {
	#[inline(never)]
	fn drain(&mut self, retained: usize) {
		let waiter = kio::Waiter::noop();
		for reader in &mut self.readers {
			let mut delivered = 0;
			while let Poll::Ready(Ok(Some(_))) = reader.poll_group(&waiter) {
				delivered += 1;
			}
			assert_eq!(delivered, retained, "every group once");
		}
	}
}

enum ResumeReader {
	Arrival(track::Subscriber),
	Key(track::Ordered),
}

impl ResumeReader {
	fn poll_group(&mut self, waiter: &kio::Waiter) -> Poll<moq_net::Result<Option<moq_net::group::Consumer>>> {
		match self {
			Self::Arrival(reader) => reader.poll_recv_group(waiter),
			Self::Key(reader) => reader.poll_next_group(waiter),
		}
	}
}

struct ResumeShape {
	readers: usize,
	retained: usize,
	routes: usize,
	waiting: bool,
}

async fn resume_overlap(shape: ResumeShape, ordered: bool) -> ResumeOverlap {
	let ResumeShape {
		readers,
		retained,
		routes,
		waiting,
	} = shape;
	let (producer, driver) = origin::Producer::new(origin::Config::default());
	tokio::spawn(moq_net::time::run(driver));
	let identity = Identity::random();
	let generation = |order: &mut dyn Iterator<Item = usize>| {
		let broadcast = producer
			.route(
				COPY_PATH,
				moq_net::origin::RouteOptions::hidden().with_identity(identity.clone(), 0),
			)
			.unwrap();
		let track = broadcast.broadcast().create_track("0", timed()).unwrap();
		for key in order {
			let mut group = track.create_group(moq_net::group::Info { start: key as u64 }).unwrap();
			group.write_frame(at(key as u64), b"f".as_ref()).unwrap();
			group.finish().unwrap();
		}
		broadcast.announce().unwrap();
		(broadcast, track)
	};
	let mut generations = vec![generation(&mut (0..retained))];
	let resolved = kio::clock::timeout(COPY_WAIT, producer.consume().request(COPY_PATH, None))
		.await
		.expect("front did not resolve")
		.expect("front refused");
	let track = resolved.track("0").unwrap();
	// A budget covering every group, so none is written off as late.
	let subscription = track::Subscription::default().with_ranges([track::Range::from(track::Start::Live {
		min: Some(Duration::from_secs(3600)),
		max: Some(Duration::from_secs(3600)),
	})]);
	let mut subscribers = Vec::with_capacity(readers);
	for _ in 0..readers {
		let subscriber = kio::clock::timeout(COPY_WAIT, track.subscribe(subscription.clone()))
			.await
			.expect("track did not splice")
			.expect("subscribe failed");
		// Held back until the drain, so every route's copy is kept for its unread groups.
		let subscriber = subscriber.with_range(..0);
		subscribers.push(subscriber);
	}
	let waiter = kio::Waiter::noop();
	for route in 1..routes + usize::from(waiting) {
		generations.push(if waiting && route == routes {
			// The serving copy has answered but not produced a group. It has not yet
			// bounded the overlapping copies, which must still skip each other's replay.
			generation(&mut std::iter::empty())
		} else {
			generation(&mut (0..retained).rev())
		});
		let (_, latest) = generations.last().unwrap();
		kio::clock::timeout(COPY_WAIT, support::wanted(latest.demand()))
			.await
			.expect("route swap did not splice")
			.expect("replacement track closed");
		// Each reader follows the swap, subscribing to the new copy.
		for subscriber in &mut subscribers {
			assert!(
				subscriber.poll_recv_group(&waiter).is_pending(),
				"the cap holds every group"
			);
		}
	}
	let readers = subscribers
		.into_iter()
		.map(|subscriber| subscriber.with_range(..))
		.map(|subscriber| match ordered {
			true => ResumeReader::Key(subscriber.ordered()),
			false => ResumeReader::Arrival(subscriber),
		})
		.collect();
	ResumeOverlap {
		_producer: producer,
		_generations: generations,
		readers,
	}
}

/// Readers draining a track whose routes all delivered every group: each group is
/// handed out once, and every other copy's duplicate is skipped against what the reader
/// handed out. The slopes over retained groups and routes are that skipping's cost.
mod resume_overlap {
	use swole::bench;

	use super::*;

	/// A drained track, the runtime its front runs on, and the groups each reader gets.
	struct Drain {
		rig: ResumeOverlap,
		_runtime: tokio::runtime::Runtime,
		retained: usize,
	}

	/// [`RESUME_OVERLAP`], or for a waiting copy only one reader of 64 groups over two
	/// routes or more, named `<readers>r_<retained>g_<routes>c`.
	fn shapes(waiting: bool) -> Vec<Case<(usize, usize, usize)>> {
		let shapes = RESUME_OVERLAP
			.into_iter()
			.filter(|&(readers, retained, routes)| !waiting || (readers == 1 && retained == 64 && routes >= 2));
		cases(shapes, |(r, g, c)| format!("{r}r_{g}g_{c}c"))
	}

	fn drain(case: Case<(usize, usize, usize)>, ordered: bool, waiting: bool) -> Drain {
		let (readers, retained, routes) = case.of;
		let runtime = tokio::runtime::Builder::new_current_thread()
			.enable_all()
			.build()
			.unwrap();
		let shape = ResumeShape {
			readers,
			retained,
			routes,
			waiting,
		};
		let rig = runtime.block_on(resume_overlap(shape, ordered));
		Drain {
			rig,
			_runtime: runtime,
			retained,
		}
	}

	macro_rules! order {
		($name:ident, $setup:ident, $ordered:expr, $waiting:expr) => {
			fn $setup(case: Case<(usize, usize, usize)>) -> Drain {
				drain(case, $ordered, $waiting)
			}

			#[bench(args = shapes($waiting), setup = $setup, items = |case: Case<(usize, usize, usize)>| case.of.0 * case.of.1)]
			fn $name(mut drain: Drain) -> Drain {
				drain.rig.drain(drain.retained);
				drain
			}
		};
	}

	order!(arrival, arrival_rig, false, false);
	order!(key, key_rig, true, false);
	order!(arrival_waiting, arrival_waiting_rig, false, true);
	order!(key_waiting, key_waiting_rig, true, true);
}

fn main() {
	swole::main();
}
