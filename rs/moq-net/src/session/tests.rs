//! [`Session::poll_idle`] reports a session that carries no subscription or fetch in either
//! direction, which is when a migrated session can close without cutting anything off.
//!
//! Deterministic: simulated time over the in-memory transport.

use std::time::Duration;

use crate::model::ProduceTest;
use crate::{Session, Timestamp, Version};

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> crate::track::Info {
	crate::track::Info::default().with_timescale(crate::Timescale::MILLI)
}

const TIMEOUT: Duration = Duration::from_secs(10);

fn origin(hop: u64) -> crate::origin::Producer {
	crate::origin::Config::new(std::num::NonZeroU64::new(hop).unwrap()).produce()
}

fn idle(session: &Session) -> bool {
	session.poll_idle(&kio::Waiter::noop()).is_ready()
}

/// Wait for `session` to go idle, naming `what` if it never does.
async fn settles(session: &Session, what: &str) {
	kio::clock::timeout(TIMEOUT, kio::wait(|waiter| session.poll_idle(waiter)))
		.await
		.unwrap_or_else(|_| panic!("{what}"));
}

/// The client and server sessions of one in-memory connection.
struct Pair {
	client: Session,
	server: Session,
	/// The client's end of the transport, for sending raw datagrams.
	client_transport: crate::transport::memory::Session,
}

/// A client publishing one announced track to a server that subscribes to whatever it hears.
struct Setup {
	pair: Pair,
	track: crate::track::Producer,
	remote: crate::broadcast::Consumer,
	broadcast: crate::origin::Route,
}

async fn setup(version: &str) -> Setup {
	let version: Version = version.parse().unwrap();
	let publisher = origin(1);
	let broadcast = publisher.route("bcast", crate::origin::RouteOptions::hidden()).unwrap();
	let track = broadcast.broadcast().create_track("video", timed()).unwrap();
	broadcast.announce().unwrap();

	let subscriber = origin(2);
	let (client, server) = crate::transport::memory::pair(crate::transport::memory::Config {
		protocol: Some(version.alpn().to_string()),
		..Default::default()
	});
	let client_transport = client.clone();
	let dial = crate::Client::new()
		.with_versions(version.into())
		.with_publisher(publisher.consume());
	let serve = crate::Server::new()
		.with_versions(version.into())
		.with_subscriber(subscriber.clone());
	let (client, server) = futures::join!(
		async {
			let (session, driver) = dial.connect(kio::clock::now(), client).await.unwrap();
			drop(kio::clock::sim::spawn(crate::time::run(driver)));
			session
		},
		async {
			let (session, driver) = serve.accept(kio::clock::now(), server).await.unwrap();
			drop(kio::clock::sim::spawn(crate::time::run(driver)));
			session
		},
	);

	let remote = kio::clock::timeout(TIMEOUT, subscriber.consume().follow("bcast", None).unwrap().available())
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves")
		.unwrap();

	Setup {
		pair: Pair {
			client,
			server,
			client_transport,
		},
		track,
		remote,
		broadcast,
	}
}

/// An announcement alone leaves both ends idle. A subscription keeps the end serving it
/// and the end holding it busy until the reader leaves.
#[kio::test]
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
			broadcast: _broadcast,
		} = setup(version).await;
		assert!(idle(&pair.client), "{version}: an announcement kept the publisher busy");
		assert!(
			idle(&pair.server),
			"{version}: an announcement kept the subscriber busy"
		);

		let mut group = track.append_group().unwrap();
		group.write_frame(Timestamp::ZERO, b"live".as_ref()).unwrap();
		group.finish().unwrap();
		let mut sub = kio::clock::timeout(TIMEOUT, remote.track("video").unwrap().subscribe(None))
			.await
			.expect("subscribe timeout")
			.expect("subscribe");
		let received = kio::clock::timeout(TIMEOUT, sub.recv_group())
			.await
			.expect("recv timeout")
			.expect("recv")
			.expect("the track ended");
		assert_eq!(received.start, 0, "{version}");
		assert!(!idle(&pair.client), "{version}: idle while serving a subscription");
		assert!(!idle(&pair.server), "{version}: idle while holding a subscription");

		drop((received, sub));
		settles(
			&pair.server,
			&format!("{version}: the subscriber stayed busy after the reader left"),
		)
		.await;
		settles(
			&pair.client,
			&format!("{version}: the publisher stayed busy after the reader left"),
		)
		.await;
	}
}

