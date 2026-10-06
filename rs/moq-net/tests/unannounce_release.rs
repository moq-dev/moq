//! A relay reading a broadcast that its publisher unannounced still cancels its upstream
//! subscription once its last reader leaves, so the publisher's demand goes unused.

mod support;

use std::time::Duration;

use moq_net::{Hop, Version};
use support::harness::{MockConnectOptions, connect_mock};

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	tokio::spawn(support::harness::run(driver));
	producer
}

async fn release(version: Version) {
	let publisher = produce_origin(1);
	let relay = produce_origin(2);
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(relay.clone());
	let _pair = connect_mock(options).await;

	let broadcast = publisher.publish("a/b", moq_net::origin::Route::default()).unwrap();
	let track = broadcast.create_track("t", None).unwrap();

	let consumer = relay.consume();
	consumer.routed("a/b").await.expect("routed");
	let remote = consumer.request_broadcast("a/b").await.expect("resolves");
	let sub = remote.track("t").unwrap().subscribe(None).await.expect("subscribes");
	track.demand().used().await.unwrap();

	broadcast.unannounce();
	tokio::time::sleep(Duration::from_secs(1)).await;

	drop(sub);
	drop(remote);
	tokio::time::timeout(Duration::from_secs(600), track.demand().unused())
		.await
		.unwrap_or_else(|_| panic!("{version}: the relay kept its upstream subscription"))
		.unwrap();
}

#[tokio::test(start_paused = true)]
async fn unannounced_remote_track_releases_demand() {
	for name in Version::names() {
		release(name.parse().unwrap()).await;
	}
}
