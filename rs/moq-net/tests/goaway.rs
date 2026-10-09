//! GOAWAY behavior tests over the in-memory mock transport.
//!
//! The mock delivers all queued bytes deterministically (no real QUIC or
//! network I/O), so these tests are reliable without sleeps: every wait is on
//! an observable event.

mod support;

use std::time::Duration;

use moq_net::{Hop, Version, goaway::Goaway};
use support::harness::{MockConnectOptions, MockPair, connect_mock};
use support::mock::create_mock_session_pair;

/// Build an origin producer, spawning its driver on the ambient runtime.
fn produce_origin(hop: Hop) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(hop));
	support::harness::spawn(driver);
	producer
}

/// Maximum time any single test may run before being treated as a deadlock.
const TEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Every version with GOAWAY support, across both wires and their distinct
/// channels: the lite Goaway control stream, the IETF draft-14-16 shared
/// control stream, and the IETF draft-17+ SETUP uni streams.
const GOAWAY_VERSIONS: &[&str] = &[
	"moq-lite-04",
	"moq-lite-05",
	"moq-transport-14",
	"moq-transport-16",
	"moq-transport-17",
	"moq-transport-18",
	"moq-transport-19",
];

/// Server drains with a redirect URI; the client observes it through
/// `Session::draining()`. Exercises every GOAWAY channel in the version
/// matrix, in both directions.
#[moq_net_sim::test]
async fn goaway_send_receive_all_versions() {
	for version in GOAWAY_VERSIONS {
		moq_net_sim::timeout(TEST_TIMEOUT, async {
			let version: Version = version.parse().unwrap();
			let pair = connect_mock(MockConnectOptions::new(version)).await;

			// Server -> client.
			pair.server
				.drain()
				.send(Goaway::redirect("https://new.example.com"))
				.expect("send goaway");

			let goaway = pair
				.client
				.draining()
				.recv()
				.await
				.expect("session closed before GOAWAY");
			assert_eq!(goaway.uri(), "https://new.example.com", "version {version}");
			assert!(pair.client.draining().peek().is_some(), "version {version}");
			// No deadline was advertised.
			assert_eq!(goaway.timeout(), None, "version {version}");

			// A session sends at most one GOAWAY: a second is refused, not silently
			// swapped in behind the peer's back.
			let err = pair
				.server
				.drain()
				.send(Goaway::redirect("https://second.example.com"))
				.expect_err("a session sends one GOAWAY");
			assert!(matches!(err, moq_net::Error::Duplicate), "version {version}");

			// Client leaves; the drain completes.
			drop(pair.client);
			pair.server.closed().await;
		})
		.await
		.unwrap_or_else(|_| panic!("test timed out on {version} (likely a mock deadlock)"));
	}
}

/// The IETF draft-17+ GOAWAY carries a timeout on the wire; the receiver
/// observes the sender's advertised deadline.
#[moq_net_sim::test]
async fn goaway_wire_timeout_moq_transport_17() {
	moq_net_sim::timeout(TEST_TIMEOUT, async {
		let version: Version = "moq-transport-17".parse().unwrap();
		let pair = connect_mock(MockConnectOptions::new(version)).await;

		pair.server
			.drain()
			.send(Goaway::redirect("moqt://relay.example/").with_timeout(Duration::from_secs(5)))
			.expect("send goaway");

		let goaway = pair
			.client
			.draining()
			.recv()
			.await
			.expect("session closed before GOAWAY");
		assert_eq!(goaway.uri(), "moqt://relay.example/");
		assert_eq!(goaway.timeout(), Some(Duration::from_secs(5)));

		drop(pair.client);
		pair.server.closed().await;
	})
	.await
	.expect("test timed out (likely a mock deadlock)");
}

/// The client also drains the server: GOAWAY is symmetric.
#[moq_net_sim::test]
async fn goaway_client_to_server_moq_lite_04() {
	moq_net_sim::timeout(TEST_TIMEOUT, async {
		let version: Version = "moq-lite-04".parse().unwrap();
		let pair = connect_mock(MockConnectOptions::new(version)).await;

		pair.client.drain().send(Goaway::new()).expect("send goaway");

		let goaway = pair
			.server
			.draining()
			.recv()
			.await
			.expect("session closed before GOAWAY");
		assert_eq!(goaway.uri(), "", "empty URI = reconnect to the same endpoint");
		assert!(pair.server.draining().peek().is_some());

		drop(pair.server);
		pair.client.closed().await;
	})
	.await
	.expect("test timed out (likely a mock deadlock)");
}

