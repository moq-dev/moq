//! A snapshot reader on a received track never waits on a hole the subscription still owes.
use moq_json::snapshot;
use moq_net::{fuzz, group, kio, track};
use serde_json::{Value, json};
use std::{task::Poll, time::Duration};

/// Group 2 arrives while group 1 is still owed: `latest()` and `next()` return it at once,
/// whatever budget the caller subscribed with or later restores through a retained handle,
/// since a snapshot consumer always reads at zero.
#[test]
fn reads_skip_an_owed_hole() {
	for max in [Some(Duration::ZERO), Some(Duration::from_secs(1)), None] {
		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("state", track::Info::default())
			.unwrap();
		fuzz::received(&track);
		let subscription = track::Subscription::default().with_delay(track::Delay::default().with_max(max));
		let reader = || {
			let subscriber = track.subscribe(subscription.clone());
			let control = subscriber.control();
			let consumer = snapshot::Consumer::<Value>::new(subscriber, Default::default());
			control.update(subscription.clone()).unwrap();
			consumer
		};
		let mut latest = reader();
		let mut next = reader();
		let waiter = kio::Waiter::noop();
		let put = |sequence: u64| {
			let mut group = track.create_group(group::Info { sequence }).unwrap();
			group
				.write_frame(None, serde_json::to_vec(&json!({ "n": sequence })).unwrap())
				.unwrap();
			group.finish().unwrap();
		};

		put(0);
		let Poll::Ready(Ok(Some(state))) = latest.poll_latest(&waiter) else {
			panic!("latest missed group 0 with max {max:?}")
		};
		assert_eq!(state.value, json!({ "n": 0 }));
		let Poll::Ready(Ok(Some(state))) = next.poll_next(&waiter) else {
			panic!("next missed group 0 with max {max:?}")
		};
		assert_eq!(state.value, json!({ "n": 0 }));

		put(2);
		let Poll::Ready(Ok(Some(state))) = latest.poll_latest(&waiter) else {
			panic!("latest waited on owed group 1 with max {max:?}")
		};
		assert_eq!(state.value, json!({ "n": 2 }));
		let Poll::Ready(Ok(Some(state))) = next.poll_next(&waiter) else {
			panic!("next waited on owed group 1 with max {max:?}")
		};
		assert_eq!(state.value, json!({ "n": 2 }));

		// The owed group landing late is superseded, not delivered.
		put(1);
		assert!(latest.poll_latest(&waiter).is_pending(), "max {max:?}");
		assert!(next.poll_next(&waiter).is_pending(), "max {max:?}");
	}
}
