#![cfg(feature = "async")]

//! A relay that backfilled a reader's backlog serves the same backlog to the next reader
//! from its cache.
//!
//! Where the upstream starts the copy at its live edge, as moq-transport and moq-lite 05
//! do, the backlog a reader's floor names below that start arrives through fills, outside
//! the live feed's arrival walk. A newcomer finds those groups cached, so it neither walks
//! nor asks for them, and read only the live edge until it took the cached backfill as
//! filled.

use std::collections::BTreeSet;
use std::time::Duration;

use moq_net::track::{Info, Subscription};
use moq_net::{Hop, Timescale, Timestamp, Version, origin, track};

use crate::support::harness::{peer, spawn};

const TIMEOUT: Duration = Duration::from_secs(10);

const GROUPS: u64 = 10;

/// Reaches every group, however long the readers take.
const BUDGET: Duration = Duration::from_secs(100);

/// Moq-transport-22 is left out: its cold relay serves an absolute floor only from the
/// live edge.
const VERSIONS: &[&str] = &["moq-lite-05", "moq-lite-06", "moq-lite-07-wip", "moq-transport-18"];

fn origin(hop: u64) -> origin::Producer {
	let (origin, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	spawn(driver);
	origin
}

async fn subscribe(relay: &origin::Producer) -> track::Subscriber {
	let consumer = relay.consume();
	let remote = consumer.follow("live", None).unwrap().available().await.unwrap();
	remote
		.track("video")
		.unwrap()
		.subscribe(Subscription::default().with_max_delay(BUDGET).with_range(0..))
		.await
		.unwrap()
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

#[moq_clock::test]
async fn a_newcomer_on_a_warm_relay_gets_the_backfill() {
	for &version in VERSIONS {
		let publisher = origin(1);
		let relay = origin(2);
		let _link = peer(version.parse::<Version>().unwrap(), &publisher, &relay).await;
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

		let mut first = moq_clock::timeout(TIMEOUT, subscribe(&relay)).await.unwrap();
		let cold = moq_clock::timeout(TIMEOUT, received(&mut first)).await.unwrap();
		assert_eq!(cold, (0..GROUPS).collect(), "{version}: cold");

		// The same subscription changes nothing upstream, so only the cache can serve it.
		let mut second = moq_clock::timeout(TIMEOUT, subscribe(&relay)).await.unwrap();
		let warm = moq_clock::timeout(TIMEOUT, received(&mut second)).await.unwrap();
		assert_eq!(warm, cold, "{version}: warm");
	}
}
