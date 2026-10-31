#![cfg(feature = "async")]

//! `broadcast::Producer::close()` ends a broadcast the same way for a local consumer
//! and a remote one: the handle closes, every new track lookup answers
//! [`Error::Unroutable`](moq_net::Error::Unroutable), a fresh request for the path
//! answers the same, and the broadcast can never be announced again.

use crate::support;

use std::time::Duration;

use moq_net::{Error, Hop, Version};
use support::harness::{MockConnectOptions, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(10);

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

/// Close a published broadcast and check what `reader`'s origin answers afterwards.
async fn close_then_lookup(publisher: moq_net::origin::Producer, reader: moq_net::origin::Producer) {
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let _track = broadcast.create_track("video", None).unwrap();
	broadcast.announce(Default::default()).unwrap();

	let consumer = reader.consume();
	let handle = moq_clock::timeout(TIMEOUT, consumer.follow("bcast", None).unwrap().available())
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves");
	handle.track("video").expect("a live broadcast serves its track");

	broadcast.close();

	moq_clock::timeout(TIMEOUT, handle.closed())
		.await
		.expect("the handle never closed");
	assert!(matches!(handle.track("video"), Err(Error::Unroutable)));
	assert!(matches!(handle.track("audio"), Err(Error::Unroutable)));
	assert!(matches!(consumer.request("bcast", None).await, Err(Error::Unroutable)));
	assert!(matches!(broadcast.announce(Default::default()), Err(Error::Closed)));
}

#[moq_clock::test]
async fn close_ends_a_local_consumer() {
	let origin = produce_origin(1);
	close_then_lookup(origin.clone(), origin).await;
}

#[moq_clock::test]
async fn close_ends_a_remote_consumer() {
	for version in ["moq-lite-05", "moq-transport-18"] {
		let publisher = produce_origin(1);
		let subscriber = produce_origin(2);
		let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
		options.server_publish = Some(publisher.consume());
		options.client_subscribe = Some(subscriber.clone());
		let _pair = connect_mock(options).await;

		close_then_lookup(publisher, subscriber).await;
	}
}
