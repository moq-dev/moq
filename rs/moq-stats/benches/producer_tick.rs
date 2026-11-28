//! What one relay pays per stats tick to drain its registry and encode its
//! bucket track, swept over held paths, tiers, and the share of those paths
//! whose counters changed since the last tick (`stats_tick`).
//!
//! Idle paths stay in the bucket's snapshot for as long as the registry holds
//! them, so the snapshot grows with the table; setup checks that it still fits
//! the frame cap and that a changed tick writes a frame.
//!
//! `stats_presence` sweeps the same tick over connected labels and the
//! presence tracks held for them.
//!
//! `stats_prefix` sweeps it over held paths, path depth, and held prefix
//! tracks: each row walks its ancestors once a prefix is held, so the cost
//! grows with rows times depth, plus each held track's encode.
//!
//! `stats_buckets` sweeps a depth-1 producer over held paths and the buckets
//! they spread across, each bucket one track: the rows cost the same however
//! they split, so a slope in buckets is per-track overhead.
//!
//! `stats_roots` sweeps a producer publishing each root's view over held paths
//! and the roots they spread across, one session per root: each row is copied
//! into the one view above it, so a slope in roots is per-broadcast overhead.
//!
//! Run with `cargo bench -p moq-stats --features bench --bench producer_tick`.

use std::fmt;

use futures::FutureExt;
use moq_net::{origin, stats};
use moq_stats::produce::bench::{Driver, FrameBytes};
use swole::bench;

/// Held paths. The last count is sized so its one-tier snapshot nears the cache cap. Every tier shares the one bucket's snapshot, so more tiers stop below it.
const PATHS: [usize; 4] = [100, 1_000, 10_000, 50_000];

const TIERS: [usize; 2] = [1, 4];

/// Share of paths whose payload counter moves each tick. Zero stays idle.
const CHANGED: [usize; 3] = [0, 10, 100];

struct Relay {
	registry: stats::Registry,
	tiers: Vec<stats::Tier>,
	paths: Vec<String>,
	/// Bump every nth path. Zero bumps nothing.
	step: usize,
	driver: Driver,
	_origin: origin::Producer,
	_origin_driver: origin::Driver,
	_publishers: Vec<origin::Route>,
	_cursors: Vec<origin::Routes>,
	_sessions: Vec<stats::Session>,
	_held: Vec<moq_net::track::Subscriber>,
}

impl Relay {
	/// `depth` is each path's segment count, at least 2. With `buckets`, the
	/// paths spread over that many first segments and the driver publishes each
	/// as its own bucket track; without, every path shares one.
	fn build(paths: usize, tiers: usize, changed_percent: usize, depth: usize, buckets: Option<usize>) -> Self {
		let registry = stats::Registry::new(
			stats::Config::new().with_exclude(moq_net::Pattern::subtree(".stats").expect("literal prefix")),
		);
		let (origin, origin_driver) = origin::Producer::new(origin::Config::default());
		let nested = "/s".repeat(depth - 2);
		let path_names: Vec<String> = (0..paths)
			.map(|index| match buckets {
				Some(buckets) => format!("g{:04}/{index:06}{nested}", index % buckets),
				None => format!("room/{index:06}{nested}"),
			})
			.collect();
		let publishers: Vec<_> = path_names
			.iter()
			.map(|path| {
				origin
					.route(path.as_str(), moq_net::origin::RouteOptions::default())
					.expect("publish path")
			})
			.collect();
		let mut cursors = Vec::with_capacity(tiers);
		let mut sessions = Vec::with_capacity(tiers);
		let tier_labels: Vec<_> = (0..tiers)
			.map(|tier| {
				let tier = stats::Tier::new(moq_net::Path::new(&format!("t{tier}")).unwrap());
				let session = registry
					.tier(tier.clone())
					.session(moq_net::stats::SessionInfo::new(moq_net::Path::from_static("root")));
				let mut cursor = origin.consume().with_stats(session.clone()).routes();
				while cursor.next().now_or_never().flatten().is_some() {}
				cursors.push(cursor);
				sessions.push(session);
				tier
			})
			.collect();

		let mut report = stats::Report::default();
		registry.report(&mut report, moq_net::time::Instant::now());
		assert_eq!(report.traffic.len(), paths * tiers, "one held entry per path and tier");

		let depth = if buckets.is_some() { 1 } else { 0 };
		let driver = Driver::new(registry.clone(), origin.clone(), depth).expect("stats broadcast");
		Self {
			registry,
			tiers: tier_labels,
			paths: path_names,
			step: 100usize.checked_div(changed_percent).unwrap_or(0),
			driver,
			_origin: origin,
			_origin_driver: origin_driver,
			_publishers: publishers,
			_cursors: cursors,
			_sessions: sessions,
			_held: Vec::new(),
		}
	}

