#![cfg(feature = "async")]

//! A relay copy lingers after its last reader leaves and resumes on the next subscribe.
//! A route answering behind that copy's cache fails the subscription rather than leave it
//! waiting, and the next subscribe builds a fresh copy (#7973).

use crate::support;

use std::time::Duration;

use moq_net::{Error, Hop, Version, origin, track};
use support::harness::peer;

const TIMEOUT: Duration = Duration::from_secs(5);

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

fn write_group(track: &track::Producer, payload: &'static [u8]) {
	let mut group = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
	group.write_frame(None, payload).unwrap();
	group.finish().unwrap();
}

/// The first group `subscriber` receives, with its first frame.
async fn first_group(subscriber: &mut track::Subscriber) -> moq_net::Result<(u64, Vec<u8>)> {
	let mut group = kio::clock::timeout(TIMEOUT, subscriber.recv_group())
		.await
		.expect("the subscription waits forever")?
		.expect("the subscription ended");
	let frame = kio::clock::timeout(TIMEOUT, group.read_frame())
		.await
		.unwrap()?
		.unwrap();
	Ok((group.sequence, frame.payload.to_vec()))
}

/// The publisher replaces its track with an empty one of the same name, so the lingering
/// copy's resubscription is answered with no Largest while the copy holds group 0. A lite
/// publisher holds its answer to that resubscription until the group exists, so only
/// moq-transport answers this way.
async fn an_empty_answer_fails_the_lingering_copy(version: &str) {
	let version: Version = version.parse().unwrap();
	let publisher = produce_origin(1);
	let relay = produce_origin(2);
	let _pair = peer(version, &publisher, &relay).await;

	let route = publisher.route("room", origin::RouteOptions::hidden()).unwrap();
	let info = track::Info::default().with_timescale(None);
	let old = route.broadcast().create_track("catalog", info.clone()).unwrap();
	route.announce().unwrap();
	write_group(&old, b"old");

	let consumer = relay.consume().request("room", None).await.unwrap();
	{
		let mut subscriber = kio::clock::timeout(TIMEOUT, consumer.track("catalog").unwrap().subscribe(None))
			.await
			.unwrap()
			.unwrap();
		assert_eq!(first_group(&mut subscriber).await.unwrap(), (0, b"old".to_vec()));
	}
	// The relay cancels its idle upstream subscription; the copy lingers.
	kio::clock::timeout(TIMEOUT, support::unwanted(old.demand()))
		.await
		.unwrap()
		.unwrap();

	old.abort(Error::Cancel).unwrap();
	let new = route.broadcast().create_track("catalog", info).unwrap();

	let resumed = kio::clock::timeout(TIMEOUT, consumer.track("catalog").unwrap().subscribe(None))
		.await
		.expect("the resubscribe waits forever");
	let failed = match resumed {
		Ok(mut subscriber) => first_group(&mut subscriber).await.map(|_| ()),
		Err(err) => Err(err),
	};
	assert!(matches!(failed, Err(Error::MalformedTrack)), "{version}: {failed:?}");
	// The failed copy leaves once the relay drops its upstream subscription.
	kio::clock::timeout(TIMEOUT, support::unwanted(new.demand()))
		.await
		.unwrap()
		.unwrap();

	write_group(&new, b"new");
	let mut subscriber = kio::clock::timeout(TIMEOUT, consumer.track("catalog").unwrap().subscribe(None))
		.await
		.unwrap()
		.unwrap();
	assert_eq!(
		first_group(&mut subscriber).await.unwrap(),
		(0, b"new".to_vec()),
		"{version}: a fresh copy accepts the feed from group 0"
	);
}

#[kio::test]
async fn an_empty_answer_fails_the_lingering_copy_ietf18() {
	an_empty_answer_fails_the_lingering_copy("moq-transport-18").await;
}

#[kio::test]
async fn an_empty_answer_fails_the_lingering_copy_ietf22() {
	an_empty_answer_fails_the_lingering_copy("moq-transport-22").await;
}
