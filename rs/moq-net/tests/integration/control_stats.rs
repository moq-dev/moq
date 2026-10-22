#![cfg(feature = "async")]

//! Control-body counters cross real protocol drivers, including legacy multiplexing.

use crate::support::harness::{MockConnectOptions, connect_mock, spawn, subscribed};
use moq_net::{
	Timescale, Timestamp, origin,
	stats::{Control, Registry, Tier},
};

fn origin() -> origin::Producer {
	let (origin, driver) = origin::Producer::new(Default::default());
	spawn(driver);
	origin
}

#[moq_clock::test]
async fn control_classes_cross_lite_and_ietf_sessions_without_counting_payload() {
	for version in [
		"moq-lite-03",
		"moq-lite-06",
		"moq-lite-07-wip",
		"moq-transport-14",
		"moq-transport-15",
		"moq-transport-16",
		"moq-transport-22",
	] {
		let publisher = origin();
		let subscriber = origin();
		let registry = Registry::new(Default::default());
		let tier = Tier::new("internal");
		let mut options = MockConnectOptions::new(version.parse().unwrap());
		options.server_stats = registry.tier(tier.clone()).session("");
		options.client_publish = Some(publisher.consume());
		options.server_subscribe = Some(subscriber.clone());
		let _pair = connect_mock(options).await;
		let broadcast = publisher.create_broadcast("room").unwrap();
		let track = broadcast
			.create_track(
				"video",
				moq_net::track::Info::default().with_timescale(Timescale::MILLI),
			)
			.unwrap();
		broadcast.announce(Default::default()).unwrap();
		subscriber.consume().routed("room").await.unwrap();
		let remote = subscriber.consume().request_broadcast("room", None).await.unwrap();
		let mut sub = remote.track("video").unwrap().subscribe(None).await.unwrap();
		subscribed(&track).await;
		let mut group = track.append_group().unwrap();
		group
			.write_frame(Timestamp::ZERO, bytes::Bytes::from(vec![7; 64 * 1024]))
			.unwrap();
		group.finish().unwrap();
		let mut group = sub.recv_group().await.unwrap().unwrap();
		assert_eq!(group.read_frame().await.unwrap().unwrap().payload.len(), 64 * 1024);
		let rows = registry.snapshot().control();
		for class in [Control::Announce, Control::Subscribe] {
			assert!(
				rows.iter()
					.any(|(row_tier, row_class, bytes)| *row_tier == tier && *row_class == class && bytes.tx > 0),
				"{version}: {class:?}: {rows:?}"
			);
		}
		let rx: u64 = rows.iter().map(|(_, _, bytes)| bytes.rx).sum();
		assert!(
			rx > 0 && rx < 64 * 1024,
			"{version}: media payload entered control counters: {rows:?}"
		);
	}
}

/// A session with neither origin still counts its control traffic under its context.
#[moq_clock::test]
async fn control_counts_without_origins() {
	for version in ["moq-lite-07-wip", "moq-transport-22"] {
		let registry = Registry::new(Default::default());
		let tier = Tier::new("internal");
		let mut options = MockConnectOptions::new(version.parse().unwrap());
		options.server_stats = registry.tier(tier.clone()).session("");
		let _pair = connect_mock(options).await;
		moq_clock::sleep(std::time::Duration::from_millis(100)).await;
		let rows = registry.snapshot().control();
		assert!(
			rows.iter()
				.any(|(row_tier, class, bytes)| *row_tier == tier && *class == Control::Session && bytes.tx > 0),
			"{version}: {rows:?}"
		);
	}
}
