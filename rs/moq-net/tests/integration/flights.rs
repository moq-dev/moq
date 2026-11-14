#![cfg(feature = "async")]

//! A late joiner of a live group far past its 32 MiB tail replays it from frame 0 through
//! a flight: each relay in the chain asks the hop above for the group from the frame it
//! shed, and the origin's handler serves it. The joiner hands over to the live copy once it
//! catches up, and every flight in the chain then stops.

use std::sync::{
	Arc,
	atomic::{AtomicU64, Ordering},
};
use std::task::Poll;
use std::time::Duration;

use bytes::Bytes;
use moq_net::{Version, cache, frame, group, origin, track};

use crate::support::AppendNext;
use crate::support::harness::{MockConnectOptions, connect_mock, spawn};

const TIMEOUT: Duration = Duration::from_secs(10);

/// 48 MiB of 16 KiB frames: half again the live tail.
const FRAMES: u32 = 3_000;

const VERSIONS: &[&str] = &["moq-lite-05", "moq-lite-06", "moq-lite-07-wip"];

/// Frame `index`: 16 KiB naming its index.
fn numbered(index: u32) -> Bytes {
	Bytes::from(index.to_le_bytes().repeat(4096))
}

fn number(frame: &frame::Frame) -> u32 {
	u32::from_le_bytes(frame.payload[..4].try_into().unwrap())
}

/// The header admission weighs: only its size.
fn info(payload: &Bytes) -> frame::Info {
	frame::Info {
		size: payload.len() as u64,
		timestamp: moq_net::Timestamp::unscaled(0),
	}
}

fn origin(hop: u64, pool: &cache::Pool) -> origin::Producer {
	let mut config = origin::Config::new(std::num::NonZeroU64::new(hop).unwrap());
	config.pool = pool.clone();
	let (origin, driver) = origin::Producer::new(config);
	spawn(driver);
	origin.with_proxy(true)
}

/// Write `payload` once `group`'s fastest reader leaves room, or return `false` once no
/// reader waits on it: its fill gave it up and nobody reads it. Without a payload, the
/// archive has nothing newer yet, so only wait for that.
async fn write_paced(
	group: &mut group::Producer,
	fill: &track::Fill,
	demand: &mut group::Demand,
	payload: Option<Bytes>,
) -> bool {
	let key = group.start;
	let room = kio::wait(|waiter| {
		while let Poll::Ready(Ok(_)) = demand.poll_wanted_changed(waiter) {}
		if fill.poll_unwanted(key, waiter).is_ready() && !demand.wanted() {
			return Poll::Ready(false);
		}
		match &payload {
			Some(payload) => group.poll_ready(waiter, &info(payload)).map(|()| true),
			None => Poll::Pending,
		}
	})
	.await;
	room && payload.is_some_and(|payload| {
		group
			.write_frame(moq_net::Timestamp::unscaled(group.start), payload)
			.is_ok()
	})
}

/// The origin's handler: serves each fill from the frame it starts at, paced by its riders,
/// counting the frames it writes and the fills it ends. With `finish`, the archive holds the
/// whole group, so each fill finishes after its last frame.
fn serve(requests: track::Requests, written: Arc<AtomicU64>, ended: Arc<AtomicU64>, finish: bool) {
	drop(kio::clock::sim::spawn(async move {
		while let Ok(fill) = requests.requested_fill().await {
			let start = fill.start();
			let mut group = fill.accept(start.group, None).expect("accept");
			let mut demand = group.demand();
			let (written, ended) = (written.clone(), ended.clone());
			drop(kio::clock::sim::spawn(async move {
				for index in start.frame as u32.. {
					let payload = (index < FRAMES).then(|| numbered(index));
					if payload.is_none() && finish {
						group.close().expect("finish");
						break;
					}
					if !write_paced(&mut group, &fill, &mut demand, payload).await {
						break;
					}
					written.fetch_add(1, Ordering::Relaxed);
				}
				ended.fetch_add(1, Ordering::Relaxed);
			}));
		}
	}));
}

