#![cfg(feature = "async")]

//! Refresh joins traverse two real protocol hops, judged independently from each cache.
use crate::support::harness::{MockConnectOptions, connect_mock, spawn};
use moq_net::{Hop, Version, origin, track};
use std::time::Duration;

fn origin(hop: u64) -> origin::Producer {
	let (origin, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	spawn(driver);
	origin
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
		b.consume().routed("room").await.unwrap();
		let remote = b.consume().request_broadcast("room", None).await.unwrap();
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
