#![cfg(feature = "async")]

//! An idle viewer session costs its server nothing per tick: no driver wakes and
//! no transport stats reads while nothing moves. A server holds thousands of these,
//! so a fixed-interval timer per session adds up to whole cores.

use crate::support;

use std::{
	sync::{
		Arc,
		atomic::{AtomicUsize, Ordering},
	},
	time::Duration,
};

use moq_net::{Hop, Version, bandwidth::Rate, origin};
use support::harness::{MockConnectOptions, MockPair, connect_mock};
use support::mock::MockStats;

const VERSIONS: [&str; 2] = ["moq-lite-06", "moq-lite-07-wip"];

const IDLE: Duration = Duration::from_secs(60);

/// PROBE refreshes an unchanged report this often.
const PROBE_MAX_AGE: Duration = Duration::from_secs(10);

const STATS: MockStats = MockStats {
	estimated_send_rate: Some(1_000_000),
	rtt: Some(Duration::from_millis(40)),
};

/// A viewer session whose server publishes `origin`, with the viewer's PROBE
/// stream open (as a browser viewer keeps one) and its first report received.
async fn viewer(
	version: &str,
	origin: &origin::Producer,
	polls: Option<Arc<AtomicUsize>>,
) -> (MockPair, moq_net::bandwidth::Consumer) {
	let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
	options.server_publish = Some(origin.consume());
	options.client_subscribe = Some(origin::Producer::new(origin::Config::new(Hop::new(2).unwrap())).0);
	options.stats = STATS;
	options.server_polls = polls;
	let pair = connect_mock(options).await;

	let mut probe = pair.client.recv_bandwidth().expect("lite carries PROBE");
	assert_eq!(probe.changed().await.unwrap(), Some(Rate::from_bps(1_000_000)));
	moq_clock::sleep(Duration::from_secs(1)).await;
	(pair, probe)
}

fn origin() -> origin::Producer {
	let (origin, driver) = origin::Producer::new(origin::Config::new(Hop::new(1).unwrap()));
	support::harness::spawn(driver);
	origin
}

#[moq_clock::test]
async fn idle_viewer_does_not_wake_the_server() {
	for version in VERSIONS {
		let origin = origin();
		let polls = Arc::new(AtomicUsize::new(0));
		let (pair, _probe) = viewer(version, &origin, Some(polls.clone())).await;

		let reads = &pair.server_transport;
		let (start_polls, start_reads) = (polls.load(Ordering::Relaxed), reads.stats_reads());
		assert!(start_polls > 0, "the requested poll counter must observe the handshake");
		moq_clock::sleep(IDLE).await;
		let polls = polls.load(Ordering::Relaxed) - start_polls;
		let reads = reads.stats_reads() - start_reads;

		// Nothing moves, so the only work left is the max-age refresh: one sample and
		// one driver poll each. The deadline fires and re-arms inside that poll.
		let refreshes = (IDLE.as_secs() / PROBE_MAX_AGE.as_secs()) as usize;
		assert!(reads <= refreshes, "{version}: {reads} stats reads over {IDLE:?}");
		assert!(polls <= refreshes, "{version}: {polls} driver polls over {IDLE:?}");
	}
}

/// The flip side: a session the server is doing work for samples as it goes, so a
/// moved estimate reaches the viewer with that work, not at the next max age.
#[moq_clock::test]
async fn busy_viewer_sees_the_estimate_move() {
	for version in VERSIONS {
		let origin = origin();
		let (pair, mut probe) = viewer(version, &origin, None).await;

		pair.server_transport.set_stats(MockStats {
			estimated_send_rate: Some(2_000_000),
			..STATS
		});
		// Server work for this session: announcing a broadcast to the viewer.
		let broadcast = origin.create_broadcast("live").unwrap();
		broadcast.announce(Default::default()).unwrap();

		let moved = moq_clock::timeout(Duration::from_millis(1), probe.changed()).await;
		assert_eq!(
			moved.expect("reported with the work").unwrap(),
			Some(Rate::from_bps(2_000_000)),
			"{version}"
		);
	}
}

/// A transport estimate demanded locally still costs one poll per sampling deadline.
#[moq_clock::test]
async fn ietf_stats_refresh_costs_one_driver_poll() {
	for version in ["moq-transport-16", "moq-transport-22"] {
		let polls = Arc::new(AtomicUsize::new(0));
		let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
		options.stats = STATS;
		options.server_polls = Some(polls.clone());
		let pair = connect_mock(options).await;
		let _bandwidth = pair.server.send_bandwidth().expect("transport estimate");
		moq_clock::sleep(Duration::from_secs(1)).await;
		let start = polls.load(Ordering::Relaxed);
		let reads = pair.server_transport.stats_reads();
		moq_clock::sleep(Duration::from_secs(10)).await;
		let polls = polls.load(Ordering::Relaxed) - start;
		let reads = pair.server_transport.stats_reads() - reads;
		assert_eq!(reads, 100, "{version}: one sample per 100 ms");
		assert_eq!(polls, reads, "{version}: one poll per refresh");
	}
}
