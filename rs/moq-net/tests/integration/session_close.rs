#![cfg(feature = "async")]

//! `Session::close` delivers what the session queued before closing, within a deadline,
//! while `abort` still closes at once.
//!
//! Deterministic: simulated time over the mock transport.

use crate::support;

use std::time::Duration;

use moq_net::{Error, Hop, Timestamp, Version};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

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
	reader: moq_clock::sim::JoinHandle<Read>,
	_broadcast: moq_net::origin::Publication,
}

async fn setup(version: &str) -> Setup {
	setup_with_gate(version, None).await
}

async fn setup_with_gate(version: &str, gate: Option<kio::Consumer<bool>>) -> Setup {
	let publisher = produce_origin(1);
	let broadcast = publisher.create_broadcast("bcast").unwrap();
	let track = broadcast.create_track("video", timed()).unwrap();
	broadcast.announce(Default::default()).unwrap();

	let subscriber = produce_origin(2);
	let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
	options.client_publish = Some(publisher.consume());
	options.server_subscribe = Some(subscriber.clone());
	options.server_gate = gate;
	let pair = connect_mock(options).await;

	let consumer = subscriber.consume();
	let remote = moq_clock::timeout(TIMEOUT, consumer.follow("bcast", None).unwrap().available())
		.await
		.expect("resolve timeout")
		.expect("broadcast resolves");

	// The publisher only learns of a subscription once the subscriber polls it.
	let (ready, active) = futures::channel::oneshot::channel();
	let reader = moq_clock::sim::spawn(async move {
		let mut sub = remote.track("video").unwrap().subscribe(None).await.expect("subscribe");
		let _ = ready.send(());
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

	moq_clock::timeout(TIMEOUT, support::harness::subscribed(&track))
		.await
		.expect("no subscriber appeared");
	// Both ends hold the subscription, so a close tests only the group tail and not a
	// control response still in flight.
	moq_clock::timeout(TIMEOUT, active)
		.await
		.expect("the subscription never became active")
		.expect("reader alive");

	Setup {
		pair,
		track,
		reader,
		_broadcast: broadcast,
	}
}

/// TRACK_INFO creates demand before any SUBSCRIBE, so demand alone cannot gate a drain test.
#[moq_clock::test]
async fn track_info_interest_precedes_a_subscription() {
	for version in ["moq-lite-05", "moq-lite-06", "moq-lite-07-wip"] {
		let publisher = produce_origin(1);
		let broadcast = publisher.create_broadcast("bcast").unwrap();
		let track = broadcast.create_track("video", timed()).unwrap();
		broadcast.announce(Default::default()).unwrap();
		let subscriber = produce_origin(2);
		let mut options = MockConnectOptions::new(version.parse().unwrap());
		options.server_publish = Some(publisher.consume());
		options.client_subscribe = Some(subscriber.clone());
		let _pair = connect_mock(options).await;
		let consumer = subscriber.consume();
		let remote = consumer.follow("bcast", None).unwrap().available().await.unwrap();
		let copy = remote.track("video").unwrap();
		copy.info().await.expect("TRACK_INFO");
		crate::support::wanted(track.demand())
			.await
			.expect("TRACK_INFO holds demand");
		assert!(track.subscription().is_none(), "{version}: no SUBSCRIBE was requested");
	}
}

/// A finished track's last group and FIN reach the subscriber, even though the
/// close is requested before the session wrote them.
#[moq_clock::test]
async fn close_delivers_a_finished_track() {
	close_delivers_a_finished_track_for("moq-lite-05").await;
}

#[moq_clock::test]
async fn ietf_close_delivers_a_finished_track() {
	close_delivers_a_finished_track_for("moq-transport-22").await;
}

async fn close_delivers_a_finished_track_for(version: &str) {
	let Setup {
		pair, track, reader, ..
	} = setup(version).await;

	let mut group = track.append_group().unwrap();
	group.write_frame(Timestamp::ZERO, b"last".as_slice()).unwrap();
	group.finish().unwrap();
	track.finish().unwrap();

	let started = moq_clock::now();
	moq_clock::timeout(TIMEOUT, pair.client.close())
		.await
		.expect("close timed out")
		.expect("the close drains");
	assert!(
		(moq_clock::now() - started) < Duration::from_secs(1),
		"drained before the deadline"
	);

	let (got, end) = moq_clock::timeout(TIMEOUT, reader)
		.await
		.expect("reader timed out")
		.unwrap();
	assert_eq!(got, vec![b"last".to_vec()]);
	end.expect("the track finishes");
}

/// A track that never finishes holds the drain until the deadline, then the
/// session closes anyway.
#[moq_clock::test]
async fn close_gives_up_on_a_live_track() {
	close_gives_up_on_a_live_track_for("moq-lite-05").await;
}

#[moq_clock::test]
async fn ietf_close_gives_up_on_a_live_track() {
	close_gives_up_on_a_live_track_for("moq-transport-22").await;
}

async fn close_gives_up_on_a_live_track_for(version: &str) {
	let Setup {
		pair, track: _track, ..
	} = setup(version).await;

	let started = moq_clock::now();
	let res = moq_clock::timeout(TIMEOUT, pair.client.close())
		.await
		.expect("close timed out");
	assert!(matches!(res, Err(Error::Timeout)), "{res:?}");
	assert_eq!((moq_clock::now() - started), Duration::from_secs(1));
}

/// An abort from another handle cuts a drain short.
#[moq_clock::test]
async fn abort_cuts_a_drain_short() {
	abort_cuts_a_drain_short_for("moq-lite-05").await;
}

#[moq_clock::test]
async fn ietf_abort_cuts_a_drain_short() {
	abort_cuts_a_drain_short_for("moq-transport-22").await;
}

async fn abort_cuts_a_drain_short_for(version: &str) {
	let Setup {
		pair, track: _track, ..
	} = setup(version).await;

	let other = pair.client.clone();
	let started = moq_clock::now();
	let close = moq_clock::sim::spawn(pair.client.close());
	moq_clock::sleep(Duration::from_millis(100)).await;
	other.abort(Error::Cancel);

	let res = moq_clock::timeout(TIMEOUT, close)
		.await
		.expect("close timed out")
		.unwrap();
	assert!(res.is_err() && !matches!(res, Err(Error::Timeout)), "{res:?}");
	assert!(
		(moq_clock::now() - started) < Duration::from_secs(1),
		"{res:?} after {:?}",
		(moq_clock::now() - started)
	);
}

#[moq_clock::test]
async fn close_withdraws_announcements_before_closing_the_transport() {
	for version in ["moq-lite-01", "moq-lite-07-wip", "moq-transport-18", "moq-transport-22"] {
		let publisher = produce_origin(1);
		let broadcast = publisher.create_broadcast("bcast").unwrap();
		broadcast.announce(Default::default()).unwrap();
		let subscriber = produce_origin(2);
		let mut options = MockConnectOptions::new(version.parse().unwrap());
		options.client_publish = Some(publisher.consume());
		options.server_subscribe = Some(subscriber.clone());
		let pair = connect_mock(options).await;
		crate::support::announced(&subscriber.consume(), "bcast").await;
		let before = pair.client_transport.finished_streams();
		pair.client.close().await.unwrap();
		assert!(
			pair.client_transport.finished_streams() > before,
			"{version}: announcement ended by cancellation instead of FIN"
		);
	}
}

#[moq_clock::test]
async fn close_times_out_waiting_for_announcement_acknowledgements() {
	for version in ["moq-lite-01", "moq-lite-07-wip", "moq-transport-18", "moq-transport-22"] {
		let publisher = produce_origin(1);
		let broadcast = publisher.create_broadcast("bcast").unwrap();
		broadcast.announce(Default::default()).unwrap();
		let subscriber = produce_origin(2);
		let mut options = MockConnectOptions::new(version.parse().unwrap());
		options.client_publish = Some(publisher.consume());
		options.server_subscribe = Some(subscriber.clone());
		let pair = connect_mock(options).await;
		crate::support::announced(&subscriber.consume(), "bcast").await;
		pair.client_transport.hold_fin_acknowledgements();
		let started = moq_clock::now();
		assert!(matches!(pair.client.close().await, Err(Error::Timeout)), "{version}");
		assert_eq!(moq_clock::now() - started, Duration::from_secs(1));
	}
}

/// A transport ACK is not proof the subscriber read the final group.
#[moq_clock::test]
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
	let close = moq_clock::sim::spawn(pair.client.close());
	moq_clock::sleep(Duration::from_millis(100)).await;
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
#[moq_clock::test]
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
	let started = moq_clock::now();
	assert!(matches!(pair.client.close().await, Err(Error::Timeout)));
	assert_eq!(moq_clock::now() - started, Duration::from_secs(1));
}

/// Released drafts retain ACK-based close even when application delivery is delayed.
#[moq_clock::test]
async fn older_lite_close_keeps_ack_drain() {
	for version in ["moq-lite-05", "moq-lite-06"] {
		peer_close_reads_buffered_tail(version).await;
	}
}

#[moq_clock::test]
async fn earlier_lite_peer_close_reads_buffered_tail() {
	for version in ["moq-lite-01", "moq-lite-02", "moq-lite-03", "moq-lite-04"] {
		peer_close_reads_buffered_tail(version).await;
	}
}

#[moq_clock::test]
async fn ietf_peer_close_reads_buffered_tail() {
	for version in ["moq-transport-18", "moq-transport-22"] {
		peer_close_reads_buffered_tail(version).await;
	}
}

async fn peer_close_reads_buffered_tail(version: &str) {
	let Setup {
		pair, track, reader, ..
	} = setup(version).await;
	// Delivery to the transport acknowledges the FIN before the receiving
	// protocol driver has consumed the buffered stream.
	pair.client_transport.ack_fins();
	let mut group = track.append_group().unwrap();
	group.write_frame(Timestamp::ZERO, b"last".as_slice()).unwrap();
	group.finish().unwrap();
	track.finish().unwrap();
	pair.client.close().await.expect("released draft still drains on ACKs");
	let (got, _) = reader.await.unwrap();
	assert_eq!(got, vec![b"last".to_vec()], "{version}");
}

/// Legacy session/control streams and the accept error race the buffered group.
/// Holding the whole receiver ensures the close is visible on its first poll.
#[moq_clock::test]
async fn peer_close_before_receiver_poll_keeps_buffered_tail() {
	for version in [
		"moq-lite-01",
		"moq-lite-02",
		"moq-lite-05",
		"moq-lite-06",
		"moq-transport-18",
	] {
		let gate = kio::Producer::new(true);
		let Setup {
			pair, track, reader, ..
		} = setup_with_gate(version, Some(gate.consume())).await;
		*gate.write().ok().expect("receiver gate remains open") = false;
		pair.client_transport.ack_fins();
		let mut group = track.append_group().unwrap();
		group.write_frame(Timestamp::ZERO, b"last".as_slice()).unwrap();
		group.finish().unwrap();
		track.finish().unwrap();
		pair.client.close().await.expect("FIN delivery needs no receiver poll");
		*gate.write().ok().expect("receiver gate remains open") = true;
		let (got, _) = reader.await.unwrap();
		assert_eq!(got, vec![b"last".to_vec()], "{version}");
	}
}

/// A locally unsupported feature uses only the negotiated session registry.
#[moq_clock::test]
async fn unsupported_abort_uses_the_negotiated_session_registry() {
	for protocol in ["moq-lite-06", "moq-lite-07-wip", "moq-transport-18", "moq-transport-22"] {
		let version: Version = protocol.parse().unwrap();
		let pair = connect_mock(MockConnectOptions::new(version)).await;
		pair.client.abort(Error::Unsupported);
		let _ = pair.server.closed().await;
		let (code, _) = pair.server_transport.close_reason().expect("transport closed");
		assert_eq!(code, if version.is_lite() { 0x30 } else { 0x1 }, "{protocol}");
	}
}

/// A Lite-only close code arriving on IETF stays unknown instead of gaining Lite semantics.
#[moq_clock::test]
async fn unsupported_close_is_decoded_by_the_negotiated_session_registry() {
	for protocol in ["moq-lite-06", "moq-lite-07-wip", "moq-transport-18", "moq-transport-22"] {
		let version: Version = protocol.parse().unwrap();
		let pair = connect_mock(MockConnectOptions::new(version)).await;
		moq_net::transport::poll::Session::close(&pair.server_transport, 0x30, "unsupported");
		let error = pair.client.closed().await;
		let expected = if version.is_lite() {
			moq_net::SessionError::Unsupported
		} else {
			moq_net::SessionError::Unknown(0x30)
		};
		assert!(matches!(error, Error::Session(code) if code == expected), "{protocol}");
	}
}

/// moq-lite's own session range (0x30-0x3f) is never sent as an unknown code on either
/// wire, while a code outside it passes through.
#[moq_clock::test]
async fn an_unknown_lite_range_abort_is_unspecified_on_every_wire() {
	for protocol in ["moq-lite-06", "moq-lite-07-wip", "moq-transport-18", "moq-transport-22"] {
		for (unknown, expected) in [(0x31, 0x1), (0x20, 0x20)] {
			let version: Version = protocol.parse().unwrap();
			let pair = connect_mock(MockConnectOptions::new(version)).await;
			pair.client
				.abort(Error::Session(moq_net::SessionError::Unknown(unknown)));
			let _ = pair.server.closed().await;
			let (code, _) = pair.server_transport.close_reason().expect("transport closed");
			assert_eq!(code, expected, "{protocol} {unknown:#x}");
		}
	}
}

/// How the server's session ended, as `closed` reports it and as its stats record it.
async fn ended(version: &str, close: impl FnOnce(&MockPair)) -> (Error, moq_net::stats::Failures) {
	let registry = moq_net::stats::Registry::new(Default::default());
	let mut options = MockConnectOptions::new(version.parse().unwrap());
	options.server_stats = registry
		.tier(Default::default())
		.session(moq_net::Path::from_static("room"));
	let pair = connect_mock(options).await;
	close(&pair);
	let err = moq_clock::timeout(TIMEOUT, pair.server.closed())
		.await
		.expect("the session closes");
	drop(pair);
	// The drivers release the stats once the transport has closed under them.
	moq_clock::sleep(Duration::from_secs(1)).await;
	let presence = registry.snapshot().sessions()[0].1;
	assert_eq!(presence.sessions_ended, 1, "{version}: the session ended");
	(err, presence.sessions_failed)
}

/// A clean close is `Error::Cancel` and no failure, whichever side closed. Anything else
/// counts by its kind, without the caller recording it.
#[moq_clock::test]
async fn a_session_records_how_it_ended() {
	use moq_net::stats::Failures;
	for version in ["moq-lite-06", "moq-lite-07-wip", "moq-transport-22"] {
		let (err, failed) = ended(version, |pair| pair.client.abort(Error::Cancel)).await;
		assert!(matches!(err, Error::Cancel), "{version}: {err:?}");
		assert_eq!(failed, Failures::default(), "{version}");

		// A drain that gave up is still this side ending the session on purpose.
		let (err, failed) = ended(version, |pair| pair.server.abort(Error::GoingAway)).await;
		assert!(matches!(err, Error::Cancel), "{version}: {err:?}");
		assert_eq!(failed, Failures::default(), "{version}");

		let (_, failed) = ended(version, |pair| pair.client.abort(Error::App(7))).await;
		assert_eq!(failed.application, 1, "{version}: {failed:?}");

		let (_, failed) = ended(version, |pair| pair.server.abort(Error::Expired)).await;
		assert_eq!(failed.expired, 1, "{version}: {failed:?}");
	}
}
