#![cfg(feature = "async")]

//! A route's instance across each wire: lite-07 carries the publisher's, and every other
//! version carries none, so the receiving session mints one per announcement.

use crate::support;

use std::time::Duration;

use moq_net::{Instance, Version, origin};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(std::num::NonZeroU64::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer.with_proxy(true)
}

async fn link(version: Version, from: &origin::Producer, to: &origin::Producer) -> MockPair {
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(from.consume());
	options.client_subscribe = Some(to.clone());
	connect_mock(options).await
}

/// The instance of the next route `announced` starts, failing on any other event.
async fn started(announced: &mut origin::Routes) -> Instance {
	match kio::clock::timeout(Duration::from_secs(10), announced.next())
		.await
		.expect("announce hung")
		.expect("announce ended")
	{
		origin::RouteEvent {
			kind: origin::RouteKind::Start,
			announce,
		} => announce.route.instance().clone(),
		other => panic!("expected a start, got {other:?}"),
	}
}

async fn ended(announced: &mut origin::Routes) {
	match kio::clock::timeout(Duration::from_secs(10), announced.next())
		.await
		.expect("announce hung")
		.expect("announce ended")
	{
		origin::RouteEvent {
			kind: origin::RouteKind::End | origin::RouteKind::Offline,
			..
		} => {}
		other => panic!("expected an end, got {other:?}"),
	}
}

/// A job announced, withdrawn and announced again on one session, under a stable instance.
/// lite-07 delivers that instance both times, so the second run continues the first. Every
/// other wire mints one per announcement, so the second run is a stream of its own and is
/// never spliced onto the first, and neither is the publisher's.
async fn job_reannounced(version: &str) {
	let version: Version = version.parse().unwrap();
	let publisher = produce_origin(1);
	let relay = produce_origin(2);
	// Text like the mark minted instances once carried, which names an instance like any other.
	let instance: Instance = "~job".parse().unwrap();
	let job = publisher
		.route(
			"job",
			moq_net::origin::RouteOptions::default().with_instance(instance.clone(), 0),
		)
		.unwrap();
	let _session = link(version, &publisher, &relay).await;
	let mut announced = relay.consume().routes();

	let first = started(&mut announced).await;
	job.withdraw();
	ended(&mut announced).await;
	job.announce().unwrap();
	let second = started(&mut announced).await;

	match version.to_string().as_str() {
		"moq-lite-07-wip" => {
			assert_eq!(first, instance, "{version}");
			assert_eq!(second, instance, "{version}");
		}
		_ => {
			assert_ne!(first, instance, "{version}: an instance crossed a wire that has none");
			assert_ne!(second, first, "{version}: a re-announcement continued the old stream");
		}
	}
}

#[kio::test]
async fn lite07_carries_the_instance() {
	job_reannounced("moq-lite-07-wip").await;
}

#[kio::test]
async fn older_lite_mints_per_announcement() {
	for version in ["moq-lite-06", "moq-lite-05", "moq-lite-03"] {
		job_reannounced(version).await;
	}
}

#[kio::test]
async fn moq_transport_mints_per_announcement() {
	for version in ["moq-transport-18", "moq-transport-22"] {
		job_reannounced(version).await;
	}
}

/// A session mints an instance for a route an older wire delivered, and lite-07 forwards it
/// still minted, so every hop past it names the same stream.
#[kio::test]
async fn lite07_forwards_a_minted_instance() {
	let publisher = produce_origin(1);
	let relay = produce_origin(2);
	let viewer = produce_origin(3);
	let _route = publisher.route("job", Default::default()).unwrap();
	let _older = link("moq-lite-06".parse().unwrap(), &publisher, &relay).await;
	let _newer = link("moq-lite-07-wip".parse().unwrap(), &relay, &viewer).await;
	let minted = started(&mut relay.consume().routes()).await;
	assert!(minted.is_minted());
	assert_eq!(started(&mut viewer.consume().routes()).await, minted);
}

/// One publisher reaches a relay through two neighbors, and the one the relay prefers
/// goes away. lite-07 carries the publisher's instance through both, so the relay keeps
/// serving the same stream through the other. Every other wire mints one per announcement, so changing
/// the next hop, with the same origin and no epoch, restarts the route rather than
/// updating it in place.
async fn next_hop_lost(version: &str) {
	let version: Version = version.parse().unwrap();
	let publisher = produce_origin(1);
	let preferred = produce_origin(2);
	let standby = produce_origin(3);
	let relay = produce_origin(4);
	let _job = publisher
		.route("job", moq_net::origin::RouteOptions::default())
		.unwrap();
	let _to_preferred = link(version, &publisher, &preferred).await;
	let _to_standby = link(version, &publisher, &standby).await;
	let mut announced = relay.consume().routes();
	let first_hop = link(version, &preferred, &relay).await;
	let first = started(&mut announced).await;
	let _second_hop = link(
		version,
		&standby,
		&relay.clone().with_preference(origin::Preference::new(-1)),
	)
	.await;
	if let Ok(event) = kio::clock::timeout(Duration::from_secs(1), announced.next()).await {
		panic!("{version}: the standby route changed the preferred one: {event:?}");
	}

	first_hop.server.abort(moq_net::Error::Cancel);
	first_hop.client.abort(moq_net::Error::Cancel);
	let event = kio::clock::timeout(Duration::from_secs(10), announced.next()).await;
	match version.to_string().as_str() {
		// The relay already serves the claim through every feasible route, so the
		// standby carries on with nothing to announce.
		"moq-lite-07-wip" => {
			if let Ok(event) = event {
				panic!("{version}: expected the same stream to continue, got {event:?}");
			}
			kio::clock::timeout(Duration::from_secs(10), relay.consume().request("job", None))
				.await
				.expect("request hung")
				.expect("the standby serves the claim");
		}
		_ => match event.expect("announce hung").expect("announce ended") {
			origin::RouteEvent {
				kind: origin::RouteKind::Restart,
				announce,
			} => {
				assert_ne!(
					announce.route.instance(),
					&first,
					"{version}: spliced onto the old stream"
				)
			}
			other => panic!("{version}: expected a restart, got {other:?}"),
		},
	}
}

#[kio::test]
async fn next_hop_lost_lite07() {
	next_hop_lost("moq-lite-07-wip").await;
}

#[kio::test]
async fn next_hop_lost_restarts_older_lite() {
	for version in ["moq-lite-06", "moq-lite-03"] {
		next_hop_lost(version).await;
	}
}

#[kio::test]
async fn next_hop_lost_restarts_moq_transport() {
	next_hop_lost("moq-transport-22").await;
}
