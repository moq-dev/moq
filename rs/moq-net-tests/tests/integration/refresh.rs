//! Refresh joins traverse real protocol hops, judged independently from each cache.
use crate::support::harness::{MockConnectOptions, connect_mock, peer, spawn, subscribed};
use moq_net::{Hop, Version, kio, origin, track};
use std::time::Duration;

fn origin(hop: u64) -> origin::Producer {
	let (origin, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	spawn(driver);
	origin
}

/// The JSON owner answers demand across a publisher link, a cluster peer, and a reader link.
/// Existing readers keep their deltas; the late reader receives the cached group, then a new
/// complete snapshot.
#[moq_clock::test]
async fn json_snapshot_refresh_crosses_a_two_relay_cluster() {
	use moq_json::snapshot;
	use moq_net::{Timed, Timescale, Timestamp};
	use serde_json::{Value, json};

	for ratio in [1, 2] {
		let version = "moq-lite-07-wip".parse().unwrap();
		let source = origin(1);
		let a = origin(2);
		let b = origin(3);
		let reader = origin(4);
		let broadcast = source.create_broadcast("room").unwrap();
		let track = broadcast
			.create_track(
				"state",
				track::Info::default()
					.with_refresh(ratio)
					.with_timescale(Timescale::MILLI),
			)
			.unwrap();
		let mut producer = snapshot::Producer::<Value>::new(track.clone(), Default::default());
		broadcast.announce(Default::default()).unwrap();
		let mut links = vec![peer(version, &a, &b).await];
		for (upstream, downstream) in [(&source, &a), (&b, &reader)] {
			let mut options = MockConnectOptions::new(version);
			options.server_publish = Some(upstream.consume());
			options.client_subscribe = Some(downstream.clone());
			links.push(connect_mock(options).await);
		}
		let remote = reader
			.consume()
			.follow("room", None)
			.unwrap()
			.available()
			.await
			.unwrap();
		let remote = remote.track("state").unwrap();
		let mut baseline = remote.subscribe(None).await.unwrap();
		assert_eq!(baseline.info().refresh, Some(ratio));
		subscribed(&track).await;

		let at = |n| Timestamp::from_millis(100 + n).unwrap();
		let value = |n| json!({ "n": n, "p": "" });
		let snapshot_bytes = producer.update(Timed::from(&value(0)).at(at(0))).unwrap().unwrap();
		let mut original = baseline.recv_group().await.unwrap().unwrap();
		assert_eq!(original.sequence, 0);
		let first = original.read_frame().await.unwrap().unwrap();
		assert_eq!(first.timestamp, Some(at(0)));
		assert_eq!(first.payload.len(), snapshot_bytes);
		assert_eq!(serde_json::from_slice::<Value>(&first.payload).unwrap(), value(0));

		let mut delta_bytes = 0;
		let mut joins = Vec::new();
		for n in 1..=2 * u64::from(ratio) {
			delta_bytes += producer.update(Timed::from(&value(n)).at(at(n))).unwrap().unwrap();
			let delta = original.read_frame().await.unwrap().unwrap();
			assert_eq!(delta.timestamp, Some(at(n)));
			assert_eq!(
				serde_json::from_slice::<Value>(&delta.payload).unwrap(),
				json!({ "n": n })
			);
			if n == 1 || n == 2 * u64::from(ratio) {
				let mut join =
					snapshot::Consumer::<Value>::new(remote.subscribe(None).await.unwrap(), Default::default());
				let received = join.latest().await.unwrap().unwrap();
				assert_eq!((received.value, received.at), (value(n), Some(at(n))));
				assert!(producer.poll_refresh(&kio::Waiter::noop()).is_pending());
				assert_eq!(track.demand().wanted(), Some(0));
				joins.push(join);
			}
		}
		assert_eq!(
			delta_bytes,
			ratio as usize * snapshot_bytes,
			"equality does not refresh"
		);

		let n = 2 * u64::from(ratio) + 1;
		delta_bytes += producer.update(Timed::from(&value(n)).at(at(n))).unwrap().unwrap();
		original.read_frame().await.unwrap().unwrap();
		assert!(delta_bytes > ratio as usize * snapshot_bytes);
		// Simulated time passing cannot roll a group, even with deltas over the ratio.
		moq_clock::sleep(Duration::from_secs(60)).await;
		assert!(producer.poll_refresh(&kio::Waiter::noop()).is_pending());
		assert_eq!(track.demand().wanted(), Some(0));

		let mut late = remote.subscribe(None).await.unwrap();
		// Poll the new reader while the owner drives refresh: a front does not join its
		// serving copy until read. Refresh is a request, not a hold, so the cached group
		// arrives first, whole, and the fresh one follows.
		let (mut cached, mut fresh) = moq_clock::timeout(Duration::from_secs(1), async {
			let (refreshed, groups) = futures::join!(producer.refresh(), async {
				let cached = late.recv_group().await.unwrap().unwrap();
				(cached, late.recv_group().await.unwrap().unwrap())
			});
			refreshed.unwrap();
			groups
		})
		.await
		.unwrap();
		assert_eq!(cached.sequence, 0, "the cached group is not withheld");
		let mut cached_bytes = 0;
		for _ in 0..=n {
			cached_bytes += cached.read_frame().await.unwrap().unwrap().payload.len();
		}
		assert_eq!(cached_bytes, snapshot_bytes + delta_bytes);
		assert!(
			producer.poll_refresh(&kio::Waiter::noop()).is_pending(),
			"answered once"
		);
		assert_eq!(fresh.sequence, 1, "new content gets a new group identity");
		let frame = fresh.read_frame().await.unwrap().unwrap();
		assert_eq!(frame.timestamp, Some(at(n)), "refresh preserves the newest source time");
		assert_eq!(serde_json::from_slice::<Value>(&frame.payload).unwrap(), value(n));
		assert_eq!(frame.payload.len(), snapshot_bytes);
		assert!(fresh.poll_read_frame(&kio::Waiter::noop()).is_pending());
		assert_eq!(baseline.recv_group().await.unwrap().unwrap().sequence, 1);

		producer.update(Timed::from(&value(n + 1)).at(at(n + 1))).unwrap();
		let delta = fresh.read_frame().await.unwrap().unwrap();
		assert_eq!(delta.timestamp, Some(at(n + 1)));
		assert_eq!(
			serde_json::from_slice::<Value>(&delta.payload).unwrap(),
			json!({ "n": n + 1 })
		);
		let mut current = snapshot::Consumer::<Value>::new(remote.subscribe(None).await.unwrap(), Default::default());
		let received = current.latest().await.unwrap().unwrap();
		assert_eq!((received.value, received.at), (value(n + 1), Some(at(n + 1))));
		assert!(producer.poll_refresh(&kio::Waiter::noop()).is_pending());
		producer.finish().unwrap();
		drop((current, fresh, late, joins, original, baseline, links));
	}
}

#[moq_clock::test]
async fn ratio_joins_cross_two_relays() {
	for name in ["moq-lite-06", "moq-lite-07-wip"] {
		let version: Version = name.parse().unwrap();
		let source = origin(1);
		let a = origin(2);
		let b = origin(3);
		let broadcast = source.create_broadcast("room").unwrap();
		let producer = broadcast
			.create_track("data", track::Info::default().with_refresh(1))
			.unwrap();
		broadcast.announce(Default::default()).unwrap();
		let mut links = Vec::new();
		for (upstream, downstream) in [(&source, &a), (&a, &b)] {
			let mut options = MockConnectOptions::new(version);
			options.server_publish = Some(upstream.consume());
			options.client_subscribe = Some(downstream.clone());
			links.push(connect_mock(options).await);
		}
		let remote = b.consume().follow("room", None).unwrap().available().await.unwrap();
		let remote = remote.track("data").unwrap();
		let mut baseline = remote.subscribe(None).await.unwrap();
		assert_eq!(baseline.info().refresh, (name == "moq-lite-07-wip").then_some(1));
		crate::support::harness::subscribed(&producer).await;
		let mut group = producer.append_group().unwrap();
		group.write_frame(None, &b"key"[..]).unwrap();
		group.write_frame(None, &b"abc"[..]).unwrap();
		let mut received = baseline.recv_group().await.unwrap().unwrap();
		assert_eq!(received.read_frame().await.unwrap().unwrap().payload.len(), 3);
		assert_eq!(received.read_frame().await.unwrap().unwrap().payload.len(), 3);
		let mut under = remote.subscribe(None).await.unwrap();
		let _under_group = under.recv_group().await.unwrap().unwrap();
		assert_eq!(producer.demand().wanted(), Some(0), "ratio equality does not refresh");
		group.write_frame(None, &b"d"[..]).unwrap();
		assert_eq!(received.read_frame().await.unwrap().unwrap().payload.len(), 1);
		let mut joins = Vec::new();
		for _ in 0..8 {
			let mut join = remote.subscribe(None).await.unwrap();
			let _group = join.recv_group().await.unwrap().unwrap();
			joins.push(join);
		}
		if name == "moq-lite-07-wip" {
			let mut demand = producer.demand();
			moq_clock::timeout(Duration::from_secs(1), async {
				while demand.wanted_changed().await.unwrap() != Some(1) {}
			})
			.await
			.unwrap();
		} else {
			// Drain runnable protocol tasks on simulated time, without a wall-clock sleep.
			moq_clock::sleep(Duration::from_millis(1)).await;
			assert_eq!(producer.demand().wanted(), Some(0));
		}
		drop((joins, under, received, baseline, group, links));
	}
}
