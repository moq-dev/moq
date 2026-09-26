//! A relay spreading one prefix across a pool of workers, seen from downstream.
//!
//! Workers claim the same prefix, so the pool relay advertises one route for all
//! of them, labelled by whichever member ranks first for the prefix. Which member
//! serves a path is the relay's per-path choice, so the label cannot say what a
//! downstream relay is receiving; the TRACK_INFO reply does. When the serving
//! worker dies, the pool relay re-serves the path from another worker, and the
//! downstream relay must end its subscription rather than splice that worker's
//! frames onto the first one's.

mod support;

use std::time::Duration;

use moq_net::{Hop, Timestamp, Version, origin};
use support::harness::{MockConnectOptions, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(10);

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	tokio::spawn(support::harness::run(driver));
	producer
}

/// A worker claiming the "pool" prefix: every request is answered with a fresh
/// broadcast whose "video" track keeps producing groups carrying the worker's name.
fn worker(hop: u64) -> origin::Producer {
	let producer = produce_origin(hop);
	let dynamic = producer.dynamic("pool", origin::Route::default()).unwrap();
	let name = format!("w{hop}").into_bytes();
	tokio::spawn(async move {
		while let Ok(request) = dynamic.requested_broadcast().await {
			let broadcast = moq_net::broadcast::Info::new().produce();
			let track = broadcast.create_track("video", None).unwrap();
			request.accept(&broadcast);
			let name = name.clone();
			tokio::spawn(async move {
				let _broadcast = broadcast;
				loop {
					let Ok(mut group) = track.append_group() else { return };
					group.write_frame(Timestamp::ZERO, name.clone()).unwrap();
					group.finish().unwrap();
					tokio::time::sleep(Duration::from_millis(5)).await;
				}
			});
		}
	});
	producer
}

#[tokio::test]
async fn downstream_failover_never_splices_another_pool_member() {
	tokio::time::timeout(TIMEOUT, async {
		let version: Version = "moq-lite-07-wip".parse().unwrap();
		let pool = produce_origin(10);
		let downstream = produce_origin(30);

		// Each worker publishes its claim to the pool relay.
		let mut members = Vec::new();
		for hop in [20, 21] {
			let producer = worker(hop);
			let mut options = MockConnectOptions::new(version);
			options.client_publish = Some(producer.consume());
			options.server_subscribe = Some(pool.clone());
			members.push((hop, producer, connect_mock(options).await));
		}

		// The downstream relay reaches the pool through one session.
		let mut options = MockConnectOptions::new(version);
		options.server_publish = Some(pool.consume());
		options.client_subscribe = Some(downstream.clone());
		let _link = connect_mock(options).await;

		let consumer = downstream.consume();
		consumer.routed("pool/job").await.unwrap();
		let remote = consumer.request_broadcast("pool/job").await.unwrap();
		let mut subscription = remote.track("video").unwrap().subscribe(None).await.unwrap();

		let mut group = subscription.recv_group().await.unwrap().expect("a first group");
		let first = group.read_frame().await.unwrap().expect("a frame").payload.to_vec();

		// Kill whichever worker the pool relay picked for this path.
		let serving = members
			.iter()
			.position(|(hop, ..)| first == format!("w{hop}").into_bytes())
			.expect("a pool member served the path");
		let (_, _, pair) = members.remove(serving);
		pair.client.abort(moq_net::Error::Cancel);

		// Every group this subscription delivers is the first worker's; it ends
		// instead of carrying on with the survivor's.
		while let Ok(Some(mut group)) = subscription.recv_group().await {
			while let Some(frame) = group.read_frame().await.unwrap_or(None) {
				assert_eq!(frame.payload.to_vec(), first, "spliced another pool member's frames");
			}
		}

		// A fresh request is served by the survivor.
		let (hop, ..) = &members[0];
		let survivor = format!("w{hop}").into_bytes();
		let remote = consumer.request_broadcast("pool/job").await.unwrap();
		let mut subscription = remote.track("video").unwrap().subscribe(None).await.unwrap();
		let mut group = subscription.recv_group().await.unwrap().expect("a group");
		let frame = group.read_frame().await.unwrap().expect("a frame");
		assert_eq!(frame.payload.to_vec(), survivor);
	})
	.await
	.expect("timed out");
}
