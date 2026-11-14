#![cfg(feature = "async")]

//! A relay copy lingers after its last reader leaves and resumes on the next subscribe.
//! A route answering behind that copy's cache fails the subscription rather than leave it
//! waiting, and the next subscribe builds a fresh copy.

use crate::support;

use std::time::Duration;

use moq_net::{Error, Version, origin, track};
use support::harness::{MockConnectOptions, connect_mock, peer};

const TIMEOUT: Duration = Duration::from_secs(5);

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(std::num::NonZeroU64::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer.with_proxy(true)
}

fn write_group(track: &track::Producer, payload: &'static [u8]) {
	let mut group = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
	group.write_frame(moq_net::Timestamp::unscaled(group.start), payload).unwrap();
	group.close().unwrap();
}

/// The next group `subscriber` receives, with its first frame.
async fn next_group(subscriber: &mut track::Subscriber) -> moq_net::Result<(u64, Vec<u8>)> {
	let mut group = kio::clock::timeout(TIMEOUT, subscriber.recv_group())
		.await
		.expect("the subscription waits forever")?
		.expect("the subscription ended");
	let frame = kio::clock::timeout(TIMEOUT, group.read_frame())
		.await
		.unwrap()?
		.unwrap();
	Ok((group.start, frame.payload.to_vec()))
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
		assert_eq!(next_group(&mut subscriber).await.unwrap(), (0, b"old".to_vec()));
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
		Ok(mut subscriber) => next_group(&mut subscriber).await.map(|_| ()),
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
		next_group(&mut subscriber).await.unwrap(),
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

/// A relay whose upstream reset its newest group while it stayed live still holds the
/// groups before it, so its moq-transport answer names the last of them as Largest. A
/// downstream copy that lingered over the same groups is current with that answer and
/// resumes rather than failing.
async fn a_reset_newest_group_answers_the_group_before(version: &str) {
	let version: Version = version.parse().unwrap();
	let publisher = produce_origin(1);
	let upper = produce_origin(2);
	let lower = produce_origin(3);
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(upper.clone());
	let _upper = connect_mock(options).await;
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(upper.consume());
	options.client_subscribe = Some(lower.clone());
	let _lower = connect_mock(options).await;

	let route = publisher.route("room", origin::RouteOptions::hidden()).unwrap();
	let track = route
		.broadcast()
		.create_track("audio", track::Info::default().with_timescale(None))
		.unwrap();
	route.announce().unwrap();
	let mut g0 = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
	g0.write_frame(moq_net::Timestamp::unscaled(g0.start), &b"0a"[..]).unwrap();

	// A reader on the upper relay keeps its copy live after the lower relay leaves.
	let local = kio::clock::timeout(TIMEOUT, upper.consume().follow("room", None).unwrap().available())
		.await
		.unwrap()
		.unwrap()
		.unwrap();
	let mut keeper = kio::clock::timeout(TIMEOUT, local.track("audio").unwrap().subscribe(None))
		.await
		.unwrap()
		.unwrap();
	assert_eq!(next_group(&mut keeper).await.unwrap(), (0, b"0a".to_vec()));

	let remote = kio::clock::timeout(TIMEOUT, lower.consume().follow("room", None).unwrap().available())
		.await
		.unwrap()
		.unwrap()
		.unwrap();
	{
		let mut subscriber = kio::clock::timeout(TIMEOUT, remote.track("audio").unwrap().subscribe(None))
			.await
			.unwrap()
			.unwrap();
		assert_eq!(next_group(&mut subscriber).await.unwrap(), (0, b"0a".to_vec()));
		g0.close().unwrap();
		let mut g1 = track.create_group(moq_net::group::Info { start: 1 }).unwrap();
		g1.write_frame(moq_net::Timestamp::unscaled(g1.start), &b"1a"[..]).unwrap();
		assert_eq!(next_group(&mut subscriber).await.unwrap(), (1, b"1a".to_vec()));
		let mut kept = kio::clock::timeout(TIMEOUT, keeper.recv_group())
			.await
			.unwrap()
			.unwrap()
			.unwrap();
		assert_eq!(kept.start, 1);
		g1.abort(Error::Cancel).unwrap();
		// The upper relay's copy stays live with group 1 reset and group 0 held.
		loop {
			match kio::clock::timeout(TIMEOUT, kept.read_frame()).await.unwrap() {
				Ok(Some(_)) => {}
				Ok(None) => panic!("group 1 finished rather than reset"),
				Err(_) => break,
			}
		}
	}
	// On the simulated clock: the lower relay cancels its upstream subscription and its copy
	// lingers over group 0.
	kio::clock::sleep(Duration::from_secs(1)).await;

	let mut subscriber = kio::clock::timeout(TIMEOUT, remote.track("audio").unwrap().subscribe(None))
		.await
		.expect("the resubscribe waits forever")
		.unwrap();
	// The upper relay answers the resubscription before the feed moves on.
	kio::clock::sleep(Duration::from_secs(1)).await;
	let mut g2 = track.create_group(moq_net::group::Info { start: 2 }).unwrap();
	g2.write_frame(moq_net::Timestamp::unscaled(g2.start), &b"2a"[..]).unwrap();
	loop {
		let (key, payload) = next_group(&mut subscriber).await.unwrap();
		if key == 2 {
			assert_eq!(payload, b"2a".to_vec(), "{version}");
			break;
		}
	}
}

#[kio::test]
async fn a_reset_newest_group_answers_the_group_before_ietf18() {
	a_reset_newest_group_answers_the_group_before("moq-transport-18").await;
}

#[kio::test]
async fn a_reset_newest_group_answers_the_group_before_ietf22() {
	a_reset_newest_group_answers_the_group_before("moq-transport-22").await;
}
