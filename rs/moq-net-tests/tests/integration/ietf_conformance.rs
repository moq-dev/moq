//! Moxygen announces one prefix, then serves its sixteen-field test namespaces.
//!
//! It answers each namespace without announcing its exact path, as only a session's peer
//! claim does, so the claim lands through the hidden `fuzz` module.

use crate::support::harness::{MockConnectOptions, connect_mock, spawn};
use moq_net::{Hop, Version, broadcast, origin};
use std::sync::Arc;

// Moxygen c27ae6358df62a1195da479b2e07c50dd8174279, moqtest/Utils.cpp.
const PREFIX: &str = "moq-test-00";
const NAMESPACE: &str = "moq-test-00/0/0/0/1/2/2/1024/100/50/1/1/1/-1/-1/0";

fn origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	spawn(driver);
	producer
}

async fn routing(version: &str, cluster: bool) {
	let version: Version = version.parse().unwrap();
	let publisher = origin(1);
	let relay = origin(2);
	let reader = origin(4);
	let prefix = Arc::new(
		moq_net::fuzz::requests_route(&publisher, moq_net::Path::new(PREFIX).unwrap(), Default::default()).unwrap(),
	);
	let broadcast = broadcast::Info::new().produce();
	let track = broadcast.create_track("test", moq_net::track::Info::default()).unwrap();
	let serving = prefix.clone();
	let answer = broadcast.consume();
	moq_clock::sim::spawn(async move {
		let request = serving.requested_broadcast().await.unwrap();
		assert_eq!(request.path().as_str(), NAMESPACE);
		moq_net::fuzz::accept(request, answer);
	});

	let mut options = MockConnectOptions::new(version);
	options.client_publish = Some(publisher.consume());
	options.server_subscribe = Some(relay.clone());
	let mut links = vec![connect_mock(options).await];
	let downstream = if cluster {
		let second = origin(3);
		let mut options = MockConnectOptions::new("moq-lite-07-wip".parse().unwrap());
		options.server_publish = Some(relay.consume());
		options.client_subscribe = Some(second.clone());
		options.client_extensions.ietf = true;
		options.server_extensions.ietf = true;
		links.push(connect_mock(options).await);
		second
	} else {
		relay.clone()
	};
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(downstream.consume());
	options.client_subscribe = Some(reader.clone());
	links.push(connect_mock(options).await);

	let remote = reader
		.consume()
		.follow(NAMESPACE, None)
		.unwrap()
		.available()
		.await
		.unwrap();
	let mut subscription = remote.track("test").unwrap().subscribe(None).await.unwrap();
	crate::support::harness::subscribed(&track).await;
	let mut group = track.append_group().unwrap();
	group.write_frame(None, &b"conformance"[..]).unwrap();
	group.finish().unwrap();
	track.finish().unwrap();
	let mut received = subscription.recv_group().await.unwrap().unwrap();
	assert_eq!(received.sequence, 0);
	assert_eq!(
		received.read_frame().await.unwrap().unwrap().payload.as_ref(),
		b"conformance"
	);
	assert!(received.read_frame().await.unwrap().is_none());
	drop((links, prefix, broadcast, track));
}

#[moq_clock::test]
async fn conformance_namespaces_route_through_one_relay() {
	for version in ["moq-transport-18"] {
		routing(version, false).await;
	}
}

#[moq_clock::test]
async fn conformance_namespaces_route_through_a_lite_cluster() {
	for version in ["moq-transport-18"] {
		routing(version, true).await;
	}
}
