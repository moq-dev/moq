#![cfg(feature = "async")]

//! `Session::idle` reports a session that carries no subscription or fetch in either
//! direction, which is when a migrated session can close without cutting anything off.
//!
//! Deterministic: simulated time over the mock transport.

use crate::support;

use std::time::Duration;

use moq_net::{Hop, Timestamp, Version, kio};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

const TIMEOUT: Duration = Duration::from_secs(10);

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

fn idle(session: &moq_net::Session) -> bool {
	session.poll_idle(&kio::Waiter::noop()).is_ready()
}

/// A client publishing one announced track to a server that subscribes to whatever it hears.
struct Setup {
	pair: MockPair,
	track: moq_net::track::Producer,
	remote: moq_net::broadcast::Consumer,
	_broadcast: moq_net::origin::Publication,
}

async fn setup(version: &str) -> Setup {
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let track = broadcast.create_track("video", timed()).unwrap();
	broadcast.announce(Default::default()).unwrap();

	let subscriber = produce_origin(2);
	let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
	options.client_publish = Some(publisher.consume());
	options.server_subscribe = Some(subscriber.clone());
	let pair = connect_mock(options).await;

	let remote = moq_clock::timeout(TIMEOUT, subscriber.consume().follow("bcast", None).unwrap().available())
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves");

	Setup {
		pair,
		track,
		remote,
		_broadcast: broadcast,
	}
}

/// An announcement alone leaves both ends idle. A subscription keeps the end serving it
/// and the end holding it busy until the reader leaves.
#[moq_clock::test]
async fn a_subscription_keeps_both_ends_busy() {
	for version in [
		"moq-lite-04",
		"moq-lite-06",
		"moq-lite-07-wip",
		"moq-transport-18",
		"moq-transport-22",
	] {
		let Setup {
			pair,
			track,
			remote,
			_broadcast,
		} = setup(version).await;
		assert!(idle(&pair.client), "{version}: an announcement kept the publisher busy");
		assert!(
			idle(&pair.server),
			"{version}: an announcement kept the subscriber busy"
		);

		let mut group = track.append_group().unwrap();
		group.write_frame(Timestamp::ZERO, b"live".as_ref()).unwrap();
		group.finish().unwrap();
		let mut sub = moq_clock::timeout(TIMEOUT, remote.track("video").unwrap().subscribe(None))
			.await
			.expect("subscribe timeout")
			.expect("subscribe");
		let received = moq_clock::timeout(TIMEOUT, sub.recv_group())
			.await
			.expect("recv timeout")
			.expect("recv")
			.expect("the track ended");
		assert_eq!(received.sequence, 0, "{version}");
		assert!(!idle(&pair.client), "{version}: idle while serving a subscription");
		assert!(!idle(&pair.server), "{version}: idle while holding a subscription");

		drop((received, sub));
		moq_clock::timeout(TIMEOUT, pair.server.idle())
			.await
			.unwrap_or_else(|_| panic!("{version}: the subscriber stayed busy after the reader left"));
		moq_clock::timeout(TIMEOUT, pair.client.idle())
			.await
			.unwrap_or_else(|_| panic!("{version}: the publisher stayed busy after the reader left"));
	}
}

/// A fetch keeps both ends busy until it ends: here, until the group it reads finishes.
#[moq_clock::test]
async fn a_fetch_keeps_both_ends_busy() {
	for version in ["moq-lite-06", "moq-lite-07-wip", "moq-transport-22"] {
		let Setup {
			pair,
			track,
			remote,
			_broadcast,
		} = setup(version).await;

		let mut writing = track.append_group().unwrap();
		writing.write_frame(Timestamp::ZERO, b"first".as_ref()).unwrap();
		let fetching = remote.track("video").unwrap();
		let fetch = moq_clock::sim::spawn(async move {
			let mut fetched = fetching.fetch_group(0, None).await?;
			let mut frames = Vec::new();
			while let Some(frame) = fetched.read_frame().await? {
				frames.push(frame.payload);
			}
			Ok::<_, moq_net::Error>(frames)
		});
		// Simulated: every task runs until the fetch waits on the unfinished group.
		moq_clock::sleep(Duration::from_secs(1)).await;
		assert!(!idle(&pair.client), "{version}: idle while serving a fetch");
		assert!(!idle(&pair.server), "{version}: idle while a fetch is in flight");

		writing.write_frame(Timestamp::ZERO, b"last".as_ref()).unwrap();
		writing.finish().unwrap();
		let frames = moq_clock::timeout(TIMEOUT, fetch)
			.await
			.expect("fetch timeout")
			.expect("fetch task")
			.expect("fetch");
		assert_eq!(frames, [&b"first"[..], &b"last"[..]], "{version}");
		moq_clock::timeout(TIMEOUT, pair.server.idle())
			.await
			.unwrap_or_else(|_| panic!("{version}: the fetching side stayed busy after the fetch"));
		moq_clock::timeout(TIMEOUT, pair.client.idle())
			.await
			.unwrap_or_else(|_| panic!("{version}: the serving side stayed busy after the fetch"));
	}
}
