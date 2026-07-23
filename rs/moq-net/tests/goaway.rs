//! GOAWAY behavior tests over the in-memory mock transport.
//!
//! The mock delivers all queued bytes deterministically (no real QUIC or
//! network I/O), so these tests are reliable without sleeps: every wait is on
//! an observable event.

mod support;

use std::time::Duration;

use moq_net::{Origin, Version};
use support::harness::{MockConnectOptions, MockPair, connect_mock};
use support::mock::create_mock_session_pair;

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

/// Server drains with a redirect URI; the client observes it via
/// `Session::goaway()` and flips `is_going_away()`. Exercises every GOAWAY
/// channel in the version matrix, in both directions.
#[tokio::test]
async fn goaway_send_receive_all_versions() {
	for version in GOAWAY_VERSIONS {
		tokio::time::timeout(TEST_TIMEOUT, async {
			let version: Version = version.parse().unwrap();
			let pair = connect_mock(MockConnectOptions::new(version)).await;

			// Server -> client.
			let draining = pair
				.server
				.drain()
				.expect("drain must be available on GOAWAY versions")
				.start("https://new.example.com");

			let goaway = pair.client.goaway().await.expect("session closed before GOAWAY");
			assert_eq!(&*goaway.uri, "https://new.example.com", "version {version}");
			assert!(pair.client.is_going_away(), "version {version}");
			// No deadline was advertised.
			assert_eq!(goaway.timeout, None, "version {version}");

			// The drain claim is exclusive: a second drain on the same session is refused.
			assert!(pair.server.drain().is_none(), "version {version}");

			// Client leaves; the drain completes.
			drop(pair.client);
			draining.complete().await;
		})
		.await
		.unwrap_or_else(|_| panic!("test timed out on {version} (likely a mock deadlock)"));
	}
}

/// The IETF draft-17+ GOAWAY carries a timeout on the wire; the receiver
/// observes the sender's advertised deadline.
#[tokio::test]
async fn goaway_wire_timeout_moq_transport_17() {
	tokio::time::timeout(TEST_TIMEOUT, async {
		let version: Version = "moq-transport-17".parse().unwrap();
		let pair = connect_mock(MockConnectOptions::new(version)).await;

		eprintln!("CHECKPOINT: connected");
		let draining = pair
			.server
			.drain()
			.expect("drain")
			.start_with_timeout("moqt://relay.example/", Duration::from_secs(5));
		eprintln!("CHECKPOINT: drain started");

		let goaway = pair.client.goaway().await.expect("session closed before GOAWAY");
		eprintln!("CHECKPOINT: goaway observed");
		assert_eq!(&*goaway.uri, "moqt://relay.example/");
		assert_eq!(goaway.timeout, Some(Duration::from_secs(5)));

		drop(pair.client);
		draining.complete().await;
	})
	.await
	.expect("test timed out (likely a mock deadlock)");
}

/// The client also drains the server: GOAWAY is symmetric.
#[tokio::test]
async fn goaway_client_to_server_moq_lite_04() {
	tokio::time::timeout(TEST_TIMEOUT, async {
		let version: Version = "moq-lite-04".parse().unwrap();
		let pair = connect_mock(MockConnectOptions::new(version)).await;

		let draining = pair.client.drain().expect("drain").start("");

		let goaway = pair.server.goaway().await.expect("session closed before GOAWAY");
		assert_eq!(&*goaway.uri, "", "empty URI = reconnect to the same endpoint");
		assert!(pair.server.is_going_away());

		drop(pair.server);
		draining.complete().await;
	})
	.await
	.expect("test timed out (likely a mock deadlock)");
}

/// Versions without GOAWAY (moq-lite-03 and earlier) return `None` from
/// `drain()` instead of silently hanging a `Draining` nobody will serve.
#[tokio::test]
async fn drain_unavailable_moq_lite_03() {
	tokio::time::timeout(TEST_TIMEOUT, async {
		let version: Version = "moq-lite-03".parse().unwrap();
		let pair = connect_mock(MockConnectOptions::new(version)).await;

		assert!(!pair.server.version().has_goaway());
		assert!(pair.server.drain().is_none());
		assert!(pair.client.drain().is_none());
	})
	.await
	.expect("test timed out (likely a mock deadlock)");
}

/// Dropping an unstarted `Drain` releases the claim so a later drain can retry.
#[tokio::test]
async fn drain_claim_released_on_drop_moq_lite_04() {
	tokio::time::timeout(TEST_TIMEOUT, async {
		let version: Version = "moq-lite-04".parse().unwrap();
		let pair = connect_mock(MockConnectOptions::new(version)).await;

		let drain = pair.server.drain().expect("first claim");
		assert!(pair.server.drain().is_none(), "claim is exclusive");
		drop(drain);
		assert!(
			pair.server.drain().is_some(),
			"dropping an unstarted Drain releases the claim"
		);
	})
	.await
	.expect("test timed out (likely a mock deadlock)");
}

