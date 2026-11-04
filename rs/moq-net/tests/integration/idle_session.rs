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
	// Driven, so its readers' interests reach the session and the viewer asks for what
	// the server publishes.
	let (subscribe, driver) = origin::Producer::new(origin::Config::new(Hop::new(2).unwrap()));
	support::harness::spawn(driver);
	options.client_subscribe = Some(subscribe);
	options.stats = STATS;
	options.server_polls = polls;
	let pair = connect_mock(options).await;

	let mut probe = pair.client.recv_bandwidth().expect("lite carries PROBE");
	assert_eq!(probe.changed().await.unwrap(), Some(Rate::from_bps(1_000_000)));
	kio::clock::sleep(Duration::from_secs(1)).await;
	(pair, probe)
}

fn origin() -> origin::Producer {
	let (origin, driver) = origin::Producer::new(origin::Config::new(Hop::new(1).unwrap()));
	support::harness::spawn(driver);
	origin
}

#[kio::test]
async fn idle_viewer_does_not_wake_the_server() {
	for version in VERSIONS {
		let origin = origin();
		let polls = Arc::new(AtomicUsize::new(0));
		let (pair, _probe) = viewer(version, &origin, Some(polls.clone())).await;

		let reads = &pair.server_transport;
		let (start_polls, start_reads) = (polls.load(Ordering::Relaxed), reads.stats_reads());
		assert!(start_polls > 0, "the requested poll counter must observe the handshake");
		kio::clock::sleep(IDLE).await;
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
#[kio::test]
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

		let moved = kio::clock::timeout(Duration::from_millis(1), probe.changed()).await;
		assert_eq!(
			moved.expect("reported with the work").unwrap(),
			Some(Rate::from_bps(2_000_000)),
			"{version}"
		);
	}
}

/// A transport estimate demanded locally still costs one poll per sampling deadline.
#[kio::test]
async fn ietf_stats_refresh_costs_one_driver_poll() {
	for version in ["moq-transport-22"] {
		let polls = Arc::new(AtomicUsize::new(0));
		let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
		options.stats = STATS;
		options.server_polls = Some(polls.clone());
		let pair = connect_mock(options).await;
		let _bandwidth = pair.server.send_bandwidth().expect("transport estimate");
		kio::clock::sleep(Duration::from_secs(1)).await;
		let start = polls.load(Ordering::Relaxed);
		let reads = pair.server_transport.stats_reads();
		kio::clock::sleep(Duration::from_secs(10)).await;
		let polls = polls.load(Ordering::Relaxed) - start;
		let reads = pair.server_transport.stats_reads() - reads;
		assert_eq!(reads, 100, "{version}: one sample per 100 ms");
		assert_eq!(polls, reads, "{version}: one poll per refresh");
	}
}

/// The session and stats snapshots are not demand; only held consumers open PROBE.
#[kio::test]
async fn probe_follows_consumer_lifetime() {
	for version in VERSIONS {
		let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
		options.stats = STATS;
		let pair = connect_mock(options).await;
		let probe_count = || {
			pair.client_transport
				.bidi_types()
				.iter()
				.filter(|&&kind| kind == 4)
				.count()
		};
		let _ = pair.client.stats();
		kio::clock::sleep(Duration::from_secs(1)).await;
		assert_eq!(probe_count(), 0, "{version}: an idle session must not open PROBE");

		let mut first = pair.client.recv_bandwidth().unwrap();
		assert_eq!(first.changed().await.unwrap(), Some(Rate::from_bps(1_000_000)));
		assert_eq!(probe_count(), 1, "{version}");
		let second = first.clone();
		let resets = pair.client_transport.resets().len();
		drop(first);
		kio::clock::sleep(Duration::from_secs(1)).await;
		assert_eq!(
			pair.client_transport.resets().len(),
			resets,
			"{version}: a clone still needs PROBE"
		);
		drop(second);
		kio::clock::sleep(Duration::from_secs(1)).await;
		assert!(
			pair.client_transport.resets().len() > resets,
			"{version}: last drop resets PROBE"
		);
		let mut resumed = pair.client.recv_bandwidth().unwrap();
		assert_eq!(resumed.peek(), None, "{version}: an unused estimate is cleared");
		assert_eq!(resumed.changed().await.unwrap(), Some(Rate::from_bps(1_000_000)));
		kio::clock::sleep(Duration::from_secs(1)).await;
		assert_eq!(probe_count(), 2, "{version}: demand can resume");
	}
}

/// A stats tick reads the transport's wire counters and adds no sampler timer or session wake.
#[kio::test]
async fn wire_collection_never_arms_the_idle_sampler() {
	for version in ["moq-lite-06", "moq-lite-07-wip", "moq-transport-22"] {
		let registry = moq_net::stats::Registry::new(Default::default());
		let polls = Arc::new(AtomicUsize::new(0));
		let mut options = MockConnectOptions::new(version.parse().unwrap());
		options.server_stats = registry
			.tier(Default::default())
			.session(moq_net::Path::from_static("room"));
		options.server_polls = Some(polls.clone());
		let pair = connect_mock(options).await;
		kio::clock::sleep(Duration::from_secs(1)).await;
		let start_polls = polls.load(Ordering::Relaxed);
		let start_reads = pair.server_transport.stats_reads();
		for tick in 1..=60 {
			pair.server_transport.wire().set_sent_bytes(tick);
			pair.server_transport.wire().set_received_bytes(tick * 2);
			let mut report = moq_net::stats::Report::default();
			registry.report(&mut report);
			assert_eq!(
				(report.sessions[0].presence.wire_tx, report.sessions[0].presence.wire_rx),
				(tick, tick * 2)
			);
			kio::clock::sleep(Duration::from_secs(1)).await;
		}
		assert_eq!(
			polls.load(Ordering::Relaxed),
			start_polls,
			"{version}: a tick woke the session"
		);
		assert_eq!(
			pair.server_transport.stats_reads(),
			start_reads,
			"{version}: a tick armed the sampler"
		);
	}
}
