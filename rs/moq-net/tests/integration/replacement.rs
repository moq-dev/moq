#![cfg(feature = "async")]

//! A replacement session, end to end over real sessions: a second session from the
//! same peer re-announces every route while the first one is still open, as a GOAWAY
//! drain does.
//!
//! Without an epoch the re-announced routes are new instances, so announce consumers
//! get a `Restart`. A handle resolved before the replacement keeps serving new requests
//! until that `Restart` is deliverable, so its holder can follow the `Restart` instead
//! of failing `Unroutable`. With an epoch the replacement continues the instance: no
//! `Restart`, and the old handle keeps working.

use crate::support;

use std::time::Duration;

use moq_net::{Epoch, Error, Hop, Timestamp, Version, announce, broadcast, origin, track};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

const TEST_TIMEOUT: Duration = Duration::from_secs(60);

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

/// Have `to` pull everything `from` publishes, over a new session.
async fn link(version: Version, from: &origin::Producer, to: &origin::Producer) -> MockPair {
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(from.consume());
	options.client_subscribe = Some(to.clone());
	connect_mock(options).await
}

/// The next announce event, failing on a hang.
async fn next_event(announced: &mut announce::Consumer) -> announce::Event {
	kio::clock::timeout(Duration::from_secs(10), announced.next())
		.await
		.expect("announce hung")
		.expect("announce ended")
}

/// Write one group of one frame tagged `tag`.
fn write(track: &track::Producer, tag: &str) {
	let mut group = track.append_group().unwrap();
	group.write_frame(Timestamp::ZERO, tag.as_bytes().to_vec()).unwrap();
	group.finish().unwrap();
}

/// The next frame a subscription delivers, as text.
async fn read(subscription: &mut track::Subscriber) -> String {
	let mut group = kio::clock::timeout(Duration::from_secs(10), subscription.recv_group())
		.await
		.expect("subscription hung")
		.unwrap()
		.expect("subscription ended");
	let frame = group.read_frame().await.unwrap().expect("a frame");
	String::from_utf8(frame.payload.to_vec()).unwrap()
}

async fn subscribe(broadcast: &broadcast::Consumer, name: &str) -> Result<track::Subscriber, Error> {
	kio::clock::timeout(Duration::from_secs(10), broadcast.track(name)?.subscribe(None))
		.await
		.expect("subscribe hung")
}

/// Shorter than the default update hold, so the replacement's `Restart` is still held.
const WITHIN_HOLD: Duration = Duration::from_millis(10);

/// Longer than the default update hold.
const PAST_HOLD: Duration = Duration::from_millis(500);

