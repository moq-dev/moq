//! Over real QUIC, a subscription ends cleanly only when every group in its range has
//! been accounted for; the publisher disconnecting with a group still in flight ends it
//! with an error.
//!
//! moq-lite, Subscribe: "The publisher closes the stream (FIN) only once every group
//! from start to end has been accounted for, either via a Group Stream (completed or
//! reset) or a SUBSCRIBE_DROP message." And Routing: "when the serving session ends,
//! in-flight subscriptions end with it (a reset)". So `recv_group()` may return
//! `Ok(None)` only after that FIN; any other ending is an error.
//!
//! The in-process twin of this test is `moq-net`'s `subscription_end_integrity`, which
//! pins the rule deterministically over the mock transport. This one repeats it over a
//! real QUIC connection, where the publisher's disconnect is a `CONNECTION_CLOSE` that
//! cuts the final group's stream mid-flight.
//!
//! The subscriber drains a head group first (so the subscription is established and
//! only the tail is at stake), then stops reading for [`STALL`] while the publisher
//! writes a 4 MB final group, finishes the track and disconnects, so the group is still
//! queued behind the connection's flow-control window when the session goes away. The
//! subscriber must then either see every frame or end with an error; it must not end
//! `Ok(None)` short of the final group.
//!
//! Whether the publisher should have been able to flush before disconnecting is a
//! separate question (it has nothing to await today); this test is only about the
//! signal the subscriber gets when it could not.

use std::time::Duration;

use moq_tokio::moq_net;

const TIMEOUT: Duration = Duration::from_secs(10);
const FRAMES: usize = 10;
/// Frames are padded so the final group (4 MB of it) cannot fit in the connection's
/// flow-control window while the subscriber is not reading: without unsent bytes at
/// close, the publisher would only ever lose its FIN, and the test would not cover
/// a group cut mid-flight.
const PADDING: usize = 400_000;
/// How long the subscriber stops reading after the head group, so the final group is
/// still queued in the publisher when the session goes away.
const STALL: Duration = Duration::from_millis(300);

/// One round: a head group the subscriber drains, then a final group, `finish()`, and
/// the publisher's session dropped with no await in between. Returns how many frames
/// the subscriber read and the error its subscription ended with, if any.
async fn round(drop_session: bool) -> (usize, Option<moq_net::Error>) {
	let pub_origin = moq_tokio::origin::spawn();
	let broadcast = pub_origin.create_broadcast("test").expect("create broadcast");
	broadcast.announce(Default::default()).expect("announce");
	let track = broadcast.create_track("video", None).expect("create track");

	let mut server_config = moq_tokio::listen::Config::default();
	server_config.bind = Some("127.0.0.1:0".parse().unwrap());
	server_config.tls.generate = vec!["localhost".into()];
	let mut server = server_config
		.init(Default::default())
		.expect("server init")
		.listen()
		.await
		.expect("listen");
	let port = server.local_addr().expect("local addr").port();

	// The publisher only learns of a subscription once the subscriber polls it, so the
	// reader runs concurrently. It tells the publisher when it has drained the head
	// group, so the final group is written against an established subscription.
	let (drained_tx, drained_rx) = tokio::sync::oneshot::channel();

	let reader = tokio::spawn(async move {
		let mut client_config = moq_tokio::connect::Config::default();
		client_config.tls.insecure = Some(true);
		let sub_origin = moq_tokio::origin::spawn();
		let consumer = sub_origin.consume();
		let client = client_config.init(Default::default()).expect("client init");
		let url: url::Url = format!("https://localhost:{port}").parse().expect("parse url");
		let _connection = client
			.with_subscriber(sub_origin)
			.with_reconnect(false)
			.connect(url)
			.established()
			.await
			.expect("client connect");

		consumer.routed("test").await.expect("routed");
		let remote = consumer.request_broadcast("test").await.expect("broadcast resolves");
		let mut sub = remote.track("video").unwrap().subscribe(None).await.expect("subscribe");

		let mut got = 0;
		let mut drained = Some(drained_tx);
		loop {
			let mut group = match sub.recv_group().await {
				Ok(Some(group)) => group,
				Ok(None) => return (got, None),
				Err(err) => return (got, Some(err)),
			};
			loop {
				match group.read_frame().await {
					Ok(Some(_)) => got += 1,
					Ok(None) => break,
					Err(err) => return (got, Some(err)),
				}
			}
			// The head group is in: from here on, only the tail can be lost. Stop reading
			// for a moment, so the publisher's final group is still queued behind the
			// connection's flow-control window when it disconnects.
			if let Some(tx) = drained.take() {
				let _ = tx.send(());
				tokio::time::sleep(STALL).await;
			}
		}
	});

	let request = tokio::time::timeout(TIMEOUT, server.accept())
		.await
		.expect("accept timeout")
		.expect("no incoming connection");
	let session = request
		.with_publisher(&pub_origin)
		.ok()
		.await
		.expect("publisher session");

	tokio::time::timeout(TIMEOUT, track.used())
		.await
		.expect("no subscriber appeared")
		.expect("track closed");

	write_group(&track);
	tokio::time::timeout(TIMEOUT, drained_rx)
		.await
		.expect("the head group never arrived")
		.expect("reader gone");

	// The final group, a clean group end and a clean track end, innermost first and with
	// no await in between: the session has sent none of it yet.
	write_group(&track);
	track.finish().expect("finish track");
	drop(track);

	// The publisher is done, so it disconnects. Nothing it could have awaited first.
	let session = if drop_session {
		drop(session);
		None
	} else {
		Some(session)
	};

	let result = tokio::time::timeout(TIMEOUT, reader)
		.await
		.expect("the subscription never ended")
		.expect("reader panicked");
	drop((session, broadcast, pub_origin, server));
	result
}

/// Append one group of [`FRAMES`] padded frames.
fn write_group(track: &moq_net::track::Producer) {
	let mut group = track.append_group().expect("append group");
	for _ in 0..FRAMES {
		group
			.write_frame(moq_net::Timestamp::ZERO, vec![0; PADDING])
			.expect("write frame");
	}
	group.finish().expect("finish group");
}

/// The publisher disconnects with the final group in flight: the subscription ends
/// with an error, or delivers every frame.
#[tokio::test]
async fn a_subscription_cut_by_the_publisher_disconnecting_does_not_end_clean() {
	let (got, err) = round(true).await;
	assert!(
		err.is_some() || got == 2 * FRAMES,
		"the subscription ended Ok(None) with {got}/{} frames \
		 (a subscription that did not deliver every group must end with an error, not as complete)",
		2 * FRAMES,
	);
}

/// With the session kept alive until the subscriber is done, the same track arrives
/// whole and ends `Ok(None)`.
#[tokio::test]
async fn a_finished_track_ends_clean_while_its_session_lives() {
	let (got, err) = round(false).await;
	assert!(
		err.is_none() && got == 2 * FRAMES,
		"session kept alive: got {got}/{} frames, err={err:?}",
		2 * FRAMES,
	);
}
