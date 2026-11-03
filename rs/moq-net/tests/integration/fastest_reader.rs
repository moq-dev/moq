#![cfg(feature = "async")]

//! A relay's ingest buffers at most a window past the group's fastest reader, then leaves
//! the rest in the stream, so flow control pushes back to the publisher. The window is the
//! stream receive credit the relay's transport grants, capped at 16 MiB, half the group's
//! live tail. A slower reader is never waited on: it falls off the group's front.

use std::time::Duration;

use bytes::Bytes;
use moq_net::{Hop, Timestamp, Version, origin, track, transport::ReceiveWindow};

use crate::support::harness::{MockPair, peer, spawn};

const TIMEOUT: Duration = Duration::from_secs(10);

const MB: usize = 1 << 20;

const VERSIONS: &[&str] = &["moq-lite-06", "moq-lite-07-wip", "moq-transport-22"];

/// Receive windows whose stream credit, 64 MiB, is past the 16 MiB pacing cap.
fn uncapped() -> ReceiveWindow {
	ReceiveWindow::new(u64::MAX, 64 * MB as u64)
}

fn origin(hop: u64) -> origin::Producer {
	let (origin, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	spawn(driver);
	origin
}

/// A publisher's track and a relay's subscription to it, over one peering.
struct Relay {
	track: track::Producer,
	subscriber: track::Subscriber,
	_broadcast: origin::Publication,
	pair: MockPair,
	_origins: [origin::Producer; 2],
}

async fn relay(version: &str) -> Relay {
	let publisher = origin(1);
	let relay = origin(2);
	let pair = peer(version.parse::<Version>().unwrap(), &publisher, &relay).await;

	let broadcast = publisher.create_broadcast("live").unwrap();
	let track = broadcast
		.create_track("data", track::Info::default().with_timescale(moq_net::Timescale::MILLI))
		.unwrap();
	broadcast.announce(Default::default()).unwrap();

	let remote = kio::clock::timeout(TIMEOUT, relay.consume().follow("live", None).unwrap().available())
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves");
	let subscriber = kio::clock::timeout(
		TIMEOUT,
		remote.track("data").unwrap().subscribe(
			track::Subscription::default()
				.with_delay(moq_net::track::Delay::default().with_max(Duration::from_secs(3600))),
		),
	)
	.await
	.expect("subscribe timeout")
	.expect("subscribe");
	Relay {
		track,
		subscriber,
		_broadcast: broadcast,
		pair,
		_origins: [publisher, relay],
	}
}

/// An idle reader holds the relay to about its stream credit, or the 16 MiB cap past it, and
/// still receives every frame.
#[kio::test]
async fn an_idle_reader_paces_the_relay_and_loses_nothing() {
	// The stream credit the relay grants, and how many 1 MiB frames it then holds.
	let cases = [
		(ReceiveWindow::new(16 * MB as u64, 4 * MB as u64), 3..=5),
		(uncapped(), 15..=18),
	];
	for version in VERSIONS {
		for (window, expected) in cases.clone() {
			idle_reader(version, window, expected).await;
		}
	}
}

async fn idle_reader(version: &str, window: ReceiveWindow, expected: std::ops::RangeInclusive<usize>) {
	let payload = Bytes::from(vec![7u8; MB]);
	let mut relay = relay(version).await;
	relay.pair.server_transport.set_receive_window(Some(window));

	let mut group = relay.track.append_group().unwrap();
	group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
	let mut reader = kio::clock::timeout(TIMEOUT, relay.subscriber.recv_group())
		.await
		.expect("group timeout")
		.unwrap()
		.expect("group 0");
	// Within the publisher's own 32 MB tail, so only the relay's pacing holds it back.
	for _ in 1..30 {
		group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
	}
	group.finish().unwrap();

	// Simulated time moves only once every task is parked: the relay's ingest stopped.
	kio::clock::sleep(Duration::from_secs(1)).await;
	let held = reader.frame_count();
	assert!(
		expected.contains(&held),
		"{version}: the relay read {held} MB ahead of a reader that took nothing under {window:?}"
	);

	for index in 0..30 {
		let frame = kio::clock::timeout(TIMEOUT, reader.read_frame())
			.await
			.unwrap_or_else(|_| panic!("{version}: frame {index} never arrived"))
			.unwrap_or_else(|err| panic!("{version}: frame {index}: {err}"))
			.expect("a frame");
		assert_eq!(frame.payload.len(), MB, "{version}: frame {index}");
	}
	let end = kio::clock::timeout(TIMEOUT, reader.read_frame())
		.await
		.expect("end timeout");
	assert!(matches!(end, Ok(None)), "{version}: {end:?}");
}

/// A paused stream is not read, so the relay watches it for a reset instead: a publisher
/// aborting the group ends the relay's copy while its reader still takes nothing.
#[kio::test]
async fn a_reset_reaches_a_paused_relay() {
	let payload = Bytes::from(vec![7u8; MB]);
	for version in VERSIONS {
		let mut relay = relay(version).await;

		let mut group = relay.track.append_group().unwrap();
		group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
		let mut reader = kio::clock::timeout(TIMEOUT, relay.subscriber.recv_group())
			.await
			.expect("group timeout")
			.unwrap()
			.expect("group 0");
		for _ in 1..30 {
			group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
		}
		kio::clock::sleep(Duration::from_secs(1)).await;
		assert!(reader.frame_count() < 30, "{version}: the relay never paused");

		group.abort(moq_net::Error::Cancel).unwrap();
		let end = kio::clock::timeout(TIMEOUT, reader.finished())
			.await
			.unwrap_or_else(|_| panic!("{version}: the reset never reached the paused relay"));
		assert!(end.is_err(), "{version}: {end:?}");
	}
}

/// A frame larger than the room left waits to be admitted, since admitting it trims the
/// tail to fit: an idle reader loses nothing to a large frame either.
///
/// The relay grants past the 16 MiB cap, so it takes the first 15 frames and the publisher
/// sheds only what it already sent.
#[kio::test]
async fn a_large_frame_sheds_nothing_an_idle_reader_wants() {
	let payload = Bytes::from(vec![7u8; MB]);
	for version in VERSIONS {
		let mut relay = relay(version).await;
		relay.pair.server_transport.set_receive_window(Some(uncapped()));

		let mut group = relay.track.append_group().unwrap();
		group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
		let mut reader = kio::clock::timeout(TIMEOUT, relay.subscriber.recv_group())
			.await
			.expect("group timeout")
			.unwrap()
			.expect("group 0");
		for _ in 1..15 {
			group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
		}
		// Let the relay take what fits, so the publisher's egress has sent every frame the
		// large one sheds from the publisher's own tail.
		kio::clock::sleep(Duration::from_secs(1)).await;
		group
			.write_frame(Timestamp::ZERO, Bytes::from(vec![7u8; 20 * MB]))
			.unwrap();
		group.finish().unwrap();

		kio::clock::sleep(Duration::from_secs(1)).await;
		assert_eq!(
			reader.frame_count(),
			15,
			"{version}: the large frame waits for the reader"
		);
		for index in 0..16 {
			let frame = kio::clock::timeout(TIMEOUT, reader.read_frame())
				.await
				.unwrap_or_else(|_| panic!("{version}: frame {index} never arrived"))
				.unwrap_or_else(|err| panic!("{version}: frame {index}: {err}"))
				.expect("a frame");
			let size = if index < 15 { MB } else { 20 * MB };
			assert_eq!(frame.payload.len(), size, "{version}: frame {index}");
		}
	}
}

/// A session whose paused streams would hold more than half its connection window reads
/// on instead, and its idle reader keeps what the live tail holds.
#[kio::test]
async fn a_session_past_its_credit_cap_reads_on() {
	let payload = Bytes::from(vec![7u8; MB]);
	for version in VERSIONS {
		let mut relay = relay(version).await;
		// One paused stream's credit is already more than half the connection window.
		let window = moq_net::transport::ReceiveWindow::new(MB as u64, MB as u64);
		relay.pair.server_transport.set_receive_window(Some(window));

		let mut group = relay.track.append_group().unwrap();
		group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
		let reader = kio::clock::timeout(TIMEOUT, relay.subscriber.recv_group())
			.await
			.expect("group timeout")
			.unwrap()
			.expect("group 0");
		for _ in 1..30 {
			group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
		}
		group.finish().unwrap();

		kio::clock::sleep(Duration::from_secs(1)).await;
		let held = reader.frame_count();
		assert_eq!(held, 30, "{version}: the relay paused past its credit cap");
	}
}

#[kio::test]
async fn a_slow_reader_falls_off_without_slowing_the_fast_one() {
	let payload = Bytes::from(vec![7u8; MB]);
	for version in VERSIONS {
		let mut relay = relay(version).await;

		let mut group = relay.track.append_group().unwrap();
		group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
		let mut fast = kio::clock::timeout(TIMEOUT, relay.subscriber.recv_group())
			.await
			.expect("group timeout")
			.unwrap()
			.expect("group 0");
		let mut slow = fast.clone();

		// Well past the 32 MB tail: the fast reader keeps up the whole way.
		for index in 0..48 {
			if index > 0 {
				group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
			}
			let frame = kio::clock::timeout(TIMEOUT, fast.read_frame())
				.await
				.unwrap_or_else(|_| panic!("{version}: the fast reader stalled at frame {index}"))
				.unwrap_or_else(|err| panic!("{version}: frame {index}: {err}"))
				.expect("a frame");
			assert_eq!(frame.payload.len(), MB, "{version}: frame {index}");
		}
		group.finish().unwrap();

		let fell = kio::clock::timeout(TIMEOUT, slow.read_frame())
			.await
			.expect("read timeout");
		assert!(
			matches!(fell, Err(moq_net::Error::Evicted)),
			"{version}: the slow reader got {fell:?}"
		);
	}
}

/// Backpressure crosses both relays, with one pacing window of unread payload per hop: the
/// 1 MiB stream credit each relay grants, not the 16 MiB cap. The upstream copy also keeps
/// sent history within its 32 MiB tail.
/// Cache charges include frame slots and relay metadata, not transport buffers or RSS.
#[kio::test]
async fn two_relays_bound_their_cache_and_resume_without_a_hole() {
	use moq_net::{frame, transport::ReceiveWindow};

	const FRAME: usize = 64 * 1024;
	const TOTAL: usize = 64 * MB / FRAME;
	for version in VERSIONS {
		let publisher = origin(1);
		let first = origin(2);
		let second = origin(3);
		let version = version.parse::<Version>().unwrap();
		let upstream = peer(version, &publisher, &first).await;
		let downstream = peer(version, &first, &second).await;
		for pair in [&upstream, &downstream] {
			pair.server_transport
				.set_receive_window(Some(ReceiveWindow::new(4 * MB as u64, MB as u64)));
		}
		let broadcast = publisher.create_broadcast("chain").unwrap();
		let track = broadcast
			.create_track("data", track::Info::default().with_timescale(moq_net::Timescale::MILLI))
			.unwrap();
		broadcast.announce(Default::default()).unwrap();
		let remote = second
			.consume()
			.follow("chain", None)
			.unwrap()
			.available()
			.await
			.unwrap();
		let mut subscriber = remote
			.track("data")
			.unwrap()
			.subscribe(
				track::Subscription::default().with_delay(track::Delay::default().with_max(Duration::from_secs(3600))),
			)
			.await
			.unwrap();
		crate::support::harness::subscribed(&track).await;
		let mut group = track.append_group().unwrap();
		let written = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
		let progress = written.clone();
		let writer = kio::clock::sim::spawn(async move {
			for index in 0..TOTAL {
				let payload = Bytes::from(vec![(index % 251) as u8; FRAME]);
				let info = frame::Info {
					size: FRAME as u64,
					timestamp: Some(Timestamp::ZERO),
				};
				group.ready(&info).await;
				group.write_frame(Timestamp::ZERO, payload).unwrap();
				progress.store(index + 1, std::sync::atomic::Ordering::Relaxed);
			}
			group.finish().unwrap();
		});
		let mut reader = subscriber.recv_group().await.unwrap().unwrap();
		// Simulated time advances only once all drivers and the paced writer park.
		kio::clock::sleep(Duration::from_secs(1)).await;
		let parked = written.load(std::sync::atomic::Ordering::Relaxed);
		assert!(parked < TOTAL, "{version}: no backpressure reached the producer");
		// Frame slots count toward pacing too, so the last whole frame can fall short. The
		// upstream copy also holds what it sent downstream, within its 32 MiB tail.
		for (hop, relay, max) in [(1, &first, 32 * MB + FRAME), (2, &second, MB + FRAME)] {
			let held = relay.config().pool.used();
			assert!(
				held >= (MB - FRAME) as u64,
				"{version}: hop {hop} never filled its window: {held}"
			);
			assert!(held < max as u64, "{version}: hop {hop} retained {held} bytes");
			eprintln!("{version}: hop {hop} parked cache={held} B, origin wrote {parked} frames");
		}
		for index in 0..TOTAL {
			let frame = kio::clock::timeout(TIMEOUT, reader.read_frame())
				.await
				.expect("chain stalled")
				.unwrap()
				.unwrap();
			assert_eq!(
				frame.payload,
				Bytes::from(vec![(index % 251) as u8; FRAME]),
				"{version}: frame {index}"
			);
			for relay in [&first, &second] {
				assert!(
					relay.config().pool.used() < 33 * MB as u64,
					"{version}: live tail grew past its budget"
				);
			}
		}
		assert!(reader.read_frame().await.unwrap().is_none());
		writer.await.unwrap();
	}
}
