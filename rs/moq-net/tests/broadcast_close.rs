//! `broadcast::Producer::close()` ends a broadcast the same way for a local consumer
//! and a remote one: the handle closes, every new track lookup answers
//! [`Error::Unroutable`](moq_net::Error::Unroutable), a fresh request for the path
//! answers the same, and the broadcast can never be announced again.

mod support;

use std::time::Duration;

use moq_net::{Error, Hop, Version};
use support::harness::{MockConnectOptions, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(10);

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	tokio::spawn(support::harness::run(driver));
	producer
}

/// Close a published broadcast and check what `reader`'s origin answers afterwards.
async fn close_then_lookup(publisher: moq_net::origin::Producer, reader: moq_net::origin::Producer) {
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let _track = broadcast.create_track("video", None).unwrap();
	broadcast.announce(Default::default()).unwrap();

	let consumer = reader.consume();
	tokio::time::timeout(TIMEOUT, consumer.routed("bcast"))
		.await
		.expect("announce timeout")
		.expect("routed");
	let handle = tokio::time::timeout(TIMEOUT, consumer.request_broadcast("bcast"))
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves");
	handle.track("video").expect("a live broadcast serves its track");

	broadcast.close();

	tokio::time::timeout(TIMEOUT, handle.closed())
		.await
		.expect("the handle never closed");
	assert!(matches!(handle.track("video"), Err(Error::Unroutable)));
	assert!(matches!(handle.track("audio"), Err(Error::Unroutable)));
	assert!(matches!(
		consumer.request_broadcast("bcast").await,
		Err(Error::Unroutable)
	));
	assert!(matches!(broadcast.announce(Default::default()), Err(Error::Closed)));
}

#[tokio::test]
async fn close_ends_a_local_consumer() {
	tokio::time::pause();
	let origin = produce_origin(1);
	close_then_lookup(origin.clone(), origin).await;
}

#[tokio::test]
async fn close_ends_a_remote_consumer() {
	tokio::time::pause();
	for version in ["moq-lite-05", "moq-transport-14", "moq-transport-19"] {
		let publisher = produce_origin(1);
		let subscriber = produce_origin(2);
		let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
		options.server_publish = Some(publisher.consume());
		options.client_subscribe = Some(subscriber.clone());
		let _pair = connect_mock(options).await;

		close_then_lookup(publisher, subscriber).await;
	}
}