/// A version with no GOAWAY message (moq-lite-03 and earlier) still drains: the
/// deadline is the sender's own timer, so the session closes on schedule even
/// though the peer is never told why. Callers do not branch on the version.
#[moq_net_sim::test]
async fn goaway_drains_without_a_wire_message_moq_lite_03() {
	moq_net_sim::timeout(TEST_TIMEOUT, async {
		let version: Version = "moq-lite-03".parse().unwrap();
		let pair = connect_mock(MockConnectOptions::new(version)).await;

		pair.server
			.drain()
			.send(Goaway::new().with_timeout(Duration::from_secs(1)))
			.expect("drain is available on every version");

		// Nothing reaches the peer, since this version has no GOAWAY message.
		assert!(pair.client.draining().peek().is_none());

		// The deadline still force-closes the session on schedule.
		let err = pair.server.closed().await;
		assert!(
			matches!(err, moq_net::Error::Session(moq_net::SessionError::GoawayTimeout)),
			"expected a GoawayTimeout close, got {err}"
		);
	})
	.await
	.expect("test timed out (a version without GOAWAY must still honor the deadline)");
}

/// A moq-transport client cannot tell a server where to reconnect, so naming a
/// URI is refused locally instead of getting the session closed by the peer.
#[moq_net_sim::test]
async fn goaway_client_redirect_refused_moq_transport_19() {
	moq_net_sim::timeout(TEST_TIMEOUT, async {
		let version: Version = "moq-transport-19".parse().unwrap();
		let pair = connect_mock(MockConnectOptions::new(version)).await;

		let err = pair
			.client
			.drain()
			.send(Goaway::redirect("https://elsewhere.example/"))
			.expect_err("a client may not name a redirect URI");
		assert!(matches!(err, moq_net::Error::ProtocolViolation));
		// The refusal is local, so nothing was claimed: the client may still send the
		// empty-URI GOAWAY it was always allowed to send.
		pair.client.drain().send(Goaway::new()).expect("an empty URI is legal");

		// An empty URI ("I am leaving") is what a client may send. Check it from the
		// server side, which is the direction allowed to redirect at all.
		pair.server.drain().send(Goaway::new()).expect("send goaway");
		let goaway = pair
			.client
			.draining()
			.recv()
			.await
			.expect("session closed before GOAWAY");
		assert_eq!(goaway.uri(), "");
	})
	.await
	.expect("test timed out (likely a mock deadlock)");
}

/// The draining side force-closes the session when the peer overstays the
/// deadline, and the peer observes the GOAWAY_TIMEOUT close code.
#[moq_net_sim::test]
async fn goaway_timeout_force_close_moq_transport_17() {
	moq_net_sim::timeout(TEST_TIMEOUT, async {
		let version: Version = "moq-transport-17".parse().unwrap();
		let pair = connect_mock(MockConnectOptions::new(version)).await;

		pair.server
			.drain()
			.send(Goaway::redirect("moqt://relay.example/").with_timeout(Duration::from_millis(100)))
			.expect("send goaway");

		// The client observes the GOAWAY but deliberately does NOT leave.
		let goaway = pair
			.client
			.draining()
			.recv()
			.await
			.expect("session closed before GOAWAY");
		assert_eq!(goaway.timeout(), Some(Duration::from_millis(100)));

		// The deadline fires and the driver force-closes with GOAWAY_TIMEOUT (0x10),
		// which the peer decodes back through the session registry.
		let reason = pair.client.closed().await;
		assert!(
			matches!(reason, moq_net::Error::Session(moq_net::SessionError::GoawayTimeout)),
			"peer should observe the GoawayTimeout close: {reason}"
		);
	})
	.await
	.expect("test timed out (likely a mock deadlock)");
}

