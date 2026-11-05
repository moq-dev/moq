#![cfg(feature = "async")]

//! Control counters cross real protocol drivers, including legacy multiplexing.

use crate::support::{
	harness::{MockConnectOptions, connect_mock, now, spawn, subscribed},
	mock::create_mock_session_pair,
};
use moq_net::{
	Timescale, Timestamp, origin,
	stats::{Message, Registry, Tier},
};

fn origin() -> origin::Producer {
	let (origin, driver) = origin::Producer::new(Default::default());
	spawn(driver);
	origin
}

#[kio::test]
async fn control_classes_cross_lite_and_ietf_sessions_without_counting_payload() {
	for version in ["moq-lite-03", "moq-lite-06", "moq-lite-07-wip", "moq-transport-22"] {
		let publisher = origin();
		let subscriber = origin();
		let registry = Registry::new(Default::default());
		let tier = Tier::new(moq_net::Path::from_static("internal"));
		let mut options = MockConnectOptions::new(version.parse().unwrap());
		options.server_stats = registry.tier(tier.clone()).session(moq_net::Path::from_static(""));
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
		let remote = subscriber
			.consume()
			.follow("room", None)
			.unwrap()
			.available()
			.await
			.unwrap();
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
		for class in [Message::Announce, Message::Subscribe] {
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

/// The SETUP a server reads before admission counts under the stats the handshake is
/// admitted with, even for a session with neither origin.
#[kio::test]
async fn setup_counts_under_the_admitted_stats() {
	for version in ["moq-lite-07-wip", "moq-transport-22"] {
		let version: moq_net::Version = version.parse().unwrap();
		let registry = Registry::new(Default::default());
		let tier = Tier::new(moq_net::Path::from_static("internal"));
		let mut transport = moq_net::transport::memory::Config::default();
		transport.protocol = Some(version.alpn().to_string());
		let (client_transport, server_transport) = create_mock_session_pair(transport);
		let client = moq_net::Client::new().with_versions(version.into());
		let server = moq_net::Server::new().with_versions(version.into());
		let stats = registry.tier(tier.clone()).session(moq_net::Path::from_static(""));
		let client = async {
			let (session, driver) = client.connect(now(), client_transport).await.unwrap();
			spawn(driver);
			session
		};
		let server = async {
			let request = server.accept_request(now(), server_transport).await.unwrap();
			let (session, driver) = request.with_stats(stats).ok().await.unwrap();
			spawn(driver);
			session
		};
		let _sessions = futures::join!(client, server);
		kio::clock::sleep(std::time::Duration::from_millis(100)).await;
		let rows = registry.snapshot().control();
		assert!(
			rows.iter()
				.any(|(row_tier, class, bytes)| *row_tier == tier
					&& *class == Message::Session
					&& bytes.tx > 0 && bytes.rx > 0),
			"{version}: {rows:?}"
		);
	}
}
