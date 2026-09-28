//! A peer asking for announcements below a route's prefix hears that route as
//! covering the prefix it asked for, the way a local consumer rooted there does.
//!
//! The route `.dash` serves every path beneath it, so a cursor rooted at
//! `.dash/nobody` sees it at its own root. Across a session the publisher must
//! translate it into the requested scope rather than end the session.

mod support;

use std::time::Duration;

use moq_net::{Hop, Pattern, Patterns, Version, origin};
use support::harness::{MockConnectOptions, connect_mock};

/// Long enough, in virtual time, for anything in flight to reach the far side.
const SETTLE: Duration = Duration::from_secs(1);

const SERVED: &str = ".dash";
const REQUESTED: &str = ".dash/nobody";

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	tokio::spawn(support::harness::run(driver));
	producer
}

/// Drain every update the cursor has pending, as `kind prefix` lines.
fn drain(announced: &mut moq_net::announce::Consumer) -> Vec<String> {
	let mut seen = Vec::new();
	while let Some(update) = announced.try_next() {
		seen.push(format!("{:?} {}", update.kind, update.prefix));
	}
	seen
}

/// Announce `.dash`, then a path beneath the requested prefix, and record what a
/// cursor rooted at `.dash/nobody` sees, either on the publishing origin or on a
/// subscriber whose announce interest is that prefix.
async fn covering(version: Option<&str>) -> Vec<String> {
	let publisher = produce_origin(1);
	let everything = Patterns::from(Pattern::all());

	let (observer, pair) = match version {
		None => (publisher.consume(), None),
		Some(version) => {
			let subscriber = produce_origin(2);
			let interest = Patterns::from(Pattern::subtree(REQUESTED).unwrap());
			let scoped = subscriber.scope("", &interest).unwrap();
			let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
			options.server_publish = Some(publisher.consume());
			options.client_subscribe = Some(scoped);
			(subscriber.consume(), Some(connect_mock(options).await))
		}
	};
	let mut announced = observer.scope(REQUESTED, &everything).unwrap().announced();
	let mut log = Vec::new();

	let served = publisher.create_broadcast(SERVED).unwrap();
	served.announce(Default::default()).unwrap();
	tokio::time::sleep(SETTLE).await;
	log.push(format!("served: {:?}", drain(&mut announced)));

	let below = publisher.create_broadcast(format!("{REQUESTED}/cam")).unwrap();
	below.announce(Default::default()).unwrap();
	tokio::time::sleep(SETTLE).await;
	log.push(format!("below: {:?}", drain(&mut announced)));

	if let Some(pair) = pair {
		let closed = tokio::time::timeout(SETTLE, pair.client.closed()).await;
		log.push(format!("session open: {}", closed.is_err()));
	}

	log
}

const EXPECTED: &[&str] = &["served: [\"Announced \"]", "below: [\"Announced cam\"]"];

#[tokio::test]
async fn local_cursor_sees_the_covering_route_at_its_root() {
	tokio::time::pause();
	assert_eq!(covering(None).await, EXPECTED);
}

#[tokio::test]
async fn remote_lite_peer_hears_the_covering_route() {
	tokio::time::pause();
	let mut expected = EXPECTED.to_vec();
	expected.push("session open: true");
	for version in [
		"moq-lite-03",
		"moq-lite-04",
		"moq-lite-05",
		"moq-lite-06",
		"moq-lite-07-wip",
	] {
		assert_eq!(covering(Some(version)).await, expected, "{version}");
	}
}

#[tokio::test]
async fn remote_ietf_peer_hears_the_covering_route() {
	tokio::time::pause();
	let mut expected = EXPECTED.to_vec();
	expected.push("session open: true");
	for version in [
		"moq-transport-14",
		"moq-transport-15",
		"moq-transport-16",
		"moq-transport-17",
		"moq-transport-18",
		"moq-transport-19",
		"moq-transport-20",
		"moq-transport-21",
		"moq-transport-22",
	] {
		assert_eq!(covering(Some(version)).await, expected, "{version}");
	}
}
