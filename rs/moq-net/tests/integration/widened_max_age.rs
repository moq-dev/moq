#![cfg(feature = "async")]

//! A relay a narrow reader warmed serves a newcomer with a wider max age the groups a cold
//! relay would: the widened SUBSCRIBE_UPDATE has the publisher re-offer the cached groups
//! it skipped as stale, below where the copy started.
//!
//! Lite-06 relies on that re-offer alone: its copy is untimed and its FETCH carries no max
//! age, so a reader asks no fills below the start the publisher resolved from the budget.

use std::collections::BTreeSet;
use std::time::Duration;

use moq_net::track::{Info, Range, Subscription};
use moq_net::{Hop, Timescale, Timestamp, Version, origin, track};

use crate::support::harness::{MockConnectOptions, connect_mock, peer, spawn};

const TIMEOUT: Duration = Duration::from_secs(10);

/// Groups 0 through 9 present one per second, so a budget picks how far back a reader reaches.
const GROUPS: u64 = 10;

/// Reaches groups 6 and up: group 5 ends at 6s, a full 3s behind the 9s edge.
const NARROW: Duration = Duration::from_millis(2500);

/// Reaches groups 2 and up.
const WIDE: Duration = Duration::from_millis(6500);

/// The wires whose publisher resolves the start from the budget. Lite-05 and moq-transport
/// start at the live edge and fill the backlog by range instead, which the budget does not gate.
const VERSIONS: &[&str] = &["moq-lite-06", "moq-lite-07-wip"];

/// The wire for the tests on one version: only lite-07 FETCHes below the start, so only it
/// can pull a group twice.
const VERSION: &str = "moq-lite-07-wip";