/// A fetch keeps both ends busy until it ends: here, until the group it reads finishes.
/// moq-transport refuses a FETCH of the whole live group rather than waiting on it.
#[kio::test]
async fn a_fetch_keeps_both_ends_busy() {
	for version in ["moq-lite-06", "moq-lite-07-wip"] {
		let Setup {
			pair,
			track,
			remote,
			broadcast: _broadcast,
		} = setup(version).await;

		let mut writing = track.append_group().unwrap();
		writing.write_frame(Timestamp::ZERO, b"first".as_ref()).unwrap();
		let fetching = remote.track("video").unwrap();
		let fetch = kio::clock::sim::spawn(async move {
			let mut fetched = fetching.fetch_group(0, None).await?;
			let mut frames = Vec::new();
			while let Some(frame) = fetched.read_frame().await? {
				frames.push(frame.payload);
			}
			Ok::<_, crate::Error>(frames)
		});
		// Simulated: every task runs until the fetch waits on the unfinished group.
		kio::clock::sleep(Duration::from_secs(1)).await;
		assert!(!idle(&pair.client), "{version}: idle while serving a fetch");
		assert!(!idle(&pair.server), "{version}: idle while a fetch is in flight");

		writing.write_frame(Timestamp::ZERO, b"last".as_ref()).unwrap();
		writing.finish().unwrap();
		let frames = kio::clock::timeout(TIMEOUT, fetch)
			.await
			.expect("fetch timeout")
			.expect("fetch task")
			.expect("fetch");
		assert_eq!(frames, [&b"first"[..], &b"last"[..]], "{version}");
		settles(
			&pair.server,
			&format!("{version}: the fetching side stayed busy after the fetch"),
		)
		.await;
		settles(
			&pair.client,
			&format!("{version}: the serving side stayed busy after the fetch"),
		)
		.await;
	}
}

/// A track's end waits for the subscriber to read the datagrams queued with it, so a flood
/// that keeps its receive queue full delays the end. Meanwhile the track keeps its stream, and
/// the session stays busy, so the peer's stream credit bounds the ends a flood can leave queued.
#[kio::test]
async fn a_track_ended_under_a_datagram_flood_keeps_its_stream() {
	use crate::transport::poll::Session as _;

	for version in ["moq-lite-05", "moq-lite-06", "moq-lite-07-wip"] {
		let Setup {
			pair,
			remote,
			broadcast,
			..
		} = setup(version).await;
		let info = crate::datagram::Info::default().with_timescale(crate::Timescale::MILLI);
		let producer = broadcast.broadcast().create_datagrams("audio", info).unwrap();
		let mut consumer = remote.datagrams(producer.name()).unwrap();
		// The consumer takes a priority first, so the publisher seeing it can't be anything but
		// this consumer's request.
		consumer.set_priority(1);
		kio::clock::timeout(TIMEOUT, async {
			let mut priority = producer.priority();
			while priority != Some(1) {
				priority = kio::wait(|waiter| producer.poll_priority(waiter, priority)).await;
			}
		})
		.await
		.expect("the request never reached the publisher");
		kio::clock::timeout(TIMEOUT, consumer.info())
			.await
			.expect("info timeout")
			.expect("the publisher answered");

		// Each names no subscription, so the subscriber reads and drops it. The queue holds 1024.
		let flood = || {
			for _ in 0..512 {
				let _ = pair.client_transport.send_datagram(b"x"[..].into(), Default::default());
			}
		};
		flood();
		producer.finish();
		for _ in 0..32 {
			flood();
			// One turn for the session's tasks, which read at most a budget each.
			let mut turned = false;
			kio::wait(|waiter| {
				if std::mem::replace(&mut turned, true) {
					return std::task::Poll::Ready(());
				}
				waiter.waker().wake_by_ref();
				std::task::Poll::Pending
			})
			.await;
			assert!(
				!idle(&pair.server),
				"{version}: the track released its stream before its end applied"
			);
		}
		let end = kio::clock::timeout(TIMEOUT, consumer.recv())
			.await
			.expect("recv timeout");
		assert!(end.unwrap().is_none(), "{version}");
		settles(&pair.server, &format!("{version}: the track never released its stream")).await;
	}
}
