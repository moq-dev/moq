#![cfg(feature = "async")]

//! Initial catalog acquisition while the same publisher gains a second session.

use crate::support;

use moq_net::{Error, Hop, Version, origin};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

fn origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

async fn link(version: Version, from: &origin::Producer, to: &origin::Producer, cost: u64) -> MockPair {
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(from.consume());
	options.client_subscribe = Some(to.clone());
	options.cost = Some(cost);
	connect_mock(options).await
}

/// `carries_epoch`: the wire carries the epoch, as lite-07 and moq-transport with the MoQ
/// Epoch extension do.
async fn acquisition(version: &str, carries_epoch: bool) {
	let version: Version = version.parse().unwrap();
	let publisher = origin(1);
	let subscriber = origin(2);
	let broadcast = publisher.create_broadcast("live").unwrap();
	let mut dynamic = broadcast.requests();
	broadcast
		.announce(origin::Route::default().with_epoch(moq_net::Epoch::mint()))
		.unwrap();
	let _old = link(version, &publisher, &subscriber, 100).await;
	let remote = subscriber
		.consume()
		.follow("live", None)
		.unwrap()
		.available()
		.await
		.unwrap();
	let mut announced = subscriber.consume().announced();
	let old_cost = match announced.next().await {
		Some(moq_net::announce::Event::Start(announce)) => announce.route.cost,
		other => panic!("expected the first route, got {other:?}"),
	};
	let mut pending = Box::pin(remote.track("catalog.json").unwrap().subscribe(None));
	let request = match futures::future::select(Box::pin(dynamic.requested_track()), &mut pending).await {
		futures::future::Either::Left((request, _)) => request.unwrap(),
		_ => panic!("catalog resolved before the publisher answered"),
	};
	// The publisher is still available and the old session remains alive. Holding
	// TRACK_INFO makes the replacement happen during acquisition without sleeps.
	let _new = link(version, &publisher, &subscriber, 1).await;
	// An epochless replacement restarts the advertisement; a matching epoch only updates it.
	let new_cost = match announced.next().await {
		Some(moq_net::announce::Event::Update(announce)) if carries_epoch => announce.route.cost,
		Some(moq_net::announce::Event::Restart(announce)) if !carries_epoch => announce.route.cost,
		other => panic!("{version}: unexpected route change {other:?}"),
	};
	assert!(new_cost < old_cost, "the cheaper session wins the route");
	let track = request.accept(None);
	let mut group = track.append_group().unwrap();
	group.write_frame(None, b"catalog".to_vec()).unwrap();
	group.finish().unwrap();
	let result = pending.await;
	if carries_epoch {
		let mut subscription = result.expect("same epoch must acquire");
		let mut group = subscription.recv_group().await.unwrap().unwrap();
		assert_eq!(group.read_frame().await.unwrap().unwrap().payload.as_ref(), b"catalog");
	} else {
		assert!(
			matches!(result, Err(Error::Unroutable)),
			"epochless replacement: {:?}",
			result.err()
		);
	}
}

#[kio::test]
async fn epochless_replacement_refuses_pending_catalog() {
	acquisition("moq-lite-06", false).await;
}

#[kio::test]
async fn same_epoch_replacement_acquires_pending_catalog() {
	acquisition("moq-lite-07-wip", true).await;
	acquisition("moq-transport-18", true).await;
	acquisition("moq-transport-22", true).await;
}