/// Regression: a duplicate GOAWAY (a protocol violation; a peer sends at most
/// one per session) is ignored rather than replacing the first payload, since
/// an observer may already be acting on the first URI.
///
/// The public API's drain claim only ever sends one GOAWAY per session, so the
/// handshake is hand-rolled to keep a raw transport clone for injecting
/// wire-level GOAWAY control streams.
#[moq_net_sim::test]
async fn duplicate_goaway_keeps_first_payload_moq_lite_04() {
	/// Open a raw lite Goaway control stream:
	/// `[ControlType::Goaway][message size][uri length][uri bytes]`.
	/// Varints under 64 encode as a single byte, so the frame is hand-rolled.
	/// Returns the recv half so the caller can wait for the peer to fully
	/// process (and drop) the stream.
	async fn send_goaway_raw<S: moq_net::transport::poll::Session>(session: &mut S, uri: &str) -> S::RecvStream {
		use moq_net::transport::poll::SendStream as _;
		assert!(uri.len() < 63, "helper only encodes single-byte varints");
		// Message body = [uri length varint][uri bytes]; the size prefix covers it.
		let mut frame = vec![0x05u8, uri.len() as u8 + 1, uri.len() as u8];
		frame.extend_from_slice(uri.as_bytes());

		let (mut send, recv) = session.open_bi().await.map_err(|e| e.to_string()).expect("open_bi");
		let n = send.write(&frame).await.map_err(|e| e.to_string()).expect("write");
		assert_eq!(n, frame.len());
		send.finish().map_err(|e| e.to_string()).expect("finish");
		recv
	}

	/// Block until the peer closes its half of the stream, i.e. it finished
	/// processing the control message and dropped the stream.
	async fn wait_processed<R: moq_net::transport::poll::RecvStream>(mut recv: R) {
		let mut buf = [0u8; 16];
		while let Ok(Some(_)) = recv.read(&mut buf).await {}
	}

	moq_net_sim::timeout(TEST_TIMEOUT, async {
		let version: Version = "moq-lite-04".parse().unwrap();

		let (client_transport, server_transport) = create_mock_session_pair(Some(version.alpn()));
		let server_raw = server_transport.clone();

		let client = moq_net::Client::new().with_versions(version.into());
		let server = moq_net::Server::new().with_versions(version.into());
		let (client_result, server_result) = futures::join!(
			client.connect(support::harness::now(), client_transport),
			server.accept(support::harness::now(), server_transport)
		);
		let (client_session, client_driver) = client_result.expect("client handshake failed");
		support::harness::spawn(client_driver);
		let (_server_session, server_driver) = server_result.expect("server handshake failed");
		support::harness::spawn(server_driver);

		// First GOAWAY: observed with its URI. Waiting for the peer to close the
		// stream guarantees the control message was fully processed.
		let mut server_raw = server_raw;
		let recv_a = send_goaway_raw(&mut server_raw, "a").await;
		wait_processed(recv_a).await;
		let goaway = client_session
			.draining()
			.recv()
			.await
			.expect("session closed before GOAWAY");
		assert_eq!(goaway.uri(), "a");
		assert!(client_session.draining().peek().is_some());

		// Second GOAWAY: once the client has fully processed the stream, the
		// observed payload must still carry the FIRST URI.
		let recv_b = send_goaway_raw(&mut server_raw, "bb").await;
		wait_processed(recv_b).await;

		let goaway = client_session
			.draining()
			.recv()
			.await
			.expect("session closed before GOAWAY");
		assert_eq!(goaway.uri(), "a", "duplicate GOAWAY must not replace the first payload");
	})
	.await
	.expect("test timed out (likely a mock deadlock)");
}