fn origin(hop: u64) -> origin::Producer {
	let (origin, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	spawn(driver);
	origin
}

/// A publisher holding every group, a relay, and a viewer behind it.
struct Chain {
	source: track::Producer,
	viewer: origin::Producer,
	_hold: Vec<Box<dyn std::any::Any>>,
}

impl Chain {
	/// Without `relay`, the viewer reads the publisher directly.
	async fn new(version: &str, relay: bool) -> Self {
		let version: Version = version.parse().unwrap();
		let publisher = origin(1);
		let relay = relay.then(|| origin(2));
		let viewer = origin(3);
		let mut links = Vec::new();
		match &relay {
			Some(relay) => {
				links.push(peer(version, &publisher, relay).await);
				links.push(peer(version, relay, &viewer).await);
			}
			None => links.push(peer(version, &publisher, &viewer).await),
		}
		let broadcast = publisher.create_broadcast("live").unwrap();
		let source = broadcast
			.create_track("video", Info::default().with_timescale(Timescale::MILLI))
			.unwrap();
		for sequence in 0..GROUPS {
			let mut group = source.create_group(moq_net::group::Info { sequence }).unwrap();
			group
				.write_frame(Timestamp::from_millis(sequence * 1000).unwrap(), &b"x"[..])
				.unwrap();
			group.finish().unwrap();
		}
		broadcast.announce(Default::default()).unwrap();
		Self {
			source,
			viewer,
			_hold: vec![
				Box::new(publisher),
				Box::new(relay),
				Box::new(links),
				Box::new(broadcast),
			],
		}
	}

	/// Subscribe from the viewer with `budget`, reading from the oldest group still around.
	async fn subscribe(&self, budget: Duration) -> track::Subscriber {
		let consumer = self.viewer.consume();
		consumer.routed("live").await.unwrap();
		let remote = consumer.request_broadcast("live", None).await.unwrap();
		let subscription = Subscription::default()
			.with_max_delay(budget)
			.with_ranges([Range::oldest()]);
		remote.track("video").unwrap().subscribe(subscription).await.unwrap()
	}
}

/// Every group `subscriber` receives until it goes quiet.
async fn received(subscriber: &mut track::Subscriber) -> BTreeSet<u64> {
	let mut got = BTreeSet::new();
	while let Ok(group) = moq_clock::timeout(Duration::from_secs(1), subscriber.recv_group()).await {
		let group = group.unwrap().expect("the track is live");
		assert!(got.insert(group.sequence), "group {} delivered twice", group.sequence);
	}
	got
}

/// What the subscriber reads after widening its own max age, through a relay or not.
#[moq_clock::test]
async fn a_widened_reader_gets_what_was_skipped() {
	for &version in VERSIONS {
		for relay in [false, true] {
			let chain = Chain::new(version, relay).await;
			let mut reader = moq_clock::timeout(TIMEOUT, chain.subscribe(NARROW)).await.unwrap();
			let first = moq_clock::timeout(TIMEOUT, received(&mut reader)).await.unwrap();
			assert_eq!(first, (6..GROUPS).collect(), "{version} relay={relay}");

			let wide = Subscription::default()
				.with_max_delay(WIDE)
				.with_ranges([Range::oldest()]);
			reader.update(wide).unwrap();
			let widened = moq_clock::timeout(TIMEOUT, received(&mut reader)).await.unwrap();
			assert_eq!(
				widened,
				(2..6).collect(),
				"{version} relay={relay}: only what was skipped"
			);
		}
	}
}

#[moq_clock::test]
async fn a_warm_relay_serves_a_wider_newcomer_what_a_cold_one_would() {
	for &version in VERSIONS {
		let cold = Chain::new(version, true).await;
		let mut wide = moq_clock::timeout(TIMEOUT, cold.subscribe(WIDE)).await.unwrap();
		let want = moq_clock::timeout(TIMEOUT, received(&mut wide)).await.unwrap();
		assert_eq!(want, (2..GROUPS).collect(), "{version}: cold");

		let warm = Chain::new(version, true).await;
		let mut narrow = moq_clock::timeout(TIMEOUT, warm.subscribe(NARROW)).await.unwrap();
		let first = moq_clock::timeout(TIMEOUT, received(&mut narrow)).await.unwrap();
		assert_eq!(
			first,
			(6..GROUPS).collect(),
			"{version}: the narrow reader warms the relay"
		);

		// The widened max age lowers the start the publisher resolved, and its re-offer
		// serves what lies between.
		let mut wide = moq_clock::timeout(TIMEOUT, warm.subscribe(WIDE)).await.unwrap();
		let got = moq_clock::timeout(TIMEOUT, received(&mut wide)).await.unwrap();
		assert_eq!(got, want, "{version}: warm");
		// A lite-06 copy has no media time to find the re-offer stale by, and each group in
		// it arrives just now, so the narrow reader takes it too, once per widening.
		let late = moq_clock::timeout(TIMEOUT, received(&mut narrow)).await.unwrap();
		let reoffered = match version {
			"moq-lite-06" => (2..6).collect(),
			_ => BTreeSet::new(),
		};
		assert_eq!(late, reoffered, "{version}: what the narrow reader takes late");
	}
}

/// A reader joining with the max age the relay already asks for changes nothing upstream.
#[moq_clock::test]
async fn the_same_max_age_sends_no_update() {
	let mut chain = Chain::new(VERSION, true).await;
	let mut narrow = moq_clock::timeout(TIMEOUT, chain.subscribe(NARROW)).await.unwrap();
	moq_clock::timeout(TIMEOUT, received(&mut narrow)).await.unwrap();
	while moq_clock::timeout(Duration::from_secs(1), chain.source.subscription_changed())
		.await
		.is_ok()
	{}

	let mut same = moq_clock::timeout(TIMEOUT, chain.subscribe(NARROW)).await.unwrap();
	assert_eq!(
		moq_clock::timeout(TIMEOUT, received(&mut same)).await.unwrap(),
		(6..GROUPS).collect()
	);
	let changed = moq_clock::timeout(Duration::from_secs(1), chain.source.subscription_changed()).await;
	assert!(changed.is_err(), "the publisher saw {changed:?}");
}

/// The groups `registry`'s sessions sent as publisher.
fn sent_groups(registry: &moq_net::stats::Registry) -> u64 {
	registry
		.snapshot()
		.traffic()
		.into_iter()
		.filter(|(_, role, _)| *role == moq_net::stats::Role::Publisher)
		.map(|(_, _, traffic)| traffic.groups)
		.sum()
}

/// Only the newcomer that widens the relay's max age pulls a re-offer upstream; the ones
/// after it are served from what the relay cached, however many there are.
#[moq_clock::test]
async fn wider_newcomers_after_the_first_pull_nothing_upstream() {
	let version: Version = VERSION.parse().unwrap();
	let publisher = origin(1);
	let relay = origin(2);
	// The publisher accepts the relay, so its session meters what goes upstream.
	let registry = moq_net::stats::Registry::new(Default::default());
	let mut options = MockConnectOptions::new(version);
	options.client_publish = Some(relay.consume().with_hidden(true));
	options.client_subscribe = Some(relay.clone());
	options.server_publish = Some(publisher.consume().with_hidden(true));
	options.server_subscribe = Some(publisher.clone());
	options.server_stats = registry
		.tier(moq_net::stats::Tier::new(moq_net::Path::from_static("upstream")))
		.session(moq_net::Path::from_static(""));
	let _upstream = connect_mock(options).await;
	let broadcast = publisher.create_broadcast("live").unwrap();
	let source = broadcast
		.create_track("video", Info::default().with_timescale(Timescale::MILLI))
		.unwrap();
	for sequence in 0..GROUPS {
		let mut group = source.create_group(moq_net::group::Info { sequence }).unwrap();
		group
			.write_frame(Timestamp::from_millis(sequence * 1000).unwrap(), &b"x"[..])
			.unwrap();
		group.finish().unwrap();
	}
	broadcast.announce(Default::default()).unwrap();

	let mut links = Vec::new();
	let mut readers = Vec::new();
	let mut join = async |budget| {
		let viewer = origin(3);
		links.push(peer(version, &relay, &viewer).await);
		let consumer = viewer.consume();
		consumer.routed("live").await.unwrap();
		let remote = consumer.request_broadcast("live", None).await.unwrap();
		let subscription = Subscription::default()
			.with_max_delay(budget)
			.with_ranges([Range::oldest()]);
		let mut reader = remote.track("video").unwrap().subscribe(subscription).await.unwrap();
		let got = received(&mut reader).await;
		readers.push((viewer, reader));
		got
	};
	let narrow = moq_clock::timeout(TIMEOUT, join(NARROW)).await.unwrap();
	assert_eq!(narrow, (6..GROUPS).collect());
	let wide = moq_clock::timeout(TIMEOUT, join(WIDE)).await.unwrap();
	assert_eq!(wide, (2..GROUPS).collect());
	let widened = sent_groups(&registry);
	for newcomer in 0..3 {
		let got = moq_clock::timeout(TIMEOUT, join(WIDE)).await.unwrap();
		assert_eq!(got, wide, "newcomer {newcomer}");
	}
	assert_eq!(sent_groups(&registry), widened, "later newcomers pull nothing upstream");
}
