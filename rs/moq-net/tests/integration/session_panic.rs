#![cfg(feature = "async")]

//! A panic in one session's driver ends only that session.
//!
//! A host contains such a panic to the task driving the session (see
//! `moq_tokio::panic`). Unwinding drops the driver mid-poll, which must withdraw
//! the session's broadcasts from the shared origin and leave every shared lock
//! usable, so the other sessions on that origin keep working.
//!
//! Deterministic: simulated time over the mock transport.

use crate::support;

use std::time::Duration;

use moq_net::{Client, Hop, Server, Session, Version, announce, origin};
use support::harness::{now, spawn};
use support::mock::{MockSession, create_mock_session_pair};

const TIMEOUT: Duration = Duration::from_secs(10);

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	spawn(driver);
	producer
}

/// A client publishing `name` into `relay`, with the relay side's driver left
/// for the caller to run.
struct Publisher {
	client: Session,
	server: Session,
	server_transport: MockSession,
	server_driver: moq_clock::sim::JoinHandle<moq_net::Error>,
	/// The client's origin, which it publishes from.
	origin: origin::Producer,
	_broadcast: moq_net::origin::Publication,
}

async fn publish(version: Version, relay: &origin::Producer, hop: u64, name: &str) -> Publisher {
	let origin = produce_origin(hop);
	let broadcast = origin.create_broadcast(name).unwrap();
	broadcast.announce(Default::default()).unwrap();

	let mut mock = moq_net::transport::memory::Config::default();
	mock.protocol = Some(version.alpn().to_string());
	let (client_transport, server_transport) = create_mock_session_pair(mock);
	let client = Client::new()
		.with_versions(version.into())
		.with_publisher(origin.consume());
	let server = Server::new()
		.with_versions(version.into())
		.with_subscriber(relay.clone());

	let client = async {
		let (session, driver) = client.connect(now(), client_transport).await.unwrap();
		spawn(driver);
		session
	};
	let server = async {
		let (session, mut driver) = server.accept(now(), server_transport.clone()).await.unwrap();
		let driver = moq_clock::sim::spawn(moq_clock::drive(move |now, waiter| {
			moq_net::time::Driver::poll(&mut driver, now, waiter)
		}));
		(session, driver)
	};
	let (client, (server, server_driver)) = futures::join!(client, server);

	Publisher {
		client,
		server,
		server_transport,
		server_driver,
		origin,
		_broadcast: broadcast,
	}
}

/// Wait until `path` is announced (`true`) or withdrawn (`false`) on `announced`.
async fn until(announced: &mut announce::Consumer, path: &str, active: bool) {
	let wait = async {
		loop {
			let (announce, now_active) = match announced.next().await.expect("origin closed") {
				announce::Event::Start(announce)
				| announce::Event::Update(announce)
				| announce::Event::Restart(announce) => (announce, true),
				announce::Event::End(announce) => (announce, false),
			};
			if announce.prefix.as_str() == path && now_active == active {
				return;
			}
		}
	};
	moq_clock::timeout(TIMEOUT, wait)
		.await
		.unwrap_or_else(|_| panic!("{path} never became active={active}"));
}

#[moq_clock::test]
async fn a_panicking_session_leaves_the_others_serving() {
	for version in ["moq-lite-07-wip", "moq-transport-22"] {
		let version: Version = version.parse().unwrap();
		let relay = produce_origin(100);
		let mut announced = relay.consume().announced();

		let broken = publish(version, &relay, 1, "broken").await;
		until(&mut announced, "broken", true).await;
		let healthy = publish(version, &relay, 2, "healthy").await;
		until(&mut announced, "healthy", true).await;

		broken.server_transport.panic_on_stats();
		let _ = broken.server.stats();
		let ended = moq_clock::timeout(TIMEOUT, broken.server_driver)
			.await
			.expect("the driver never sampled the stats");
		assert!(
			ended.is_err_and(|err| err.is_panic()),
			"{version}: the injected panic did not fire"
		);

		until(&mut announced, "broken", false).await;
		moq_clock::timeout(TIMEOUT, broken.server.closed())
			.await
			.unwrap_or_else(|_| panic!("{version}: the panicked session never closed"));

		// The other session still serves: a fresh announcement reaches the relay.
		let later = healthy.origin.create_broadcast("later").unwrap();
		later.announce(Default::default()).unwrap();
		until(&mut announced, "later", true).await;
		crate::support::announced(&relay.consume(), "healthy").await;
		drop((healthy, broken.client, later));
	}
}