async fn replace(version: &str, epoch: Option<Epoch>) {
	let version: Version = version.parse().unwrap();
	let publisher = produce_origin(1);
	let viewer = produce_origin(3);

	let mut route = origin::Route::default();
	route.epoch = epoch.clone();
	let cam = publisher.publish("cam", route).unwrap();
	let video = cam.create_track("video", timed()).unwrap();
	let audio = cam.create_track("audio", timed()).unwrap();
	let meta = cam.create_track("meta", timed()).unwrap();

	let _first = link(version, &publisher, &viewer).await;
	let mut announced = viewer.consume().announced();
	match next_event(&mut announced).await {
		announce::Event::Start(announce) => assert_eq!(announce.prefix.as_str(), "cam"),
		other => panic!("{version}: expected the broadcast, got {other:?}"),
	}

	let old = viewer.consume().request("cam", None).await.unwrap();
	let mut sticky = subscribe(&old, "video").await.unwrap();
	write(&video, "video:0");
	assert_eq!(read(&mut sticky).await, "video:0", "{version}");

	// The replacement: the same peer re-announces everything over a second session,
	// while the first one is still open.
	let _second = link(version, &publisher, &viewer).await;
	kio::clock::sleep(WITHIN_HOLD).await;

	// A request through the old handle right after the replacement is still served.
	let mut late = subscribe(&old, "audio")
		.await
		.unwrap_or_else(|err| panic!("{version}: the old handle refused a request before any restart: {err}"));
	write(&audio, "audio:0");
	assert_eq!(read(&mut late).await, "audio:0", "{version}");

	if epoch.is_some() {
		// The epoch continues across the replacement: nothing to restart, and the old
		// handle keeps serving.
		if let Ok(event) = kio::clock::timeout(PAST_HOLD, announced.next()).await {
			panic!("{version}: an epoch route restarted on replacement: {event:?}");
		}
		let mut later = subscribe(&old, "meta").await.unwrap();
		write(&meta, "meta:0");
		assert_eq!(read(&mut later).await, "meta:0", "{version}");
		return;
	}

	// Without an epoch the replacement is a restart: once the hold releases it, the old
	// handle refuses new requests, and the restart is already there to follow.
	kio::clock::sleep(PAST_HOLD).await;
	let Err(err) = subscribe(&old, "meta").await else {
		panic!("{version}: a retired handle served a request");
	};
	assert!(matches!(err, Error::Unroutable), "{version}: {err}");
	match announced.try_next() {
		Some(announce::Event::Restart(announce)) => assert_eq!(announce.prefix.as_str(), "cam"),
		other => panic!("{version}: retired before the restart was deliverable, got {other:?}"),
	}

	// Following the restart resolves the path afresh, and the new handle serves.
	let fresh = viewer.consume().request("cam", None).await.unwrap();
	assert!(!fresh.is_clone(&old), "{version}: joined the retired broadcast");
	let mut followed = subscribe(&fresh, "meta").await.unwrap();
	write(&meta, "meta:0");
	assert_eq!(read(&mut followed).await, "meta:0", "{version}");
}

/// The epoch a replaced route carries on both sessions.
fn epoch() -> Epoch {
	"01900000-0000-7000-8000-000000000001".parse().unwrap()
}

#[kio::test]
async fn epochless_lite06() {
	kio::clock::timeout(TEST_TIMEOUT, replace("moq-lite-06", None))
		.await
		.expect("timed out");
}

#[kio::test]
async fn epochless_lite07() {
	kio::clock::timeout(TEST_TIMEOUT, replace("moq-lite-07-wip", None))
		.await
		.expect("timed out");
}

/// Only lite-07 carries the epoch.
#[kio::test]
async fn epoch_lite07() {
	kio::clock::timeout(TEST_TIMEOUT, replace("moq-lite-07-wip", Some(epoch())))
		.await
		.expect("timed out");
}

/// moq-transport carries the epoch with the MoQ Epoch extension.
#[kio::test]
async fn epoch_ietf() {
	for version in ["moq-transport-18", "moq-transport-22"] {
		kio::clock::timeout(TEST_TIMEOUT, replace(version, Some(epoch())))
			.await
			.expect("timed out");
	}
}

/// Without one, a moq-transport replacement restarts as it does over lite.
#[kio::test]
async fn epochless_ietf() {
	for version in ["moq-transport-22"] {
		kio::clock::timeout(TEST_TIMEOUT, replace(version, None))
			.await
			.expect("timed out");
	}
}

