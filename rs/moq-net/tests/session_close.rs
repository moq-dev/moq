//! `Session::close` delivers what the session queued before closing, within a deadline,
//! while `abort` still closes at once.
//!
//! Deterministic: simulated time over the mock transport.

mod support;

use std::time::Duration;

use moq_net::{Error, Hop, Timestamp, Version};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(10);

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

/// The payloads the server read, then how the track ended.
type Read = (Vec<Vec<u8>>, Result<(), Error>);

/// A client publishing one track to a server that subscribes to it.
struct Setup {
	pair: MockPair,
	track: moq_net::track::Producer,
	reader: moq_net_sim::JoinHandle<Read>,
	_broadcast: moq_net::broadcast::Producer,
}

async fn setup(version: &str) -> Setup {
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let track = broadcast.create_track("video", None).unwrap();
	broadcast.announce(Default::default()).unwrap();

	let subscriber = produce_origin(2);
	let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
	options.client_publish = Some(publisher.consume());
	options.server_subscribe = Some(subscriber.clone());
	let pair = connect_mock(options).await;

	let consumer = subscriber.consume();
	moq_net_sim::timeout(TIMEOUT, consumer.routed("bcast"))
		.await
		.expect("announce timeout")
		.expect("routed");
	let remote = moq_net_sim::timeout(TIMEOUT, consumer.request_broadcast("bcast", None))
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves");

	// The publisher only learns of a subscription once the subscriber polls it.
	let reader = moq_net_sim::spawn(async move {
		let mut sub = remote.track("video").unwrap().subscribe(None).await.expect("subscribe");
		let mut got = Vec::new();
		loop {
			let mut group = match sub.recv_group().await {
				Ok(Some(group)) => group,
				Ok(None) => return (got, Ok(())),
				Err(err) => return (got, Err(err)),
			};
			loop {
				match group.read_frame().await {
					Ok(Some(frame)) => got.push(frame.payload.to_vec()),
					Ok(None) => break,
					Err(err) => return (got, Err(err)),
				}
			}
		}
	});

	moq_net_sim::timeout(TIMEOUT, support::harness::subscribed(&track))
		.await
		.expect("no subscriber appeared");

	Setup {
		pair,
		track,
		reader,
		_broadcast: broadcast,
	}
}

/// A finished track's last group and FIN reach the subscriber, even though the
/// close is requested before the session wrote them.
#[moq_net_sim::test]
async fn close_delivers_a_finished_track() {
	close_delivers_a_finished_track_for("moq-lite-05").await;
}

#[moq_net_sim::test]
async fn ietf_close_delivers_a_finished_track() {
	close_delivers_a_finished_track_for("moq-transport-20").await;
}

async fn close_delivers_a_finished_track_for(version: &str) {
	let Setup {
		pair, track, reader, ..
	} = setup(version).await;

	let mut group = track.append_group().unwrap();
	group.write_frame(Timestamp::ZERO, b"last".as_slice()).unwrap();
	group.finish().unwrap();
	track.finish().unwrap();

	let started = moq_net_sim::now();
	moq_net_sim::timeout(TIMEOUT, pair.client.close())
		.await
		.expect("close timed out")
		.expect("the close drains");
	assert!(
		(moq_net_sim::now() - started) < Duration::from_secs(1),
		"drained before the deadline"
	);

	let (got, end) = moq_net_sim::timeout(TIMEOUT, reader)
		.await
		.expect("reader timed out")
		.unwrap();
	assert_eq!(got, vec![b"last".to_vec()]);
	end.expect("the track finishes");
}

/// A track that never finishes holds the drain until the deadline, then the
/// session closes anyway.
#[moq_net_sim::test]
async fn close_gives_up_on_a_live_track() {
	close_gives_up_on_a_live_track_for("moq-lite-05").await;
}

#[moq_net_sim::test]
async fn ietf_close_gives_up_on_a_live_track() {
	close_gives_up_on_a_live_track_for("moq-transport-20").await;
}

async fn close_gives_up_on_a_live_track_for(version: &str) {
	let Setup {
		pair, track: _track, ..
	} = setup(version).await;

	let started = moq_net_sim::now();
	let res = moq_net_sim::timeout(TIMEOUT, pair.client.close())
		.await
		.expect("close timed out");
	assert!(matches!(res, Err(Error::Timeout)), "{res:?}");
	assert_eq!((moq_net_sim::now() - started), Duration::from_secs(1));
}

/// An abort from another handle cuts a drain short.
#[moq_net_sim::test]
async fn abort_cuts_a_drain_short() {
	abort_cuts_a_drain_short_for("moq-lite-05").await;
}

#[moq_net_sim::test]
async fn ietf_abort_cuts_a_drain_short() {
	abort_cuts_a_drain_short_for("moq-transport-20").await;
}