/// Read the next frame of `group`, failing on anything else.
async fn next(group: &mut group::Consumer, version: &str, index: u32) -> u32 {
	let frame = kio::clock::timeout(TIMEOUT, group.read_frame())
		.await
		.unwrap_or_else(|_| panic!("{version}: frame {index} never arrived"))
		.unwrap_or_else(|err| panic!("{version}: frame {index}: {err}"))
		.unwrap_or_else(|| panic!("{version}: the group ended before frame {index}"));
	number(&frame)
}

/// Whether `group` ends cleanly at its next read.
async fn next_end(group: &mut group::Consumer, version: &str) -> bool {
	kio::clock::timeout(TIMEOUT, group.read_frame())
		.await
		.unwrap_or_else(|_| panic!("{version}: the group never ended"))
		.unwrap_or_else(|err| panic!("{version}: the end: {err}"))
		.is_none()
}

#[kio::test]
async fn a_late_joiner_replays_through_a_relay_chain() {
	for version in VERSIONS {
		chain(version, Replay::Live).await;
	}
}

/// A moq-transport hop refuses a FETCH reaching past the live edge of a live group, as a
/// flight's whole-group FETCH does, before reading any of it: the late joiner is refused
/// rather than replayed, and no flight reaches the origin.
#[kio::test]
async fn moq_transport_refuses_a_replay_of_a_live_group() {
	chain("moq-transport-22", Replay::Refused).await;
}

/// A moq-transport hop answers a FETCH of a finished group before reading it, then streams
/// its objects, so a group far past one tail replays through each hop's flight.
#[kio::test]
async fn moq_transport_replays_a_finished_group() {
	chain("moq-transport-22", Replay::Finished).await;
}

/// A moq-transport relay holding none of a finished group, far past its ingest window,
/// answers a FETCH of it once its own copy finishes arriving, which nothing reads until
/// FETCH_OK, then streams it, riding a flight for the front its copy shed.
#[kio::test]
async fn moq_transport_relays_a_cold_finished_group() {
	let version = "moq-transport-22";
	let version_parsed: Version = version.parse().unwrap();
	let pools: [cache::Pool; 3] = Default::default();
	let publisher = origin(1, &pools[0]);
	let upper = origin(2, &pools[1]);
	let lower = origin(3, &pools[2]);

	let route = publisher.route("live", origin::RouteOptions::hidden()).unwrap();
	let track = route.broadcast().create_track("data", track::Info::default()).unwrap();
	route.announce().unwrap();
	let (written, ended) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
	serve(track.requests(), written, ended, true);
	let mut group = track.append_next().unwrap();
	for index in 0..FRAMES {
		group
			.write_frame(moq_net::Timestamp::unscaled(group.start), numbered(index))
			.unwrap();
	}
	group.close().unwrap();

	let mut options = MockConnectOptions::new(version_parsed);
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(upper.clone());
	let _upper_link = connect_mock(options).await;
	let mut options = MockConnectOptions::new(version_parsed);
	options.server_publish = Some(upper.consume());
	options.client_subscribe = Some(lower.clone());
	let _lower_link = connect_mock(options).await;

	let remote = kio::clock::timeout(TIMEOUT, lower.consume().follow("live", None).unwrap().available())
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves")
		.expect("an instance");
	let mut fetched = kio::clock::timeout(TIMEOUT, remote.track("data").unwrap().fetch_group(0, None))
		.await
		.expect("fetch timeout")
		.expect("fetch");
	for index in 0..FRAMES {
		assert_eq!(next(&mut fetched, version, index).await, index, "{version}: fetched");
	}
	assert!(
		next_end(&mut fetched, version).await,
		"{version}: the fetch ends with the group"
	);
}

/// What a late joiner of group 0 finds at the bottom of the chain.
#[derive(Clone, Copy, PartialEq)]
enum Replay {
	/// The live group, replayed from frame 0 until the joiner hands over to the live copy.
	Live,
	/// The finished group, replayed from frame 0 to its end.
	Finished,
	/// A refusal.
	Refused,
}

