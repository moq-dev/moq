//! Refresh joins traverse real protocol hops, judged independently from each cache.
use crate::support::harness::{MockConnectOptions, connect_mock, peer, spawn, subscribed};
use futures::FutureExt;
use moq_net::{Version, origin, track};
use std::time::Duration;

fn origin(hop: u64) -> origin::Producer {
	let (origin, driver) = origin::Producer::new(origin::Config::new(std::num::NonZeroU64::new(hop).unwrap()));
	spawn(driver);
	origin.with_proxy(true)
}

/// The JSON owner answers demand across a publisher link, a cluster peer, and a reader link.
/// Existing readers keep their deltas; the late reader receives the cached group, then a new
/// complete snapshot.
#[kio::test]
async fn json_snapshot_refresh_crosses_a_two_relay_cluster() {
	use moq_json::snapshot;
	use moq_net::{Timescale, Timestamp};
	use serde_json::{Value, json};

	for ratio in [1, 2] {
		let version = "moq-lite-07-wip".parse().unwrap();
		let source = origin(1);
		let a = origin(2);
		let b = origin(3);
		let reader = origin(4);
		let broadcast = source.route("room", origin::RouteOptions::hidden()).unwrap();
		let track = broadcast
			.broadcast()
			.create_track(
				"state",
				track::Info::default()
					.with_refresh(moq_net::track::Refresh::default().with_ratio(ratio))
					.with_timescale(Timescale::MILLI),
			)
			.unwrap();
		let mut producer = snapshot::Producer::<Value>::new(track.clone(), Default::default());
		broadcast.announce().unwrap();
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
			.unwrap()
			.unwrap();
		let remote = remote.track("state").unwrap();
		let mut baseline = remote.subscribe(None).await.unwrap();
		assert_eq!(
			baseline.info().refresh,
			moq_net::track::Refresh::default().with_ratio(ratio)
		);
		subscribed(&track).await;

		let at = |n| Timestamp::from_millis(100 + n).unwrap();
		// A group starts at its first frame's time, in the track's milliseconds.
		let key = |n| 100 + n;
		let value = |n| json!({ "n": n, "p": "" });
		let snapshot_bytes = producer.update(&value(0), at(0)).unwrap().unwrap();
		let mut original = baseline.recv_group().await.unwrap().unwrap();
		assert_eq!(original.start, key(0));
		let first = original.read_frame().await.unwrap().unwrap();
		assert_eq!(first.timestamp, at(0));
		assert_eq!(first.payload.len(), snapshot_bytes);
		assert_eq!(serde_json::from_slice::<Value>(&first.payload).unwrap(), value(0));

		let mut delta_bytes = 0;
		let mut joins = Vec::new();
		for n in 1..=2 * u64::from(ratio) {
			delta_bytes += producer.update(&value(n), at(n)).unwrap().unwrap();
			let delta = original.read_frame().await.unwrap().unwrap();
			assert_eq!(delta.timestamp, at(n));
			assert_eq!(
				serde_json::from_slice::<Value>(&delta.payload).unwrap(),
				json!({ "n": n })
			);
			if n == 1 || n == 2 * u64::from(ratio) {
				let mut join = snapshot::Consumer::<Value>::new(remote.clone(), Default::default());
				let received = join.latest().await.unwrap().unwrap();
				assert_eq!((received.value, received.at), (value(n), at(n)));
				assert!(producer.poll_refresh(&kio::Waiter::noop()).is_pending());
				joins.push(join);
			}
		}
		assert_eq!(
			delta_bytes,
			ratio as usize * snapshot_bytes,
			"equality does not refresh"
		);

		let n = 2 * u64::from(ratio) + 1;
		delta_bytes += producer.update(&value(n), at(n)).unwrap().unwrap();
		original.read_frame().await.unwrap().unwrap();
		assert!(delta_bytes > ratio as usize * snapshot_bytes);
		// Simulated time passing cannot roll a group, even with deltas over the ratio.
		kio::clock::sleep(Duration::from_secs(60)).await;
		assert!(producer.poll_refresh(&kio::Waiter::noop()).is_pending());

		let mut late = remote.subscribe(None).await.unwrap();
		// Poll the new reader while the owner drives refresh: a front does not join its
		// serving copy until read. Refresh is a request, not a hold, so the cached group
		// arrives first, whole, and the fresh one follows.
		let (mut cached, mut fresh) = kio::clock::timeout(Duration::from_secs(1), async {
			let (refreshed, groups) = futures::join!(producer.refresh(), async {
				let cached = late.recv_group().await.unwrap().unwrap();
				(cached, late.recv_group().await.unwrap().unwrap())
			});
			refreshed.unwrap();
			groups
		})
		.await
		.unwrap();
		assert_eq!(cached.start, key(0), "the cached group is not withheld");
		let mut cached_bytes = 0;
		for _ in 0..=n {
			cached_bytes += cached.read_frame().await.unwrap().unwrap().payload.len();
		}
		assert_eq!(cached_bytes, snapshot_bytes + delta_bytes);
		assert!(
			producer.poll_refresh(&kio::Waiter::noop()).is_pending(),
			"answered once"
		);
		assert_eq!(fresh.start, key(n), "new content gets a new group identity");
		let frame = fresh.read_frame().await.unwrap().unwrap();
		assert_eq!(frame.timestamp, at(n), "refresh preserves the newest source time");
		assert_eq!(serde_json::from_slice::<Value>(&frame.payload).unwrap(), value(n));
		assert_eq!(frame.payload.len(), snapshot_bytes);
		assert!(fresh.poll_read_frame(&kio::Waiter::noop()).is_pending());
		assert_eq!(baseline.recv_group().await.unwrap().unwrap().start, key(n));

		producer.update(&value(n + 1), at(n + 1)).unwrap();
		let delta = fresh.read_frame().await.unwrap().unwrap();
		assert_eq!(delta.timestamp, at(n + 1));
		assert_eq!(
			serde_json::from_slice::<Value>(&delta.payload).unwrap(),
			json!({ "n": n + 1 })
		);
		let mut current = snapshot::Consumer::<Value>::new(remote.clone(), Default::default());
		let received = current.latest().await.unwrap().unwrap();
		assert_eq!((received.value, received.at), (value(n + 1), at(n + 1)));
		assert!(producer.poll_refresh(&kio::Waiter::noop()).is_pending());
		let _finished = producer.finish().unwrap();
		drop((current, fresh, late, joins, original, baseline, links));
	}
}

