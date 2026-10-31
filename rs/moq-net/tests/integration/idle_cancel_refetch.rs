#![cfg(feature = "async")]

//! A relay whose last reader leaves mid-group cancels the idle upstream subscription. A later
//! fetch of that group still gets every frame (#5348, a release-line regression).

use crate::support;

use std::time::Duration;

use moq_net::{Hop, Version};
use support::harness::{MockConnectOptions, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(5);

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

#[moq_clock::test]
async fn a_group_cut_by_an_idle_cancel_is_refetched_whole() {
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let track = broadcast
		.create_track("audio", moq_net::track::Info::default().with_timescale(None))
		.unwrap();
	broadcast.announce(Default::default()).unwrap();

	let mut g0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
	g0.write_frame(None, &b"0a"[..]).unwrap();
	g0.finish().unwrap();
	let mut g1 = track.create_group(moq_net::group::Info { sequence: 1 }).unwrap();
	g1.write_frame(None, &b"1a"[..]).unwrap();

	let client = produce_origin(2);
	let mut options = MockConnectOptions::new("moq-lite-06".parse::<Version>().unwrap());
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(client.clone());
	let _pair = connect_mock(options).await;

	let consumer = client.consume();
	let remote = moq_clock::timeout(TIMEOUT, consumer.follow("bcast", None).unwrap().available())
		.await
		.unwrap()
		.unwrap();

	{
		let mut subscriber = moq_clock::timeout(TIMEOUT, remote.track("audio").unwrap().subscribe(None))
			.await
			.unwrap()
			.unwrap();
		let mut group = moq_clock::timeout(TIMEOUT, subscriber.recv_group())
			.await
			.unwrap()
			.unwrap()
			.unwrap();
		assert_eq!(group.sequence, 1);
		let frame = moq_clock::timeout(TIMEOUT, group.read_frame())
			.await
			.unwrap()
			.unwrap()
			.unwrap();
		assert_eq!(&frame.payload[..], b"1a");
	}

	// The relay cancels the idle upstream subscription.
	moq_clock::timeout(TIMEOUT, crate::support::unwanted(track.demand()))
		.await
		.unwrap()
		.unwrap();

	g1.write_frame(None, &b"1b"[..]).unwrap();
	g1.finish().unwrap();
	let mut g2 = track.create_group(moq_net::group::Info { sequence: 2 }).unwrap();
	g2.write_frame(None, &b"2a"[..]).unwrap();

	let track = remote.track("audio").unwrap();
	let _feed = track.subscribe(None);
	let mut group = moq_clock::timeout(TIMEOUT, track.fetch_group(1, None))
		.await
		.unwrap()
		.unwrap();
	let mut frames = Vec::new();
	loop {
		match moq_clock::timeout(TIMEOUT, group.read_frame())
			.await
			.expect("group 1 hangs")
		{
			Ok(Some(frame)) => frames.push(frame.payload),
			Ok(None) => break,
			Err(err) => panic!("group 1 failed: {err}"),
		}
	}
	assert_eq!(frames, vec![&b"1a"[..], &b"1b"[..]]);
}