async fn abort_cuts_a_drain_short_for(version: &str) {
	let Setup {
		pair, track: _track, ..
	} = setup(version).await;

	let other = pair.client.clone();
	let started = moq_net_sim::now();
	let close = moq_net_sim::spawn(pair.client.close());
	moq_net_sim::sleep(Duration::from_millis(100)).await;
	other.abort(Error::Cancel);

	let res = moq_net_sim::timeout(TIMEOUT, close)
		.await
		.expect("close timed out")
		.unwrap();
	assert!(res.is_err() && !matches!(res, Err(Error::Timeout)), "{res:?}");
	assert!(
		(moq_net_sim::now() - started) < Duration::from_secs(1),
		"{res:?} after {:?}",
		(moq_net_sim::now() - started)
	);
}

#[moq_net_sim::test]
async fn close_withdraws_announcements_before_closing_the_transport() {
	for version in ["moq-lite-01", "moq-lite-07-wip", "moq-transport-17", "moq-transport-22"] {
		let publisher = produce_origin(1);
		let broadcast = publisher.create_broadcast("bcast").unwrap();
		broadcast.announce(Default::default()).unwrap();
		let subscriber = produce_origin(2);
		let mut options = MockConnectOptions::new(version.parse().unwrap());
		options.client_publish = Some(publisher.consume());
		options.server_subscribe = Some(subscriber.clone());
		let pair = connect_mock(options).await;
		subscriber.consume().routed("bcast").await.unwrap();
		let before = pair.client_transport.finished_streams();
		pair.client.close().await.unwrap();
		assert!(
			pair.client_transport.finished_streams() > before,
			"{version}: announcement ended by cancellation instead of FIN"
		);
	}
}

#[moq_net_sim::test]
async fn close_times_out_waiting_for_announcement_acknowledgements() {
	for version in ["moq-lite-01", "moq-lite-07-wip", "moq-transport-17", "moq-transport-22"] {
		let publisher = produce_origin(1);
		let broadcast = publisher.create_broadcast("bcast").unwrap();
		broadcast.announce(Default::default()).unwrap();
		let subscriber = produce_origin(2);
		let mut options = MockConnectOptions::new(version.parse().unwrap());
		options.client_publish = Some(publisher.consume());
		options.server_subscribe = Some(subscriber.clone());
		let pair = connect_mock(options).await;
		subscriber.consume().routed("bcast").await.unwrap();
		pair.client_transport.hold_fin_acknowledgements();
		let started = moq_net_sim::now();
		assert!(matches!(pair.client.close().await, Err(Error::Timeout)), "{version}");
		assert_eq!(moq_net_sim::now() - started, Duration::from_secs(1));
	}
}

/// A transport ACK is not proof the subscriber read the final group.
#[moq_net_sim::test]
async fn close_tail() {
	let Setup {
		pair, track, reader, ..
	} = setup("moq-lite-07-wip").await;
	pair.client_transport.ack_fins();
	pair.client_transport.hold_unis();
	let mut group = track.append_group().unwrap();
	group.write_frame(Timestamp::ZERO, b"last".as_slice()).unwrap();
	group.finish().unwrap();
	track.finish().unwrap();
	let close = moq_net_sim::spawn(pair.client.close());
	moq_net_sim::sleep(Duration::from_millis(100)).await;
	assert!(
		!close.is_finished(),
		"close must wait for the subscriber, not the transport ACK"
	);
	pair.client_transport.release_unis();
	close
		.await
		.unwrap()
		.expect("close drains after the subscriber reads the tail");
	let (got, end) = reader.await.unwrap();
	assert_eq!(got, vec![b"last".to_vec()]);
	end.expect("track ends cleanly");
}

/// A lite07 peer that never acknowledges the track end cannot report a successful close.
#[moq_net_sim::test]
async fn close_requires_subscriber_fin() {
	let Setup {
		pair,
		track,
		reader: _reader,
		..
	} = setup("moq-lite-07-wip").await;
	pair.client_transport.ack_fins();
	pair.server_transport.withhold_bidi_fins();
	let mut group = track.append_group().unwrap();
	group.write_frame(Timestamp::ZERO, b"last".as_slice()).unwrap();
	group.finish().unwrap();
	track.finish().unwrap();
	let started = moq_net_sim::now();
	assert!(matches!(pair.client.close().await, Err(Error::Timeout)));
	assert_eq!(moq_net_sim::now() - started, Duration::from_secs(1));
}

/// Released drafts retain ACK-based close even when application delivery is delayed.
#[moq_net_sim::test]
async fn older_lite_close_keeps_ack_drain() {
	for version in ["moq-lite-05", "moq-lite-06"] {
		let Setup {
			pair,
			track,
			reader: _reader,
			..
		} = setup(version).await;
		pair.client_transport.ack_fins();
		pair.client_transport.hold_unis();
		let mut group = track.append_group().unwrap();
		group.write_frame(Timestamp::ZERO, b"last".as_slice()).unwrap();
		group.finish().unwrap();
		track.finish().unwrap();
		pair.client.close().await.expect("released draft still drains on ACKs");
	}
}