/// Join group 0 late at the bottom of a two-relay chain, expecting `replay`.
async fn chain(version: &str, replay: Replay) {
	let version_parsed: Version = version.parse().unwrap();
	let pools: [cache::Pool; 3] = Default::default();
	let publisher = origin(1, &pools[0]);
	let upper = origin(2, &pools[1]);
	let lower = origin(3, &pools[2]);

	let route = publisher.route("live", origin::RouteOptions::hidden()).unwrap();
	let track = route.broadcast().create_track("data", track::Info::default()).unwrap();
	route.announce().unwrap();
	let (written, ended) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
	serve(
		track.requests(),
		written.clone(),
		ended.clone(),
		replay == Replay::Finished,
	);

	let mut options = MockConnectOptions::new(version_parsed);
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(upper.clone());
	let _upper_link = connect_mock(options).await;
	let mut options = MockConnectOptions::new(version_parsed);
	options.server_publish = Some(upper.consume());
	options.client_subscribe = Some(lower.clone());
	let _lower_link = connect_mock(options).await;

	let remote = kio::clock::timeout(TIMEOUT, lower.consume().follow("live", None).unwrap().available())
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves")
		.expect("an instance");
	let subscription = || {
		track::Subscription::default()
			.with_ranges([crate::support::keys(0..)])
			.with_delay(track::Delay::default().with_max(Duration::from_secs(3600)))
	};
	let mut live_sub = kio::clock::timeout(TIMEOUT, remote.track("data").unwrap().subscribe(subscription()))
		.await
		.expect("subscribe timeout")
		.expect("subscribe");

	// The live reader at the bottom of the chain keeps up with the publisher.
	let mut live = track.append_next().unwrap();
	live.write_frame(moq_net::Timestamp::unscaled(live.start), numbered(0))
		.unwrap();
	let mut reader = kio::clock::timeout(TIMEOUT, live_sub.recv_group())
		.await
		.expect("group timeout")
		.unwrap()
		.expect("group 0");
	assert_eq!(next(&mut reader, version, 0).await, 0);
	for index in 1..FRAMES {
		kio::wait(|waiter| live.poll_ready(waiter, &info(&numbered(index)))).await;
		live.write_frame(moq_net::Timestamp::unscaled(live.start), numbered(index))
			.unwrap();
		assert_eq!(next(&mut reader, version, index).await, index, "{version}: live");
	}

	if replay == Replay::Finished {
		live.close().unwrap();
		assert!(
			next_end(&mut reader, version).await,
			"{version}: the live reader sees the end"
		);
	}

	// A late joiner at the bottom: every copy in the chain shed frame 0.
	let mut late_sub = kio::clock::timeout(TIMEOUT, remote.track("data").unwrap().subscribe(subscription()))
		.await
		.expect("subscribe timeout")
		.expect("subscribe");
	let mut late = kio::clock::timeout(TIMEOUT, late_sub.recv_group())
		.await
		.expect("group timeout")
		.unwrap()
		.expect("group 0");
	if replay == Replay::Refused {
		let refused = kio::clock::timeout(TIMEOUT, late.read_frame())
			.await
			.unwrap_or_else(|_| panic!("{version}: never answered"));
		assert!(refused.is_err(), "{version}: {refused:?}");
		kio::clock::sleep(Duration::from_secs(1)).await;
		assert_eq!(written.load(Ordering::Relaxed), 0, "{version}: no flight at the origin");
		return;
	}
	let mut held = 0;
	for index in 0..FRAMES {
		assert_eq!(next(&mut late, version, index).await, index, "{version}: replayed");
		held = pools.iter().map(cache::Pool::used).max().unwrap().max(held);
	}

	if replay == Replay::Finished {
		assert!(
			next_end(&mut late, version).await,
			"{version}: the replay ends with the group"
		);
	}

	// The joiner left its flight for the live copy before the flights fetched the whole
	// group, and every flight in the chain stopped. A finished group is short enough that
	// its flight, a window ahead of the handover, reaches its end.
	let fetched = written.load(Ordering::Relaxed);
	assert!(
		replay == Replay::Finished || fetched < u64::from(FRAMES),
		"{version}: the origin wrote {fetched} frames"
	);
	kio::clock::sleep(Duration::from_secs(1)).await;
	assert_eq!(ended.load(Ordering::Relaxed), 1, "{version}: one flight, stopped");
	// A live copy and one flight per hop, each held to its 32 MiB tail.
	assert!(held < 80 << 20, "{version}: a hop held {} MiB", held >> 20);
}
