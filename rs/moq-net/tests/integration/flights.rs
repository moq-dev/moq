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
use moq_net::{Hop, Version, cache, frame, group, origin, track};

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

fn info(payload: &Bytes) -> frame::Info {
	frame::Info {
		size: payload.len() as u64,
		timestamp: None,
	}
}

fn origin(hop: u64, pool: &cache::Pool) -> origin::Producer {
	let mut config = origin::Config::new(Hop::new(hop).unwrap());
	config.pool = pool.clone();
	let (origin, driver) = origin::Producer::new(config);
	spawn(driver);
	origin
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
	let sequence = group.sequence;
	let room = kio::wait(|waiter| {
		while let Poll::Ready(Ok(_)) = demand.poll_wanted_changed(waiter) {}
		if fill.poll_unwanted(sequence, waiter).is_ready() && !demand.wanted() {
			return Poll::Ready(false);
		}
		match &payload {
			Some(payload) => group.poll_ready(waiter, &info(payload)).map(|()| true),
			None => Poll::Pending,
		}
	})
	.await;
	room && payload.is_some_and(|payload| group.write_frame(None, payload).is_ok())
}

/// The origin's handler: serves each fill from the frame it starts at, paced by its riders,
/// counting the frames it writes and the fills it ends.
fn serve(requests: track::Requests, written: Arc<AtomicU64>, ended: Arc<AtomicU64>) {
	drop(kio::clock::sim::spawn(async move {
		while let Ok(fill) = requests.requested_fill().await {
			let start = fill.start();
			let mut group = fill.accept(start.group, None).expect("accept");
			let mut demand = group.demand();
			let (written, ended) = (written.clone(), ended.clone());
			drop(kio::clock::sim::spawn(async move {
				for index in start.frame as u32.. {
					let payload = (index < FRAMES).then(|| numbered(index));
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

#[kio::test]
async fn a_late_joiner_replays_through_a_relay_chain() {
	for version in VERSIONS {
		chain(version, true).await;
	}
}

/// A moq-transport hop holds a FETCH answer whole until FETCH_OK, so never more than one
/// tail of it: the late joiner is refused rather than replayed, and every flight stops.
#[kio::test]
async fn moq_transport_refuses_a_replay_past_one_tail() {
	chain("moq-transport-22", false).await;
}

/// Join group 0 late at the bottom of a two-relay chain, expecting it replayed from frame 0
/// when `replays`, else refused.
async fn chain(version: &str, replays: bool) {
	let version_parsed: Version = version.parse().unwrap();
	let pools: [cache::Pool; 3] = Default::default();
	let publisher = origin(1, &pools[0]);
	let upper = origin(2, &pools[1]);
	let lower = origin(3, &pools[2]);

	let broadcast = publisher.create_broadcast("live").unwrap();
	let track = broadcast.create_track("data", track::Info::default()).unwrap();
	broadcast.announce(Default::default()).unwrap();
	let (written, ended) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
	serve(track.requests(), written.clone(), ended.clone());

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
			.with_range(0..)
			.with_delay(track::Delay::default().with_max(Duration::from_secs(3600)))
	};
	let mut live_sub = kio::clock::timeout(TIMEOUT, remote.track("data").unwrap().subscribe(subscription()))
		.await
		.expect("subscribe timeout")
		.expect("subscribe");

	// The live reader at the bottom of the chain keeps up with the publisher.
	let mut live = track.append_group().unwrap();
	live.write_frame(None, numbered(0)).unwrap();
	let mut reader = kio::clock::timeout(TIMEOUT, live_sub.recv_group())
		.await
		.expect("group timeout")
		.unwrap()
		.expect("group 0");
	assert_eq!(next(&mut reader, version, 0).await, 0);
	for index in 1..FRAMES {
		kio::wait(|waiter| live.poll_ready(waiter, &info(&numbered(index)))).await;
		live.write_frame(None, numbered(index)).unwrap();
		assert_eq!(next(&mut reader, version, index).await, index, "{version}: live");
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
	if !replays {
		let refused = kio::clock::timeout(TIMEOUT, late.read_frame())
			.await
			.unwrap_or_else(|_| panic!("{version}: never answered"));
		assert!(refused.is_err(), "{version}: {refused:?}");
		kio::clock::sleep(Duration::from_secs(1)).await;
		assert_eq!(ended.load(Ordering::Relaxed), 1, "{version}: one flight, stopped");
		return;
	}
	let mut held = 0;
	for index in 0..FRAMES {
		assert_eq!(next(&mut late, version, index).await, index, "{version}: replayed");
		held = pools.iter().map(cache::Pool::used).max().unwrap().max(held);
	}

	// The joiner left its flight for the live copy before the flights fetched the whole
	// group, and every flight in the chain stopped.
	let fetched = written.load(Ordering::Relaxed);
	assert!(
		fetched < u64::from(FRAMES),
		"{version}: the origin wrote {fetched} frames"
	);
	kio::clock::sleep(Duration::from_secs(1)).await;
	assert_eq!(ended.load(Ordering::Relaxed), 1, "{version}: one flight, stopped");
	// A live copy and one flight per hop, each held to its 32 MiB tail.
	assert!(held < 80 << 20, "{version}: a hop held {} MiB", held >> 20);
}
