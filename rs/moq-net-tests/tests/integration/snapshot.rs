//! A snapshot reader on a received track never waits on a hole the subscription still owes.
use moq_json::snapshot;
use moq_net::{fuzz, group, kio, track};
use serde_json::{Value, json};
use std::{task::Poll, time::Duration};

/// Group 2 arrives while group 1 is still owed: `latest()` returns it at once, whatever
/// budget the caller subscribed with, since a snapshot consumer always reads at zero.
#[test]
fn latest_skips_an_owed_hole() {
	for max in [Some(Duration::ZERO), Some(Duration::from_secs(1)), None] {
		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("state", track::Info::default())
			.unwrap();
		fuzz::received(&track);
		let subscription = track::Subscription::default().with_delay(track::Delay::default().with_max(max));
		let mut consumer = snapshot::Consumer::<Value>::new(track.subscribe(subscription), Default::default());
		let waiter = kio::Waiter::noop();
		let put = |sequence: u64| {
			let mut group = track.create_group(group::Info { sequence }).unwrap();
			group
				.write_frame(None, serde_json::to_vec(&json!({ "n": sequence })).unwrap())
				.unwrap();
			group.finish().unwrap();
		};

		put(0);
		let Poll::Ready(Ok(Some(state))) = consumer.poll_latest(&waiter) else {
			panic!("missing group 0 with max {max:?}")
		};
		assert_eq!(state.value, json!({ "n": 0 }));

		put(2);
		let Poll::Ready(Ok(Some(state))) = consumer.poll_latest(&waiter) else {
			panic!("group 2 waited on owed group 1 with max {max:?}")
		};
		assert_eq!(state.value, json!({ "n": 2 }));

		// The owed group landing late is superseded, not delivered.
		put(1);
		assert!(consumer.poll_next(&waiter).is_pending(), "max {max:?}");
	}
}
