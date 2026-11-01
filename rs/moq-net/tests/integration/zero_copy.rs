#![cfg(feature = "async")]

//! A frame crosses an in-memory session as the same allocation it was written as: the
//! publisher hands the payload to the transport, and the subscriber reads it back,
//! without a copy on either side.

use crate::support;

use std::time::Duration;

use bytes::Bytes;
use moq_net::{Client, Hop, Server, Timestamp, Version, origin, transport::memory};
use support::harness::{now, spawn, subscribed};

const TIMEOUT: Duration = Duration::from_secs(10);

const VERSIONS: &[&str] = &["moq-lite-03", "moq-lite-06", "moq-lite-07-wip", "moq-transport-22"];

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	spawn(driver);
	producer
}

#[kio::test]
async fn frames_cross_without_a_copy() {
	for version in VERSIONS {
		let version: Version = version.parse().unwrap();
		let publisher = produce_origin(1);
		let subscriber = produce_origin(2);

		let mut config = memory::Config::default();
		config.protocol = Some(version.alpn().to_string());
		let (client, server) = memory::pair(config);
		let client_config = Client::new()
			.with_versions(version.into())
			.with_publisher(publisher.consume());
		let server_config = Server::new()
			.with_versions(version.into())
			.with_subscriber(subscriber.clone());
		// Each driver runs as soon as its handshake resolves: a newer draft's accept waits
		// on the client's SETUP, which only its driver sends.
		let (_client, _server) = futures::join!(
			async {
				let (session, driver) = client_config.connect(now(), client).await.expect("client handshake");
				spawn(driver);
				session
			},
			async {
				let (session, driver) = server_config.accept(now(), server).await.expect("server handshake");
				spawn(driver);
				session
			}
		);

		let broadcast = publisher.create_broadcast("room").unwrap();
		let info = moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI);
		let track = broadcast.create_track("video", info).unwrap();
		broadcast.announce(Default::default()).unwrap();

		let consumer = subscriber.consume();
		let remote = kio::clock::timeout(TIMEOUT, consumer.follow("room", None).unwrap().available())
			.await
			.expect("resolve timeout")
			.expect("broadcast resolves");
		let mut sub = remote.track("video").unwrap().subscribe(None).await.expect("subscribe");
		subscribed(&track).await;

		let payload = Bytes::from(vec![7u8; 64 * 1024]);
		let mut group = track.append_group().unwrap();
		group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
		group.finish().unwrap();

		let mut group = kio::clock::timeout(TIMEOUT, sub.recv_group())
			.await
			.expect("group timeout")
			.unwrap()
			.expect("a group");
		let frame = group.read_frame().await.unwrap().expect("a frame");
		assert_eq!(frame.payload, payload, "{version}");
		assert_eq!(
			frame.payload.as_ptr(),
			payload.as_ptr(),
			"{version}: the payload was copied"
		);
	}
}