	/// Scope one session to each of the first `roots` buckets and publish each
	/// root's view.
	fn root(&mut self, roots: usize) {
		for index in 0..roots {
			let root = moq_net::Path::new(&format!("g{index:04}")).unwrap();
			let session = self
				.registry
				.tier(self.tiers[0].clone())
				.session(stats::SessionInfo::new(root.clone()).with_root(root));
			self._sessions.push(session);
		}
		self.driver.publish_roots();
	}

	/// Hold the first `held` paths' `room/<index>` prefix tracks.
	fn hold(&mut self, held: usize) {
		for index in 0..held {
			let sub = self.driver.hold_prefix(&format!("room/{index:06}"));
			self._held.push(sub);
		}
	}

	/// Move the changed share's publisher byte counter, outside the timed tick.
	fn touch(&self) {
		if self.step == 0 {
			return;
		}
		for path in self.paths.iter().step_by(self.step) {
			for tier in &self.tiers {
				moq_net::fuzz::bump_publisher_bytes(
					&self.registry,
					moq_net::Path::new(path.as_str()).unwrap(),
					tier,
					1,
				);
			}
		}
	}
}

/// Warm up once, paying for track creation, then check one changed tick's frame.
fn check(relay: &mut Relay, paths: usize, changed: usize) {
	relay.driver.tick();
	relay.touch();
	let bytes = relay.driver.tick();
	assert!(bytes.held > 0, "bucket snapshot missing");
	assert!(
		bytes.held as u64 <= CAP,
		"bucket frame exceeds the cache cap: {} > {CAP}",
		bytes.held
	);
	// The snapshot keeps every held path, so a few paths cannot be a couple of braces.
	assert!(bytes.held > paths, "bucket snapshot did not grow with held paths");
	if changed > 0 {
		assert!(bytes.wrote > 0, "changed paths did not publish a frame");
	}
}

/// The most moq-net's group cache holds, which a snapshot frame must fit.
const CAP: u64 = 32 * 1024 * 1024;

/// One relay's tick, named `<paths>p_<tiers>t_<changed>pct`.
#[derive(Clone, Copy)]
struct Tick {
	paths: usize,
	tiers: usize,
	changed: usize,
}

impl fmt::Display for Tick {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}p_{}t_{}pct", self.paths, self.tiers, self.changed)
	}
}

fn ticks() -> Vec<Tick> {
	let mut cases = Vec::new();
	for changed in CHANGED {
		for tiers in TIERS {
			for paths in PATHS {
				if tiers > 1 && paths == PATHS[PATHS.len() - 1] {
					continue;
				}
				cases.push(Tick { paths, tiers, changed });
			}
		}
	}
	cases
}

fn tick_relay(case: Tick) -> Relay {
	let mut relay = Relay::build(case.paths, case.tiers, case.changed, 2, None);
	check(&mut relay, case.paths, case.changed);
	relay
}

#[bench(args = ticks(), setup = tick_relay, before = Relay::touch, items = |case: Tick| case.paths * case.tiers)]
fn stats_tick(relay: &mut Relay) -> FrameBytes {
	relay.driver.tick()
}

/// Connected labels, one session each.
const LABELS: [usize; 2] = [100, 10_000];

/// Labels whose presence track a reader holds.
const HELD: [usize; 3] = [0, 100, 1_000];

/// A relay with sessions under many labels, some of them held.
struct Presence {
	driver: Driver,
	_registry: stats::Registry,
	_origin: origin::Producer,
	_origin_driver: origin::Driver,
	_sessions: Vec<stats::Session>,
	_wire: Vec<stats::Wire>,
	_held: Vec<moq_net::track::Subscriber>,
}

impl Presence {
	fn build(labels: usize, held: usize) -> Self {
		let registry = stats::Registry::new(stats::Config::new());
		let (origin, origin_driver) = origin::Producer::new(origin::Config::default());
		let names: Vec<String> = (0..labels).map(|index| format!("room/{index:05}")).collect();
		let sessions: Vec<_> = names
			.iter()
			.map(|label| {
				registry
					.tier(stats::Tier::default())
					.session(moq_net::stats::SessionInfo::new(
						moq_net::Path::new(label.as_str()).unwrap(),
					))
			})
			.collect();
		// One set of wire counters per connection, independently of held presence tracks.
		let wire = sessions
			.iter()
			.map(|session| session.wire(moq_net::transport::Wire::new()))
			.collect();
		let mut driver = Driver::new(registry.clone(), origin.clone(), 0).expect("stats broadcast");
		driver.tick();
		let held = names
			.iter()
			.take(held)
			.map(|label| driver.hold_presence(label))
			.collect();
		Self {
			driver,
			_registry: registry,
			_origin: origin,
			_origin_driver: origin_driver,
			_sessions: sessions,
			_wire: wire,
			_held: held,
		}
	}
}

