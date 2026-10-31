#![cfg(feature = "async")]

//! A session sends its viewers' sum to the peer as the PROBE target, and a sending
//! session probes to the larger of its own reservations and the peer's target.

use crate::support;

use std::time::Duration;

use moq_net::{Version, bandwidth::Rate, broadcast, track};
use support::harness::{MockConnectOptions, MockPair, connect_mock};
use support::mock::{MockSession, MockStats};

/// The versions whose SETUP carries the probe level a target is gated on.
const VERSIONS: [&str; 2] = ["moq-lite-06", "moq-lite-07-wip"];

const STATS: MockStats = MockStats {
	estimated_send_rate: Some(10_000_000),
	rtt: Some(Duration::from_millis(40)),
};

/// A connected pair, each side with a sender when `probe`.
async fn pair(version: &str, probe: bool) -> MockPair {
	let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
	options.stats = STATS;
	options.probe = probe;
	connect_mock(options).await
}

fn mbps(n: u64) -> Rate {
	Rate::from_mbps(n)
}

/// Run until `transport`'s sender is handed `target`.
async fn probes_to(transport: &MockSession, target: Rate) {
	while transport.probe_target() != target {
		moq_clock::sleep(Duration::from_millis(1)).await;
	}
}

/// A track to reserve for, plus the broadcast keeping it alive.
fn reservable() -> (broadcast::Producer, track::Producer) {
	let broadcast = broadcast::Info::default().produce();
	let track = broadcast.create_track("t", None).unwrap();
	(broadcast, track)
}

#[moq_clock::test]
async fn viewers_sum_into_the_peers_target() {
	for version in VERSIONS {
		let pair = pair(version, true).await;
		let viewers = pair.client.viewers();

		let first = viewers.watch(mbps(3));
		probes_to(&pair.server_transport, mbps(3)).await;
		let second = viewers.watch(mbps(2));
		probes_to(&pair.server_transport, mbps(5)).await;
		first.update(mbps(1));
		probes_to(&pair.server_transport, mbps(3)).await;

		drop(second);
		probes_to(&pair.server_transport, mbps(1)).await;
		// The last viewer releases the PROBE stream, and the target lapses with it.
		drop(first);
		probes_to(&pair.server_transport, Rate::ZERO).await;
	}
}

#[moq_clock::test]
async fn the_sender_probes_to_the_larger_target() {
	for version in VERSIONS {
		let pair = pair(version, true).await;
		let (_broadcast, track) = reservable();
		let reserved = pair.server.allocator().reserve(&track.demand(), mbps(4));
		probes_to(&pair.server_transport, mbps(4)).await;

		let viewer = pair.client.viewers().watch(mbps(6));
		probes_to(&pair.server_transport, mbps(6)).await;
		viewer.update(mbps(2));
		probes_to(&pair.server_transport, mbps(4)).await;
		drop(reserved);
		probes_to(&pair.server_transport, mbps(2)).await;
	}
}

/// A session that publishes and watches probes its uplink for what it sends, never
/// for what it watches, and its peer the other way around.
#[moq_clock::test]
async fn the_directions_stay_apart() {
	for version in VERSIONS {
		let pair = pair(version, true).await;
		let (_broadcast, track) = reservable();
		let _sent = pair.client.allocator().reserve(&track.demand(), mbps(1));
		let _watched = pair.client.viewers().watch(mbps(6));

		probes_to(&pair.client_transport, mbps(1)).await;
		probes_to(&pair.server_transport, mbps(6)).await;
		moq_clock::sleep(Duration::from_secs(1)).await;
		assert_eq!(pair.client_transport.probe_target(), mbps(1), "{version}");
		assert_eq!(pair.server_transport.probe_target(), mbps(6), "{version}");
	}
}

#[moq_clock::test]
async fn opting_out_sends_no_target() {
	for version in VERSIONS {
		let pair = pair(version, true).await;
		let viewers = pair.client.viewers();
		viewers.set_target(Some(Rate::ZERO));
		let viewer = viewers.watch(mbps(6));
		let mut grant = viewer.consume();
		// The estimate arrives, so the PROBE stream is up and the target would have too.
		assert_eq!(grant.changed().await.unwrap(), Some(mbps(6)), "{version}");
		moq_clock::sleep(Duration::from_secs(1)).await;
		assert_eq!(pair.server_transport.probe_target(), Rate::ZERO, "{version}");
		assert!(!grant.probed(), "{version}: opted out, nothing probes for it");

		viewers.set_target(None);
		probes_to(&pair.server_transport, mbps(6)).await;
		assert!(grant.probed(), "{version}");
	}
}

/// A peer without a sender advertises `Report`, so its viewers are never probed for.
#[moq_clock::test]
async fn a_peer_without_a_sender_is_sent_no_target() {
	for version in VERSIONS {
		let pair = pair(version, false).await;
		let viewer = pair.client.viewers().watch(mbps(6));
		let mut grant = viewer.consume();
		assert_eq!(grant.changed().await.unwrap(), Some(mbps(6)), "{version}");
		assert!(!grant.probed(), "{version}");
	}
}