/// A route change alone is never fatal, but a dead route is: once an epochless replacement
/// is announced, a subscription on the old instance keeps going while the old publisher
/// serves it, and its session closing ends that subscription with an error, never cleanly.
/// The viewer reads either straight from the publisher or through a relay, and the old
/// session closes after `wait`: inside the update hold, before the `Restart` is delivered,
/// or past it.
async fn route_death(version: &str, relayed: bool, wait: Duration) {
	let version: Version = version.parse().unwrap();
	let publisher = produce_origin(1);
	let relay = produce_origin(2);
	let viewer = produce_origin(3);

	let cam = publisher.publish("cam", origin::Route::default()).unwrap();
	let video = cam.create_track("video", timed()).unwrap();

	let upstream = if relayed { &relay } else { &viewer };
	let old_session = link(version, &publisher, upstream).await;
	let _downstream = match relayed {
		true => Some(link(version, &relay, &viewer).await),
		false => None,
	};
	let mut announced = viewer.consume().announced();
	next_event(&mut announced).await;

	let old = viewer.consume().request("cam", None).await.unwrap();
	let mut sticky = subscribe(&old, "video").await.unwrap();
	write(&video, "video:0");
	assert_eq!(read(&mut sticky).await, "video:0", "{version}");

	let _replacement = link(version, &publisher, upstream).await;
	kio::clock::sleep(wait).await;
	write(&video, "video:1");
	assert_eq!(
		read(&mut sticky).await,
		"video:1",
		"{version}: the replacement cut the old instance off while it served"
	);

	let _ = old_session.server.close().await;
	let end = kio::clock::timeout(Duration::from_secs(10), async {
		loop {
			match sticky.recv_group().await {
				Ok(Some(_)) => continue,
				Ok(None) => return None,
				Err(err) => return Some(err),
			}
		}
	})
	.await
	.expect("the dead route's subscription hung");
	assert!(
		end.is_some(),
		"{version} (relayed: {relayed}): the dead route's subscription ended cleanly"
	);
}

#[kio::test]
async fn route_death_within_hold_errors() {
	for version in ["moq-lite-06", "moq-lite-07-wip"] {
		for relayed in [false, true] {
			kio::clock::timeout(TEST_TIMEOUT, route_death(version, relayed, WITHIN_HOLD))
				.await
				.expect("timed out");
		}
	}
}

#[kio::test]
async fn route_death_past_hold_errors() {
	for version in ["moq-lite-06", "moq-lite-07-wip"] {
		for relayed in [false, true] {
			kio::clock::timeout(TEST_TIMEOUT, route_death(version, relayed, PAST_HOLD))
				.await
				.expect("timed out");
		}
	}
}

/// A publisher restarts as a new process (a new hop) while its old session lingers,
/// dead but not yet idled out. The restart wins at once, whichever chain hashes lower,
/// so a viewer's first request reaches it instead of waiting on the dead session.
async fn restart_while_the_old_session_lingers(version: &str, old_hop: u64, new_hop: u64) {
	let version: Version = version.parse().unwrap();
	let viewer = produce_origin(3);

	let old = produce_origin(old_hop);
	let old_cam = old.publish("cam", origin::Route::default()).unwrap();
	let _old_video = old_cam.create_track("video", timed()).unwrap();
	let old_session = link(version, &old, &viewer).await;
	let mut announced = viewer.consume().announced();
	next_event(&mut announced).await;

	// The old process dies without a close: its session falls silent.
	old_session.client_transport.set_latency(Duration::from_secs(3600));
	old_session.server_transport.set_latency(Duration::from_secs(3600));

	let new = produce_origin(new_hop);
	let new_cam = new.publish("cam", origin::Route::default()).unwrap();
	let new_video = new_cam.create_track("video", timed()).unwrap();
	let _new_session = link(version, &new, &viewer).await;
	kio::clock::sleep(PAST_HOLD).await;

	let cam = viewer.consume().request("cam", None).await.unwrap();
	let mut video = subscribe(&cam, "video")
		.await
		.unwrap_or_else(|err| panic!("{version} ({old_hop} -> {new_hop}): {err}"));
	write(&new_video, "new:0");
	assert_eq!(read(&mut video).await, "new:0", "{version} ({old_hop} -> {new_hop})");
}

#[kio::test]
async fn restart_while_the_old_session_lingers_wins() {
	for version in ["moq-lite-06", "moq-lite-07-wip", "moq-transport-22"] {
		for (old_hop, new_hop) in [(1, 4), (4, 1)] {
			kio::clock::timeout(
				TEST_TIMEOUT,
				restart_while_the_old_session_lingers(version, old_hop, new_hop),
			)
			.await
			.expect("timed out");
		}
	}
}