/// After a received GOAWAY, a new subscription still opens on the draining
/// session, which is the only route, while an existing one keeps delivering.
async fn goaway_keeps_new_subscribes(version: Version) {
	moq_net_sim::timeout(TEST_TIMEOUT, async {
		// Server publishes a broadcast with one live track.
		let pub_origin = produce_origin(Hop::random());
		let broadcast = pub_origin.create_broadcast("test").expect("create broadcast");
		broadcast.announce(moq_net::origin::Route::default()).expect("announce");
		let track = broadcast.create_track("video", None).expect("create track");
		// A second track with content ready for the subscribe opened after the GOAWAY.
		let audio = broadcast.create_track("audio", None).expect("create track");
		let mut audio_group = audio.append_group().expect("append group");
		audio_group
			.write_frame(moq_net::Timestamp::ZERO, b"audio".as_ref())
			.expect("write frame");
		audio_group.finish().expect("finish group");

		// Client consumes into its own origin.
		let sub_origin = produce_origin(Hop::random());

		let mut opts = MockConnectOptions::new(version);
		opts.server_publish = Some(pub_origin.consume());
		opts.client_subscribe = Some(sub_origin.clone());
		let MockPair { client, server, .. } = connect_mock(opts).await;

		// Subscribe BEFORE the GOAWAY and receive a first group.
		let sub = sub_origin.consume();
		sub.routed("test").await.expect("route announced");
		let bc = sub.request_broadcast("test", None).await.expect("broadcast resolves");
		let mut existing = bc.track("video").unwrap().subscribe(None).await.expect("subscribe");

		let mut group = track.append_group().expect("append group");
		group
			.write_frame(moq_net::Timestamp::ZERO, b"before".as_ref())
			.expect("write frame");
		group.finish().expect("finish group");

		let mut group_sub = existing
			.recv_group()
			.await
			.expect("recv_group")
			.expect("track closed prematurely");
		let frame = group_sub.read_frame().await.expect("read frame").expect("frame");
		assert_eq!(&frame.payload[..], b"before");

		// Server drains; the client observes the GOAWAY.
		server
			.drain()
			.send(moq_net::goaway::Goaway::redirect("https://elsewhere.example/"))
			.expect("send goaway");
		client.draining().recv().await.expect("goaway");
		assert!(client.draining().peek().is_some());

		// A NEW subscription opens on the draining session rather than failing:
		// with no replacement route yet, it is still the best path.
		let mut late = bc
			.track("audio")
			.unwrap()
			.subscribe(None)
			.await
			.expect("subscribe after GOAWAY");
		let mut group_sub = late
			.recv_group()
			.await
			.expect("recv_group")
			.expect("track closed prematurely");
		let frame = group_sub.read_frame().await.expect("read frame").expect("frame");
		assert_eq!(&frame.payload[..], b"audio");

		// The EXISTING subscription keeps flowing.
		let mut group = track.append_group().expect("append group");
		group
			.write_frame(moq_net::Timestamp::ZERO, b"after".as_ref())
			.expect("write frame");
		group.finish().expect("finish group");

		let mut group_sub = existing
			.recv_group()
			.await
			.expect("recv_group")
			.expect("track closed prematurely");
		let frame = group_sub.read_frame().await.expect("read frame").expect("frame");
		assert_eq!(
			&frame.payload[..],
			b"after",
			"existing subscription must keep flowing after GOAWAY"
		);
	})
	.await
	.expect("test timed out (likely a mock deadlock)");
}

#[moq_net_sim::test]
async fn goaway_keeps_new_subscribes_moq_lite_04() {
	goaway_keeps_new_subscribes("moq-lite-04".parse().unwrap()).await;
}

#[moq_net_sim::test]
async fn goaway_keeps_new_subscribes_moq_lite_05() {
	goaway_keeps_new_subscribes("moq-lite-05".parse().unwrap()).await;
}

#[moq_net_sim::test]
async fn goaway_keeps_new_subscribes_moq_transport_19() {
	goaway_keeps_new_subscribes("moq-transport-19".parse().unwrap()).await;
}

/// A GOAWAY costs the peer's routes at [`Cost::DRAIN`] so the origin stops
/// preferring a connection that is about to close.
///
/// The peer sends nothing after the GOAWAY, which is the case that matters: the
/// subscriber has to react to the signal itself, not to a later announce that a
/// draining peer has no reason to send.
async fn goaway_drains_routes(version: Version) {
	moq_net_sim::timeout(TEST_TIMEOUT, async {
		let pub_origin = produce_origin(Hop::random());
		let broadcast = pub_origin.create_broadcast("test").expect("create broadcast");
		broadcast.announce(moq_net::origin::Route::default()).expect("announce");

		let sub_origin = produce_origin(Hop::random());

		let mut opts = MockConnectOptions::new(version);
		opts.server_publish = Some(pub_origin.consume());
		opts.client_subscribe = Some(sub_origin.clone());
		let MockPair { client, server, .. } = connect_mock(opts).await;

		let sub = sub_origin.consume();
		let route = sub.routed("test").await.expect("route announced");
		assert_ne!(
			route.cost,
			moq_net::origin::Cost::DRAIN,
			"a healthy route must not start out draining"
		);

		server.drain().send(Goaway::new()).expect("send goaway");
		client.draining().recv().await.expect("goaway");

		// Nothing else is sent on the announce stream, so this only resolves if the
		// subscriber acted on the GOAWAY rather than on another message. The drain
		// re-prices the route in place, which arrives as another active update.
		let mut announced = sub.announced();
		loop {
			let (moq_net::announce::Event::Start(update) | moq_net::announce::Event::Update(update)) =
				announced.next().await.expect("update")
			else {
				continue;
			};
			if update.prefix.as_str() == "test" && update.route.cost == moq_net::origin::Cost::DRAIN {
				break;
			}
		}
	})
	.await
	.expect("test timed out (likely a route that never drained)");
}

#[moq_net_sim::test]
async fn goaway_drains_routes_moq_lite_04() {
	goaway_drains_routes("moq-lite-04".parse().unwrap()).await;
}

#[moq_net_sim::test]
async fn goaway_drains_routes_moq_transport_19() {
	goaway_drains_routes("moq-transport-19".parse().unwrap()).await;
}
