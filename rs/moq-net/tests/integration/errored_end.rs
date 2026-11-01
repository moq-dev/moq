#![cfg(feature = "async")]

//! A track that ends with an error keeps its history across a hop: a subscriber reads every
//! group below the end, then the error, and so does one subscribing after the first ended.
//! Before lite-07 the end has nowhere to carry its error, so the subscription resets with it.

use crate::support;

use std::time::Duration;

use moq_net::track::{Position, Subscription};
use moq_net::{Error, Hop, StreamError, Timestamp, Version};
use support::harness::{MockConnectOptions, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(10);
const GROUPS: u64 = 2;
const FRAMES: [&[u8]; 2] = [b"first", b"second"];

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

/// Read every group from the first, frame by frame, returning each group's sequence and
/// payloads, and how the read ended.
async fn read(broadcast: &moq_net::broadcast::Consumer) -> (Vec<(u64, Vec<Vec<u8>>)>, Option<Error>) {
	let subscription = Subscription::default()
		.with_range(0..)
		.with_max_delay(Duration::from_secs(30));
	let mut sub = kio::clock::timeout(TIMEOUT, broadcast.track("video").unwrap().subscribe(subscription))
		.await
		.expect("subscribe timeout")
		.expect("subscribe");
	let mut got = Vec::new();
	loop {
		match kio::clock::timeout(TIMEOUT, sub.recv_group())
			.await
			.expect("read timeout")
		{
			Ok(Some(mut group)) => {
				let mut frames = Vec::new();
				while let Some(frame) = kio::clock::timeout(TIMEOUT, group.read_frame())
					.await
					.expect("frame timeout")
					.unwrap_or_else(|err| panic!("group {} cut short: {err:?}", group.sequence))
				{
					frames.push(frame.payload.to_vec());
				}
				got.push((group.sequence, frames));
			}
			Ok(None) => return (got, None),
			Err(err) => return (got, Some(err)),
		}
	}
}

/// What [`read`] returns for the whole history.
fn history() -> Vec<(u64, Vec<Vec<u8>>)> {
	(0..GROUPS)
		.map(|sequence| (sequence, FRAMES.iter().map(|frame| frame.to_vec()).collect()))
		.collect()
}

/// Each group stream's header lands, but its frames and FIN only after the subscription's
/// end, as QUIC does not order streams. A stream whose header arrived still belongs to the
/// track, so the hop reads it to its end. With the SUBSCRIBE first, the publisher's groups
/// also beat its TRACK_INFO, so they park on it and the end lands as the accept unparks them.
#[kio::test]
async fn an_errored_end_keeps_its_history_across_a_hop() {
	for (name, subscribe_first) in [
		("moq-lite-06", false),
		("moq-lite-06", true),
		("moq-lite-07-wip", false),
		("moq-lite-07-wip", true),
	] {
		let label = format!("{name} subscribe_first={subscribe_first}");
		let version: Version = name.parse().unwrap();
		let publisher = produce_origin(1);
		let broadcast = publisher.create_broadcast("bcast").unwrap();
		let info = moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI);
		let mut track = broadcast.create_track("video", info).unwrap();
		broadcast.announce(Default::default()).unwrap();
		for _ in 0..GROUPS {
			let mut group = track.append_group().unwrap();
			for frame in FRAMES {
				group.write_frame(Timestamp::ZERO, frame).unwrap();
			}
			group.finish().unwrap();
		}
		track.finish_at(Position::group(GROUPS), Some(Error::App(7))).unwrap();

		let subscriber = produce_origin(2);
		let mut options = MockConnectOptions::new(version);
		options.server_publish = Some(publisher.consume());
		options.client_subscribe = Some(subscriber.clone());
		let session = connect_mock(options).await;
		let remote = kio::clock::timeout(TIMEOUT, subscriber.consume().follow("bcast", None).unwrap().available())
			.await
			.expect("resolve timeout")
			.expect("broadcast resolves");

		session.server_transport.split_unis();
		if subscribe_first {
			session.client_transport.hold_bidis();
		}
		let release = async {
			if subscribe_first {
				kio::clock::sleep(Duration::from_millis(1)).await;
				session.client_transport.release_bidis_reversed();
			}
			kio::clock::sleep(Duration::from_millis(100)).await;
			if name != "moq-lite-07-wip" {
				assert!(
					session.server_transport.resets().contains(&(64 + 7)),
					"{label}: the reset was not sent before the frames"
				);
			}
			session.server_transport.release_split();
		};
		let (first, ()) = futures::join!(read(&remote), release);
		let later = read(&remote).await;
		for (what, (got, err)) in [("first", first), ("later", later)] {
			assert_eq!(got, history(), "{label} {what}");
			match name == "moq-lite-07-wip" {
				true => assert!(
					matches!(err, Some(Error::Stream(StreamError::App(7)))),
					"{label} {what}: {err:?}"
				),
				false => assert!(err.is_some(), "{label} {what}: ended cleanly"),
			}
		}
		// Lite-07 ends the subscription after SUBSCRIBE_END rather than resetting it.
		let reset = session.server_transport.resets().contains(&(64 + 7));
		assert_eq!(
			reset,
			name != "moq-lite-07-wip",
			"{label}: resets {:?}",
			session.server_transport.resets()
		);
		drop((session, track, broadcast, publisher, subscriber));
	}
}