#[kio::test]
async fn ratio_joins_cross_two_relays() {
	for name in ["moq-lite-06", "moq-lite-07-wip"] {
		let version: Version = name.parse().unwrap();
		let source = origin(1);
		let a = origin(2);
		let b = origin(3);
		let broadcast = source.route("room", origin::RouteOptions::hidden()).unwrap();
		let producer = broadcast
			.broadcast()
			.create_track(
				"data",
				track::Info::default().with_refresh(moq_net::track::Refresh::default().with_ratio(1)),
			)
			.unwrap();
		broadcast.announce().unwrap();
		let mut links = Vec::new();
		for (upstream, downstream) in [(&source, &a), (&a, &b)] {
			let mut options = MockConnectOptions::new(version);
			options.server_publish = Some(upstream.consume());
			options.client_subscribe = Some(downstream.clone());
			links.push(connect_mock(options).await);
		}
		let remote = b
			.consume()
			.follow("room", None)
			.unwrap()
			.available()
			.await
			.unwrap()
			.unwrap();
		let remote = remote.track("data").unwrap();
		let mut baseline = remote.subscribe(None).await.unwrap();
		assert_eq!(
			baseline.info().refresh,
			moq_net::track::Refresh::default().with_ratio((name == "moq-lite-07-wip").then_some(1))
		);
		crate::support::harness::subscribed(&producer).await;
		let mut group = producer.append_group(moq_net::Timestamp::unscaled(0)).unwrap();
		group.write_frame(moq_net::Timestamp::unscaled(0), &b"key"[..]).unwrap();
		group.write_frame(moq_net::Timestamp::unscaled(0), &b"abc"[..]).unwrap();
		let mut received = baseline.recv_group().await.unwrap().unwrap();
		assert_eq!(received.read_frame().await.unwrap().unwrap().payload.len(), 3);
		assert_eq!(received.read_frame().await.unwrap().unwrap().payload.len(), 3);
		let mut under = remote.subscribe(None).await.unwrap();
		let _under_group = under.recv_group().await.unwrap().unwrap();
		assert!(
			producer.demand().poll_refresh(&kio::Waiter::noop()).is_pending(),
			"ratio equality does not refresh"
		);
		group.write_frame(moq_net::Timestamp::unscaled(0), &b"d"[..]).unwrap();
		assert_eq!(received.read_frame().await.unwrap().unwrap().payload.len(), 1);
		let mut joins = Vec::new();
		for _ in 0..8 {
			let mut join = remote.subscribe(None).await.unwrap();
			let _group = join.recv_group().await.unwrap().unwrap();
			joins.push(join);
		}
		if name == "moq-lite-07-wip" {
			let demand = producer.demand();
			kio::clock::timeout(Duration::from_secs(1), demand.refresh())
				.await
				.unwrap()
				.unwrap();
		} else {
			// Drain runnable protocol tasks on simulated time, without a wall-clock sleep.
			kio::clock::sleep(Duration::from_millis(1)).await;
			assert!(producer.demand().poll_refresh(&kio::Waiter::noop()).is_pending());
		}
		drop((joins, under, received, baseline, group, links));
	}
}

