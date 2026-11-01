//! A capped grant over QUIC is admitted and holds its session to its caps, and every
//! session holds only the SETUP window until it is admitted, on the tokio listener and
//! the io_uring workers alike.
//!
//! Over real sockets, so the caps are measured on the wall clock as lower bounds on
//! how long a transfer takes: uncapped, the same transfer takes milliseconds.
#![cfg(feature = "quic")]

use std::time::Duration;

use moq_relay::{Config, Relay};
use moq_tokio::moq_net;
use moq_tokio::quic::driver;

const TIMEOUT: Duration = Duration::from_secs(10);

/// One megabyte a second, so a fresh cap allows a one-megabyte burst.
const RATE: u64 = 8_000_000;
/// Past the burst and the SETUP window, so the rest waits on the rate.
const FRAME: usize = 2 << 20;
/// The least time [`FRAME`] can take under [`RATE`]: what is left after a full burst and
/// the SETUP window, at the rate, with some slack for the burst a cap starts with.
const LEAST: Duration = Duration::from_millis(700);

fn config(dir: &std::path::Path) -> Config {
	let mut config = super::peer_limits::config(dir);
	config.quic.max_streams = None;
	config
}

/// A grant of everything, with `publish` and `subscribe` caps.
fn grant(publish: Option<u64>, subscribe: Option<u64>) -> moq_auth::Grant {
	let all: moq_auth::Patterns = moq_auth::Pattern::all().into();
	let mut grant = moq_auth::Grant::new(all.clone(), all);
	grant.publish_rate = publish.map(|bps| moq_auth::Rate::from_bps(bps).unwrap());
	grant.subscribe_rate = subscribe.map(|bps| moq_auth::Rate::from_bps(bps).unwrap());
	grant
}

fn client() -> moq_tokio::Client {
	let mut config = moq_tokio::connect::Config::default();
	config.tls.insecure = Some(true);
	config.bind = Some("127.0.0.1:0".parse().expect("parse bind"));
	config
		.init(Default::default())
		.expect("client init")
		.with_reconnect(false)
}

/// Connect `client`, admitted on `grant`.
async fn connect(
	client: moq_tokio::Client,
	port: u16,
	admissions: &mut moq_relay::auth::Admissions,
	grant: moq_auth::Grant,
) -> moq_tokio::Connection {
	let url: url::Url = format!("https://localhost:{port}/room").parse().unwrap();
	let connecting = client.connect(url).established();
	let admitting = async {
		let admission = admissions.next().await.expect("admission");
		admission.grant(moq_auth::lease::Consumer::fixed(grant));
	};
	let (connection, ()) = kio::clock::timeout(TIMEOUT, async { tokio::join!(connecting, admitting) })
		.await
		.expect("connect timeout");
	connection.expect("connect failed")
}

/// Publish one [`FRAME`] on `test/video`, and how long a subscriber takes to read it.
async fn transfer(
	port: u16,
	admissions: &mut moq_relay::auth::Admissions,
	publisher: moq_auth::Grant,
	subscriber: moq_auth::Grant,
) -> Duration {
	let origin = moq_tokio::origin::spawn();
	let broadcast = origin.create_broadcast("test").expect("create broadcast");
	broadcast.announce(Default::default()).expect("announce");
	let track = broadcast
		.create_track(
			"video",
			moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI),
		)
		.expect("create track");
	let _publishing = connect(client().with_publisher(&origin), port, admissions, publisher).await;

	let subscribing = moq_tokio::origin::spawn();
	let consumer = subscribing.consume();
	let _subscribed = connect(client().with_subscriber(subscribing), port, admissions, subscriber).await;
	let broadcast_consumer = kio::clock::timeout(TIMEOUT, consumer.request("test", None))
		.await
		.expect("request timeout")
		.expect("announced broadcast resolves");
	let mut subscription = broadcast_consumer
		.track("video")
		.unwrap()
		.subscribe(None)
		.await
		.expect("subscribe");

	let start = kio::clock::now();
	let mut group = track.append_group().expect("append group");
	group
		.write_frame(moq_net::Timestamp::ZERO, vec![7u8; FRAME])
		.expect("write frame");
	group.finish().expect("finish group");
	let mut group = kio::clock::timeout(TIMEOUT, subscription.recv_group())
		.await
		.expect("recv_group timeout")
		.expect("recv_group failed")
		.expect("track closed");
	let frame = kio::clock::timeout(TIMEOUT, group.read_frame())
		.await
		.expect("read_frame timeout")
		.expect("read_frame failed")
		.expect("group closed");
	assert_eq!(frame.payload.len(), FRAME);
	kio::clock::now() - start
}

/// A capped publisher feeds the relay at its rate, and a capped subscriber is fed at its.
async fn held_to_its_caps(mut relay: Relay, port: u16) {
	let mut admissions = relay.admissions().expect("embedded auth");
	let running = tokio::spawn(relay.run());

	let elapsed = transfer(port, &mut admissions, grant(Some(RATE), None), grant(None, None)).await;
	assert!(elapsed >= LEAST, "a capped publisher sent {FRAME} bytes in {elapsed:?}");

	let elapsed = transfer(port, &mut admissions, grant(None, None), grant(None, Some(RATE))).await;
	assert!(
		elapsed >= LEAST,
		"a capped subscriber received {FRAME} bytes in {elapsed:?}"
	);

	running.abort();
	let _ = running.await;
}

