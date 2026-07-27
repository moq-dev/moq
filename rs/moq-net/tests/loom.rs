//! Loom model checks for the model layer (broadcast -> track -> group -> frame).
//!
//! The unit tests drive these handles from one task at a time, so they can only
//! catch logic bugs. In production a publisher writes on one thread while a
//! session driver reads on another, and the handoff is kio's `Arc<Mutex<State>>`
//! plus a waker list. These tests hand each side to its own loom thread and let
//! loom permute the interleavings.
//!
//! A lost wakeup shows up as loom reporting a deadlock (every thread parked, none
//! runnable), so "the test terminates" is itself the assertion. Anything else is
//! asserted directly.
//!
//! Run with `just rs loom`; the whole file compiles away without `cfg(loom)`.
#![cfg(loom)]

use bytes::Bytes;
use loom::{future::block_on, thread};
use moq_net::{Timestamp, broadcast, cache, origin};

/// A frame written on the publisher thread must reach a subscriber parked on
/// `next_frame`, however the write interleaves with the reader's parking.
#[test]
fn frame_reaches_a_parked_subscriber() {
	loom::model(|| {
		let mut broadcast = broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let mut track = broadcast.create_track("video", None).expect("create track");
		let track_consumer = consumer.track("video").expect("track");

		let publisher = thread::spawn(move || {
			let mut group = track.append_group().expect("append group");
			group
				.write_frame(Timestamp::ZERO, Bytes::from_static(b"frame"))
				.expect("write frame");
			group.finish().expect("finish group");
			track.finish().expect("finish track");
		});

		let mut subscriber = block_on(track_consumer.subscribe(None)).expect("subscribe");
		let mut group = block_on(subscriber.recv_group())
			.expect("recv group")
			.expect("a group was published");
		let frame = block_on(group.read_frame()).expect("read frame").expect("a frame");
		assert_eq!(frame.payload, Bytes::from_static(b"frame"));

		publisher.join().unwrap();
	});
}

/// Two groups written back to back must both reach the subscriber in order. This is
/// the path where the producer takes the waiter list for group 1 and wakes outside
/// the lock while group 2 is already being written.
#[test]
fn back_to_back_groups_arrive_in_order() {
	loom::model(|| {
		let mut broadcast = broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let mut track = broadcast.create_track("video", None).expect("create track");
		let track_consumer = consumer.track("video").expect("track");

		let publisher = thread::spawn(move || {
			for _ in 0..2 {
				let mut group = track.append_group().expect("append group");
				group.finish().expect("finish group");
			}
			track.finish().expect("finish track");
		});

		let mut subscriber = block_on(track_consumer.subscribe(None)).expect("subscribe");
		let first = block_on(subscriber.recv_group()).expect("recv").expect("first group");
		let second = block_on(subscriber.recv_group()).expect("recv").expect("second group");
		assert!(second.sequence > first.sequence, "groups arrived out of order");

		publisher.join().unwrap();
	});
}

/// A publisher parked on `Demand::used` drives on-demand capture, so a subscriber
/// appearing on another thread must always wake it.
#[test]
fn subscriber_wakes_parked_demand() {
	loom::model(|| {
		let mut broadcast = broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let track = broadcast.create_track("video", None).expect("create track");
		let demand = track.demand();

		let subscriber = thread::spawn(move || {
			let track_consumer = consumer.track("video").expect("track");
			// Held until joined, so it outlives the `used()` poll.
			block_on(track_consumer.subscribe(None)).expect("subscribe")
		});

		block_on(demand.used()).expect("the new subscriber was missed");

		drop(subscriber.join().unwrap());
		drop(track);
	});
}

/// Two tracks share one bounded pool, each written from its own thread, so their
/// charges and eviction debt interleave. However that lands, the pool must be back
/// to zero once every handle is gone: a charge that outlives its group is how a
/// cache leaks.
#[test]
fn concurrent_tracks_drain_a_shared_pool() {
	loom::model(|| {
		let pool = cache::Pool::new(512);
		let mut info = broadcast::Info::new();
		info.origin = origin::Info::default().with_pool(pool.clone());
		let mut broadcast = info.produce();

		let handles: Vec<_> = ["video", "audio"]
			.into_iter()
			.map(|name| {
				let mut track = broadcast.create_track(name, None).expect("create track");
				thread::spawn(move || {
					let mut group = track.append_group().expect("append group");
					group
						.write_frame(Timestamp::ZERO, Bytes::from_static(b"0123456789"))
						.expect("write frame");
					group.finish().expect("finish group");
					track.finish().expect("finish track");
				})
			})
			.collect();

		for handle in handles {
			handle.join().unwrap();
		}
		broadcast.finish();
		drop(broadcast);

		assert_eq!(pool.used(), 0, "the pool kept a charge after every group was dropped");
	});
}

/// Dropping the publisher must resolve a subscriber parked on `recv_group` rather
/// than leaving it waiting for a group that will never come.
#[test]
fn publisher_drop_resolves_a_parked_subscriber() {
	loom::model(|| {
		let mut broadcast = broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let track = broadcast.create_track("video", None).expect("create track");
		let track_consumer = consumer.track("video").expect("track");

		let publisher = thread::spawn(move || drop(track));

		let mut subscriber = block_on(track_consumer.subscribe(None)).expect("subscribe");
		// Ok(None) on a clean finish, Err on an abort; either resolves the park.
		let _ = block_on(subscriber.recv_group());

		publisher.join().unwrap();
	});
}
