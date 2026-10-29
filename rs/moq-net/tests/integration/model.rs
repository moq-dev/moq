//! The explicit-time model works without async helpers or a runtime.

use moq_net::{Error, Hop, Timestamp, broadcast, frame, group, kio, origin, track};
use std::task::Poll;

fn ready<T>(value: Poll<T>) -> T {
	match value {
		Poll::Ready(value) => value,
		Poll::Pending => panic!("model operation unexpectedly pending"),
	}
}

#[test]
fn origin_front_serves_media_with_only_explicit_polls() {
	let now = moq_net::time::Instant::now();
	let waiter = kio::Waiter::noop();
	let (origin, mut driver) = origin::Producer::new(origin::Config::new(Hop::new(1).unwrap()));
	driver.poll(now, &waiter).unwrap();
	let broadcast = origin.create_broadcast("room/live").unwrap();
	let track = broadcast
		.create_track(
			"video",
			track::Info::default().with_timescale(moq_net::Timescale::MILLI),
		)
		.unwrap();
	let mut group = track.create_group(group::Info { sequence: 7 }).unwrap();
	group.write_frame(Timestamp::ZERO, b"media").unwrap();
	group.finish().unwrap();
	broadcast.announce(origin::Route::default()).unwrap();

	// Resolving and subscribing pass through the origin's core front tasks.
	let mut requesting = origin.consume().request("room/live", None);
	assert!(requesting.poll_ok(&waiter).is_pending());
	driver.poll(now, &waiter).unwrap();
	let receiving: broadcast::Consumer = ready(requesting.poll_ok(&waiter)).unwrap();
	let subscribing = receiving.track("video").unwrap().subscribe(None);
	driver.poll(now, &waiter).unwrap();
	let mut subscriber = ready(subscribing.poll_ok(&waiter)).unwrap();
	let mut group = ready(subscriber.poll_recv_group(&waiter)).unwrap().unwrap();
	assert_eq!(group.sequence, 7);
	let media: frame::Frame = ready(group.poll_read_frame(&waiter)).unwrap().unwrap();
	assert_eq!(media.payload.as_ref(), b"media");
	assert!(ready(group.poll_read_frame(&waiter)).unwrap().is_none());

	// Driver teardown also works without any executor-owned tasks.
	drop(driver);
	assert!(matches!(origin.create_broadcast("after-close"), Err(Error::Closed)));
}

#[test]
fn follow_resolves_with_only_explicit_polls() {
	let now = moq_net::time::Instant::now();
	let waiter = kio::Waiter::noop();
	let (origin, mut driver) = origin::Producer::new(origin::Config::new(Hop::new(1).unwrap()));
	driver.poll(now, &waiter).unwrap();

	// Nothing covers the path yet: the follower waits for an announcement.
	let mut following = origin.consume().follow("room/live", None).unwrap();
	assert!(following.poll_available(&waiter).is_pending());
	let broadcast = origin.create_broadcast("room/live").unwrap();
	assert!(following.poll_available(&waiter).is_pending());

	// Announcing starts the path, so the next poll asks and the driver serves it.
	broadcast.announce(origin::Route::default()).unwrap();
	assert!(following.poll_available(&waiter).is_pending());
	driver.poll(now, &waiter).unwrap();
	let resolved = ready(following.poll_available(&waiter)).unwrap();
	assert_eq!(resolved.info().path.as_str(), "room/live");

	// A closed origin ends a parked follower.
	let mut following = origin.consume().follow("room/other", None).unwrap();
	assert!(following.poll_available(&waiter).is_pending());
	drop(driver);
	assert!(matches!(ready(following.poll_available(&waiter)), Err(Error::Closed)));
}
