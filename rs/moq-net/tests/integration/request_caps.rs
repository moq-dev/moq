#![cfg(feature = "async")]

//! A server bounds what one client can make it hold: stream flow control bounds its
//! requests, and a client past [`moq_net::session::Limits::announces`] loses the session
//! with TOO_MANY_REQUESTS. Credit is granted back as requests close, so a client that keeps
//! opening and closing requests never stalls.

use crate::support;

use std::time::Duration;

use moq_net::{Hop, Version, session::Limits};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(10);

/// A moq-transport draft and the lite draft production runs.
const VERSIONS: [&str; 2] = ["moq-transport-18", "moq-lite-06"];

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

/// A server publishing `bcast` with `tracks`, and a client subscribed to its origin.
struct Setup {
	client: moq_net::origin::Producer,
	_server: moq_net::origin::Producer,
	_broadcast: moq_net::origin::Publication,
	tracks: Vec<moq_net::track::Producer>,
	_pair: MockPair,
}

async fn setup(version: &str, limits: Limits, tracks: usize) -> Setup {
	let version: Version = version.parse().unwrap();
	let server = produce_origin(1);
	let client = produce_origin(2);

	let broadcast = server.create_broadcast("bcast").unwrap();
	let tracks = (0..tracks)
		.map(|i| broadcast.create_track(format!("t{i}"), None).unwrap())
		.collect();
	broadcast.announce(Default::default()).unwrap();

	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(server.consume());
	options.client_subscribe = Some(client.clone());
	options.server_limits = Some(limits);
	let pair = connect_mock(options).await;

	Setup {
		client,
		_server: server,
		_broadcast: broadcast,
		tracks,
		_pair: pair,
	}
}

fn limits(announces: usize) -> Limits {
	let mut limits = Limits::default();
	limits.announces = announces;
	limits
}

/// Many more requests than the stream credit admits at once, opened and closed in turn, all
/// succeed: each one that closes is granted back.
#[moq_clock::test]
async fn closed_requests_are_granted_back() {
	const ROUNDS: usize = 64;

	for version in VERSIONS {
		let setup = setup(version, limits(0), ROUNDS).await;
		let broadcast = moq_clock::timeout(
			TIMEOUT,
			setup.client.consume().follow("bcast", None).unwrap().available(),
		)
		.await
		.expect("announce timeout")
		.unwrap();

		for round in 0..ROUNDS {
			let track = broadcast.track(&format!("t{round}")).unwrap();
			let subscriber = moq_clock::timeout(TIMEOUT, track.subscribe(None))
				.await
				.unwrap_or_else(|_| panic!("{version}: round {round} stalled"))
				.unwrap_or_else(|err| panic!("{version}: round {round} refused: {err}"));
			drop(subscriber);
			drop(track);

			// Wait for the server to see it end, so the next round is a fresh request.
			let published = &setup.tracks[round];
			moq_clock::timeout(TIMEOUT, crate::support::unwanted(published.demand()))
				.await
				.unwrap_or_else(|_| panic!("{version}: round {round} never ended"))
				.unwrap();
		}
	}
}

/// With no subscription cap, a client asking for more subscriptions than the server admits
/// still holds no more than the stream credit at once. The rest wait rather than close the
/// session.
#[moq_clock::test]
async fn flow_control_bounds_the_subscriptions_a_peer_holds() {
	// 8 bidi streams toward the server.
	const BOUND: usize = 8;
	const TRACKS: usize = 3 * BOUND;

	for version in VERSIONS {
		let version: Version = version.parse().unwrap();
		let server = produce_origin(1);
		let client = produce_origin(2);
		let broadcast = server.create_broadcast("bcast").unwrap();
		let tracks: Vec<_> = (0..TRACKS)
			.map(|i| broadcast.create_track(format!("t{i}"), None).unwrap())
			.collect();
		broadcast.announce(Default::default()).unwrap();

		let mut options = MockConnectOptions::new(version);
		options.transport.max_concurrent_bi_streams = Some(BOUND);
		options.server_publish = Some(server.consume());
		options.client_subscribe = Some(client.clone());
		options.server_limits = Some(limits(0));
		let pair = connect_mock(options).await;

		let remote = moq_clock::timeout(TIMEOUT, client.consume().follow("bcast", None).unwrap().available())
			.await
			.expect("announce timeout")
			.unwrap();
		let subscribing: Vec<_> = (0..TRACKS)
			.map(|i| {
				let track = remote.track(&format!("t{i}")).unwrap();
				moq_clock::sim::spawn(async move {
					let _subscriber = track.subscribe(None).await;
					std::future::pending::<()>().await
				})
			})
			.collect();
		moq_clock::sleep(Duration::from_secs(1)).await;

		let held = tracks.iter().filter(|track| track.demand().wanted().is_some()).count();
		assert!(held > 0, "{version}: nothing was subscribed");
		assert!(held <= BOUND, "{version}: the server holds {held} subscriptions");
		assert!(
			moq_clock::timeout(Duration::from_millis(100), pair.client.closed())
				.await
				.is_err(),
			"{version}: the session closed"
		);
		for task in subscribing {
			task.abort();
		}
	}
}

/// A broadcast announced past the cap closes the session with TOO_MANY_REQUESTS.
#[moq_clock::test]
async fn an_announce_past_the_cap_closes_the_session() {
	for version in VERSIONS {
		let version: Version = version.parse().unwrap();
		let client = produce_origin(1);
		let server = produce_origin(2);
		let a = client.create_broadcast("a").unwrap();
		let b = client.create_broadcast("b").unwrap();
		a.announce(Default::default()).unwrap();
		b.announce(Default::default()).unwrap();

		let mut options = MockConnectOptions::new(version);
		options.client_publish = Some(client.consume());
		options.server_subscribe = Some(server.clone());
		options.server_limits = Some(limits(1));
		let pair = connect_mock(options).await;

		let err = moq_clock::timeout(TIMEOUT, pair.client.closed())
			.await
			.unwrap_or_else(|_| panic!("{version}: the session stayed open"));
		assert!(
			matches!(err, moq_net::Error::Session(moq_net::SessionError::TooManyRequests)),
			"{version}: closed with {err}"
		);
	}
}
