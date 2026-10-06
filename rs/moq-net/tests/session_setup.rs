//! `Session::setup` waits for the server's SETUP, and reports the session's close reason.
//!
//! Deterministic: paused time over the mock transport.

mod support;

use std::time::Duration;

use moq_net::{Client, Error, Server, Session, SessionError, Version, server::Handshake};
use support::{
	harness::{now, spawn},
	mock::{MockSession, create_mock_session_pair},
};

const TIMEOUT: Duration = Duration::from_secs(10);

/// Versions whose server sends its SETUP after
/// `Client::connect` has returned.
const LATE: [&str; 9] = [
	"moq-lite-05",
	"moq-lite-06",
	"moq-lite-07-wip",
	"moq-transport-17",
	"moq-transport-18",
	"moq-transport-19",
	"moq-transport-20",
	"moq-transport-21",
	"moq-transport-22",
];

/// Versions whose handshake reads the server's SETUP before `Client::connect` returns.
const EARLY: [&str; 5] = [
	"moq-lite-01",
	"moq-lite-02",
	"moq-transport-14",
	"moq-transport-15",
	"moq-transport-16",
];

/// Versions with no server SETUP at all.
const NONE: [&str; 2] = ["moq-lite-03", "moq-lite-04"];

fn version(name: &str) -> Version {
	name.parse().unwrap()
}

/// Dial a `LATE` version, returning the client's session and the server's request,
/// still unanswered.
async fn dial(version: Version) -> (Session, Handshake<MockSession>) {
	let (client, server) = create_mock_session_pair(Some(version.alpn()));
	let client = async {
		let (session, driver) = Client::new()
			.with_versions(version.into())
			.connect(now(), client)
			.await
			.expect("client handshake failed");
		spawn(driver);
		session
	};
	let server = async {
		Server::new()
			.with_versions(version.into())
			.accept_request(now(), server)
			.await
			.expect("server never saw the request")
	};
	futures::join!(client, server)
}

async fn setup(session: &Session) -> Option<Result<(), Error>> {
	moq_net_sim::timeout(TIMEOUT, session.setup()).await.ok()
}

#[moq_net_sim::test]
async fn setup_waits_for_the_server_to_answer() {
	for name in LATE {
		let (session, request) = dial(version(name)).await;
		assert!(
			setup(&session).await.is_none(),
			"{name}: SETUP arrived before the server answered"
		);

		let (_server, driver) = request.ok().await.expect("server accept failed");
		spawn(driver);
		assert!(matches!(setup(&session).await, Some(Ok(()))), "{name}");
	}
}

/// A session that closed after its SETUP arrived reports the close reason: a caller that
/// checks only now must not take over a dead session.
#[moq_net_sim::test]
async fn setup_reports_a_close_after_the_setup() {
	for name in LATE {
		let (session, request) = dial(version(name)).await;
		let (server, driver) = request.ok().await.expect("server accept failed");
		spawn(driver);
		assert!(matches!(setup(&session).await, Some(Ok(()))), "{name}");

		server.abort(Error::Unauthorized);
		moq_net_sim::timeout(TIMEOUT, session.closed())
			.await
			.expect("the close never arrived");
		let res = session.setup().await;
		assert!(res.is_err(), "{name}: a closed session reported SETUP success");
	}
}

#[moq_net_sim::test]
async fn setup_reports_a_refusal() {
	for name in LATE {
		let (session, request) = dial(version(name)).await;
		request.close(Error::Unauthorized);
		let err = setup(&session)
			.await
			.expect("the refusal never arrived")
			.unwrap_err();
		assert!(
			matches!(err, Error::Session(SessionError::Unauthorized)),
			"{name}: {err:?}"
		);
	}
}

#[moq_net_sim::test]
async fn setup_resolves_at_once_when_the_handshake_read_the_setup() {
	for name in EARLY {
		let version = version(name);
		let (client, server) = create_mock_session_pair(Some(version.alpn()));
		let client = async {
			let (session, driver) = Client::new()
				.with_versions(version.into())
				.connect(now(), client)
				.await
				.expect("client handshake failed");
			spawn(driver);
			session
		};
		let server = async {
			let (session, driver) = Server::new()
				.with_versions(version.into())
				.accept(now(), server)
				.await
				.expect("server handshake failed");
			spawn(driver);
			session
		};
		let (session, _server) = futures::join!(client, server);
		let res = session.setup().await;
		assert!(res.is_ok(), "{name}: {res:?}");
	}
}

#[moq_net_sim::test]
async fn setup_is_unsupported_without_a_server_setup() {
	for name in NONE {
		let version = version(name);
		let (client, _server) = create_mock_session_pair(Some(version.alpn()));
		let (session, driver) = Client::new()
			.with_versions(version.into())
			.connect(now(), client)
			.await
			.expect("client handshake failed");
		spawn(driver);
		let res = session.setup().await;
		assert!(matches!(res, Err(Error::Unsupported)), "{name}: {res:?}");
	}
}

/// Every version is covered by exactly one of the tests above.
#[test]
fn every_version_is_classified() {
	let classified: Vec<&str> = LATE.iter().chain(&EARLY).chain(&NONE).copied().collect();
	let mut all: Vec<&str> = Version::names().collect();
	all.sort_unstable();
	let mut sorted = classified.clone();
	sorted.sort_unstable();
	assert_eq!(sorted, all);
}
