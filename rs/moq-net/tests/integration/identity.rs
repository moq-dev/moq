#![cfg(feature = "async")]

//! A route's identity across each wire: lite-07 carries the publisher's, and every other
//! version carries none, so the receiving session mints one per announcement.

use crate::support;

use std::time::Duration;

use moq_net::{Hop, Identity, Version, announce, origin};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

async fn link(version: Version, from: &origin::Producer, to: &origin::Producer) -> MockPair {
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(from.consume());
	options.client_subscribe = Some(to.clone());
	connect_mock(options).await
}

/// The identity of the next route `announced` starts, failing on any other event.
async fn started(announced: &mut announce::Consumer) -> Identity {
	match moq_clock::timeout(Duration::from_secs(10), announced.next())
		.await
		.expect("announce hung")
		.expect("announce ended")
	{
		announce::Event::Start(announce) => announce.route.identity.expect("a delivered route has an identity"),
		other => panic!("expected a start, got {other:?}"),
	}
}

async fn ended(announced: &mut announce::Consumer) {
	match moq_clock::timeout(Duration::from_secs(10), announced.next())
		.await
		.expect("announce hung")
		.expect("announce ended")
	{
		announce::Event::End(_) => {}
		other => panic!("expected an end, got {other:?}"),
	}
}

/// A job announced, withdrawn and announced again on one session, under a stable identity.
/// lite-07 delivers that identity both times, so the second run continues the first. Every
/// other wire mints one per announcement, so the second run is a stream of its own and is
/// never spliced onto the first, and neither is the publisher's.
async fn job_reannounced(version: &str) {
	let version: Version = version.parse().unwrap();
	let publisher = produce_origin(1);
	let relay = produce_origin(2);
	let identity: Identity = "job".parse().unwrap();
	let job = publisher
		.publish("job", origin::Route::default().with_identity(identity.clone(), 0))
		.unwrap();
	let _session = link(version, &publisher, &relay).await;
	let mut announced = relay.consume().announced();

	let first = started(&mut announced).await;
	job.unannounce();
	ended(&mut announced).await;
	job.announce(origin::Route::default()).unwrap();
	let second = started(&mut announced).await;

	match version.to_string().as_str() {
		"moq-lite-07-wip" => {
			assert_eq!(first, identity, "{version}");
			assert_eq!(second, identity, "{version}");
		}
		_ => {
			assert_ne!(first, identity, "{version}: an identity crossed a wire that has none");
			assert_ne!(second, first, "{version}: a re-announcement continued the old stream");
		}
	}
}

#[moq_clock::test]
async fn lite07_carries_the_identity() {
	job_reannounced("moq-lite-07-wip").await;
}

#[moq_clock::test]
async fn older_lite_mints_per_announcement() {
	for version in ["moq-lite-06", "moq-lite-05", "moq-lite-03"] {
		job_reannounced(version).await;
	}
}

#[moq_clock::test]
async fn moq_transport_mints_per_announcement() {
	for version in ["moq-transport-18", "moq-transport-22"] {
		job_reannounced(version).await;
	}
}