/// Before admission the relay credits only the SETUP window, so a client cannot bank
/// the full one; once admitted, the window opens.
async fn window_opens_once_admitted(mut relay: Relay, port: u16) {
	let mut admissions = relay.admissions().expect("embedded auth");
	let running = tokio::spawn(relay.run());

	let mut connect = moq_tokio::connect::Config::default();
	connect.tls.insecure = Some(true);
	let mut tls = connect.tls.build().expect("tls config");
	tls.alpn_protocols = vec![b"moq-lite-06".to_vec()];
	let crypto = driver::crypto::rustls::QuicClientConfig::try_from(tls).expect("quic crypto");
	let endpoint =
		driver::endpoint::Endpoint::new(Default::default(), std::net::UdpSocket::bind("127.0.0.1:0").unwrap())
			.expect("endpoint");
	let conn = endpoint
		.connect(
			&driver::client::Config::new(std::sync::Arc::new(crypto)),
			([127, 0, 0, 1], port).into(),
			"localhost",
		)
		.expect("connect")
		.await
		.expect("handshake");

	// SETUP type, then a one-byte message holding zero parameters.
	let mut setup = conn.open_uni(Default::default()).await.expect("open");
	setup.write_all(&[0x01, 0x01, 0x00][..]).await.expect("write");
	setup.finish();
	let admission = kio::clock::timeout(TIMEOUT, admissions.next())
		.await
		.expect("admission timeout")
		.expect("admission");

	// Unread until the session runs, so it holds its credit, and the write waits for more.
	let banked = 1 << 20;
	let mut stream = conn.open_uni(Default::default()).await.expect("open");
	let writing = tokio::spawn(async move {
		stream.write_all(&vec![0u8; banked][..]).await.expect("write");
		stream
	});
	let blocked = async {
		while conn.stats().frame_tx.data_blocked == 0 {
			kio::clock::sleep(Duration::from_millis(10)).await;
		}
	};
	kio::clock::timeout(TIMEOUT, blocked)
		.await
		.expect("the client was never blocked on the SETUP window");
	let stats = conn.stats();
	assert!(
		stats.udp_tx.bytes < moq_net::transport::SETUP_RECEIVE_WINDOW + (64 << 10),
		"sent {} bytes before admission",
		stats.udp_tx.bytes
	);

	// Nothing reads the blocked stream before admission, so only opening the window
	// credits more. The session then stops that stream, so watch the credit, not the data.
	let credited = stats.frame_rx.max_data;
	admission.grant(moq_auth::lease::Consumer::fixed(grant(None, None)));
	let opened = async {
		while conn.stats().frame_rx.max_data == credited {
			kio::clock::sleep(Duration::from_millis(10)).await;
		}
	};
	kio::clock::timeout(TIMEOUT, opened)
		.await
		.expect("the window never opened once admitted");
	writing.abort();

	conn.close(driver::VarInt::from_u32(0), "done");
	endpoint.close(driver::VarInt::from_u32(0), "done").await;
	running.abort();
	let _ = running.await;
}

async fn tokio_relay() -> (Relay, u16, tempfile::TempDir) {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	let dir = tempfile::tempdir().expect("tempdir");
	let relay = Relay::load(config(dir.path()))
		.await
		.expect("load relay")
		.with_signals(false);
	let port = relay.quic_addr().expect("quic listener bound").port();
	(relay, port, dir)
}

#[tokio::test]
async fn tokio_session_is_held_to_its_caps() {
	let (relay, port, _dir) = tokio_relay().await;
	held_to_its_caps(relay, port).await;
}

#[tokio::test]
async fn tokio_window_opens_once_admitted() {
	let (relay, port, _dir) = tokio_relay().await;
	window_opens_once_admitted(relay, port).await;
}

#[cfg(all(target_os = "linux", feature = "_uring"))]
async fn uring_relay() -> Option<(Relay, u16, tempfile::TempDir)> {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	match moq_uring::Worker::new(Default::default()) {
		Ok(_) => {}
		Err(moq_uring::Error::Unsupported(reason)) if std::env::var_os("CI_STRICT").is_none() => {
			eprintln!("skipping io_uring relay test: {reason}");
			return None;
		}
		Err(err) => panic!("io_uring worker setup failed: {err}"),
	}
	let dir = tempfile::tempdir().expect("tempdir");
	let mut config = config(dir.path());
	config.runtime.workers = Some(1);
	config.runtime.pin = false;
	config.runtime.io_uring = true;
	let relay = Relay::load(config).await.expect("load relay").with_signals(false);
	let port = relay.addr().expect("workers bound an address").port();
	Some((relay, port, dir))
}

#[cfg(all(target_os = "linux", feature = "_uring"))]
#[tokio::test]
async fn uring_session_is_held_to_its_caps() {
	if let Some((relay, port, _dir)) = uring_relay().await {
		held_to_its_caps(relay, port).await;
	}
}

#[cfg(all(target_os = "linux", feature = "_uring"))]
#[tokio::test]
async fn uring_window_opens_once_admitted() {
	if let Some((relay, port, _dir)) = uring_relay().await {
		window_opens_once_admitted(relay, port).await;
	}
}