/// The draining side force-closes the session when the peer overstays the
/// deadline, and the peer observes the GOAWAY_TIMEOUT close code.
#[tokio::test]
async fn goaway_timeout_force_close_moq_transport_17() {
	tokio::time::timeout(TEST_TIMEOUT, async {
		let version: Version = "moq-transport-17".parse().unwrap();
		let pair = connect_mock(MockConnectOptions::new(version)).await;

		let draining = pair
			.server
			.drain()
			.expect("drain")
			.start_with_timeout("moqt://relay.example/", Duration::from_millis(100));

		// The client observes the GOAWAY but deliberately does NOT leave.
		let goaway = pair.client.goaway().await.expect("session closed before GOAWAY");
		assert_eq!(goaway.timeout, Some(Duration::from_millis(100)));

		// The deadline fires and the server force-closes with GoawayTimeout (33).
		draining.complete().await;
		let reason = pair.client.closed().await;
		assert!(
			reason.to_string().contains("goaway timeout"),
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
#[tokio::test]
async fn duplicate_goaway_keeps_first_payload_moq_lite_04() {
	/// Open a raw lite Goaway control stream:
	/// `[ControlType::Goaway][message size][uri length][uri bytes]`.
	/// Varints under 64 encode as a single byte, so the frame is hand-rolled.
	/// Returns the recv half so the caller can wait for the peer to fully
	/// process (and drop) the stream.
	async fn send_goaway_raw<S: web_transport_trait::Session>(session: &S, uri: &str) -> S::RecvStream {
		use web_transport_trait::SendStream as _;
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
	async fn wait_processed<R: web_transport_trait::RecvStream>(mut recv: R) {
		let mut buf = [0u8; 16];
		while let Ok(Some(_)) = recv.read(&mut buf).await {}
	}

	tokio::time::timeout(TEST_TIMEOUT, async {
		let version: Version = "moq-lite-04".parse().unwrap();

		let (client_transport, server_transport) = create_mock_session_pair(Some(version.alpn()));
		let server_raw = server_transport.clone();

		let client = moq_net::Client::new().with_versions(version.into());
		let server = moq_net::Server::new().with_versions(version.into());
		let (client_result, server_result) =
			tokio::join!(client.connect(client_transport), server.accept(server_transport));
		let (client_session, client_driver) = client_result.expect("client handshake failed");
		let (_server_session, server_driver) = server_result.expect("server handshake failed");
		tokio::spawn(client_driver);
		tokio::spawn(server_driver);

		// First GOAWAY: observed with its URI. Waiting for the peer to close the
		// stream guarantees the control message was fully processed.
		let recv_a = send_goaway_raw(&server_raw, "a").await;
		wait_processed(recv_a).await;
		let goaway = client_session.goaway().await.expect("session closed before GOAWAY");
		assert_eq!(&*goaway.uri, "a");
		assert!(client_session.is_going_away());

		// Second GOAWAY: once the client has fully processed the stream, the
		// observed payload must still carry the FIRST URI.
		let recv_b = send_goaway_raw(&server_raw, "bb").await;
		wait_processed(recv_b).await;

		let goaway = client_session.goaway().await.expect("session closed before GOAWAY");
		assert_eq!(&*goaway.uri, "a", "duplicate GOAWAY must not replace the first payload");
	})
	.await
	.expect("test timed out (likely a mock deadlock)");
}

/// After a received GOAWAY, new subscriptions are rejected with `GoingAway`
/// while an existing subscription keeps delivering groups.
#[tokio::test]
async fn goaway_gates_new_subscribes_moq_lite_04() {
	tokio::time::timeout(TEST_TIMEOUT, async {
		let version: Version = "moq-lite-04".parse().unwrap();

		// Server publishes a broadcast with one live track.
		let pub_origin = Origin::random().produce();
		let mut broadcast = pub_origin
			.create_broadcast("test", moq_net::broadcast::Route::new().with_announce(true))
			.expect("create broadcast");
		let mut track = broadcast.create_track("video", None).expect("create track");
		// A second track with content ready, so the gated subscribe below would
		// deliver immediately if it reached the wire.
		let mut audio = broadcast.create_track("audio", None).expect("create track");
		let mut audio_group = audio.append_group().expect("append group");
		audio_group
			.write_frame(moq_net::Timestamp::ZERO, b"audio".as_ref())
			.expect("write frame");
		audio_group.finish().expect("finish group");

		// Client consumes into its own origin.
		let sub_origin = Origin::random().produce();

		let mut opts = MockConnectOptions::new(version);
		opts.server_publish = Some(pub_origin.clone());
		opts.client_subscribe = Some(sub_origin.clone());
		let MockPair { client, server } = connect_mock(opts).await;

		// Subscribe BEFORE the GOAWAY and receive a first group.
		let bc = sub_origin
			.consume()
			.announced_broadcast("test")
			.await
			.expect("broadcast announced");
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
		let _draining = server.drain().expect("drain").start("https://elsewhere.example/");
		client.goaway().await.expect("goaway");
		assert!(client.is_going_away());

		// A NEW subscription must not reach the wire: the upstream open is gated
		// with GoingAway. In the resume model a failed route delivers a stall
		// (waiting for another route), not an error, so the observable behavior
		// is that a track with content ready produces nothing. The mock delivers
		// synchronously, so the bounded wait is generous.
		match bc.track("audio").unwrap().subscribe(None).await {
			Err(_) => {} // Rejected outright is also acceptable gating.
			Ok(mut gated) => {
				let delivered = tokio::time::timeout(Duration::from_millis(500), gated.recv_group()).await;
				assert!(delivered.is_err(), "new subscribe after GOAWAY must not deliver");
			}
		}

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