/// A one-frame track behind two relays makes a group when a reader asks, and never more
/// often than its interval however many readers ask, each hop judging by its own clock.
#[kio::test]
async fn interval_caps_refreshes_across_two_relays() {
	use moq_net::Timestamp;

	let version: Version = "moq-lite-07-wip".parse().unwrap();
	let interval = Duration::from_secs(5);
	let source = origin(1);
	let a = origin(2);
	let b = origin(3);
	let broadcast = source.route("room", origin::RouteOptions::hidden()).unwrap();
	let refresh = track::Refresh::default().with_interval(interval);
	let producer = broadcast
		.broadcast()
		.create_track("thumbnail", track::Info::default().with_refresh(refresh))
		.unwrap();
	broadcast.announce().unwrap();
	let mut links = Vec::new();
	for (upstream, downstream) in [(&source, &a), (&a, &b)] {
		let mut options = MockConnectOptions::new(version);
		options.server_publish = Some(upstream.consume());
		options.client_subscribe = Some(downstream.clone());
		links.push(connect_mock(options).await);
	}
	let remote = b
		.consume()
		.follow("room", None)
		.unwrap()
		.available()
		.await
		.unwrap()
		.unwrap();
	let remote = remote.track("thumbnail").unwrap();
	let mut readers = Vec::new();
	for _ in 0..8 {
		readers.push(remote.subscribe(None).await.unwrap());
	}
	assert_eq!(readers[0].info().refresh, refresh);

	// The publisher makes a one-frame group each time a reader asks.
	let demand = producer.demand();
	let made = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
	let log = made.clone();
	let publishing = producer.clone();
	let _publisher = kio::clock::sim::spawn(async move {
		let mut n = 0u64;
		while demand.refresh().await.is_ok() {
			let start = Timestamp::unscaled(n);
			let mut group = publishing.append_group(start).unwrap();
			group.write_frame(start, &b"still"[..]).unwrap();
			group.finish().unwrap();
			log.borrow_mut().push(kio::clock::now());
			n += 1;
		}
	});

	let first = readers[0].recv_group().await.unwrap().unwrap();
	assert_eq!(first.start, 0, "the first reader asks for the first group at once");

	// Every reader asks every 100ms for 30 seconds.
	let began = kio::clock::now();
	while kio::clock::now() < began.checked_add(Duration::from_secs(30)).unwrap() {
		for reader in &readers {
			reader.refresh();
		}
		kio::clock::sleep(Duration::from_millis(100)).await;
	}
	let made = made.borrow().clone();
	assert!(made.len() >= 6, "each interval answers the readers: {made:?}");
	for pair in made.windows(2) {
		assert!(
			pair[1].saturating_duration_since(pair[0]) >= interval,
			"a group made within the interval: {made:?}"
		);
	}
	let newest = made.len() as u64 - 1;
	let mut last = None;
	while let Some(Ok(Some(group))) = readers[7].recv_group().now_or_never() {
		last = Some(group.start);
	}
	assert_eq!(last, Some(newest), "the readers receive the fresh groups");
	drop((readers, first, links, producer));
}