/// Connected and held labels, named `<labels>l_<held>h`.
#[derive(Clone, Copy)]
struct PresenceCase {
	labels: usize,
	held: usize,
}

impl fmt::Display for PresenceCase {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}l_{}h", self.labels, self.held)
	}
}

fn presence_cases() -> Vec<PresenceCase> {
	LABELS
		.into_iter()
		.flat_map(|labels| {
			HELD.into_iter()
				.filter(move |&held| held <= labels)
				.map(move |held| PresenceCase { labels, held })
		})
		.collect()
}

fn presence(case: PresenceCase) -> Presence {
	let mut relay = Presence::build(case.labels, case.held);
	relay.driver.tick();
	relay
}

/// What holding presence tracks costs a tick, swept over connected labels and
/// held labels: each session row looks its label up once, and each held track
/// is summed and encoded once.
#[bench(args = presence_cases(), setup = presence, items = |case: PresenceCase| case.labels)]
fn stats_presence(relay: &mut Presence) -> FrameBytes {
	relay.driver.tick()
}

const PREFIX_PATHS: [usize; 2] = [1_000, 10_000];

/// Segments per path.
const PREFIX_DEPTH: [usize; 2] = [2, 8];

/// Prefix tracks a reader holds.
const PREFIX_HELD: [usize; 3] = [0, 10, 100];

/// Held paths, their depth, and held prefix tracks, named `<paths>p_<depth>d_<held>h`.
#[derive(Clone, Copy)]
struct PrefixCase {
	paths: usize,
	depth: usize,
	held: usize,
}

impl fmt::Display for PrefixCase {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}p_{}d_{}h", self.paths, self.depth, self.held)
	}
}

fn prefix_cases() -> Vec<PrefixCase> {
	let mut cases = Vec::new();
	for paths in PREFIX_PATHS {
		for depth in PREFIX_DEPTH {
			for held in PREFIX_HELD {
				cases.push(PrefixCase { paths, depth, held });
			}
		}
	}
	cases
}

fn prefix(case: PrefixCase) -> Relay {
	let mut relay = Relay::build(case.paths, 1, 10, case.depth, None);
	relay.driver.tick();
	relay.hold(case.held);
	// Warm up once more, paying for the held tracks' creation.
	relay.driver.tick();
	relay
}

/// What holding prefix tracks costs a tick, with a tenth of the paths moving.
#[bench(args = prefix_cases(), setup = prefix, before = Relay::touch, items = |case: PrefixCase| case.paths)]
fn stats_prefix(relay: &mut Relay) -> FrameBytes {
	relay.driver.tick()
}

const BUCKET_PATHS: [usize; 2] = [1_000, 10_000];

/// Buckets the paths spread across, each a track.
const BUCKETS: [usize; 3] = [1, 100, 1_000];

/// Held paths and the buckets or roots they spread across, named `<paths>p_<n><unit>`.
#[derive(Clone, Copy)]
struct Spread {
	paths: usize,
	across: usize,
	unit: char,
}

impl fmt::Display for Spread {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}p_{}{}", self.paths, self.across, self.unit)
	}
}

fn spread(across: [usize; 3], unit: char) -> Vec<Spread> {
	BUCKET_PATHS
		.into_iter()
		.flat_map(|paths| {
			across
				.into_iter()
				.filter(move |&across| across <= paths)
				.map(move |across| Spread { paths, across, unit })
		})
		.collect()
}

fn buckets(case: Spread) -> Relay {
	let mut relay = Relay::build(case.paths, 1, 10, 2, Some(case.across));
	// Warm up once, paying for track creation.
	relay.driver.tick();
	relay
}

/// What spreading paths over bucket tracks costs a tick, with a tenth of the
/// paths moving.
#[bench(args = spread(BUCKETS, 'b'), setup = buckets, before = Relay::touch, items = |case: Spread| case.paths)]
fn stats_buckets(relay: &mut Relay) -> FrameBytes {
	relay.driver.tick()
}

/// Roots the paths spread across, one session scoped to each.
const ROOTS: [usize; 3] = [1, 100, 1_000];

fn roots(case: Spread) -> Relay {
	let mut relay = Relay::build(case.paths, 1, 10, 2, Some(case.across));
	relay.root(case.across);
	// Warm up once, paying for track creation.
	relay.driver.tick();
	relay
}

/// What publishing each root's view costs a tick, with a tenth of the paths moving.
#[bench(args = spread(ROOTS, 'r'), setup = roots, before = Relay::touch, items = |case: Spread| case.paths)]
fn stats_roots(relay: &mut Relay) -> FrameBytes {
	relay.driver.tick()
}

fn main() {
	swole::main();
}
