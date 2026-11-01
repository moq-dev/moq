//! A cluster sends each dialed peer its close before the runtime stops, so the
//! peer drops the link and its routes at once instead of at its idle timeout.
//!
//! Each dialer runs on its own runtime, shut down right after, as a process exit
//! would: a dropped link only queues its close, which nothing sends then.
#![cfg(feature = "quic")]

use std::time::Duration;

use moq_tokio::quic::driver;

use crate::peer_limits::{BARE_IDLE, bare_server, config};

/// Far short of [`BARE_IDLE`] and the dial's idle timeout, so only a close explains
/// the link ending in time.
const TIMEOUT: Duration = Duration::from_secs(10);

fn dialer_runtime() -> tokio::runtime::Runtime {
	tokio::runtime::Builder::new_multi_thread()
		.worker_threads(1)
		.enable_all()
		.build()
		.expect("dialer runtime")
}

fn dial_template() -> (moq_tokio::connect::Config, moq_tokio::quic::Config) {
	let mut connect = moq_tokio::connect::Config::default();
	connect.tls.insecure = Some(true);
	let mut quic = moq_tokio::quic::Config::default();
	quic.idle_timeout = Some((BARE_IDLE).into());
	(connect, quic)
}

fn peer_url(server: &driver::endpoint::Endpoint) -> String {
	let port = server.local_addr().port();
	format!("moql://127.0.0.1:{port}/")
}

async fn accept(server: &driver::endpoint::Endpoint) -> driver::connection::Connection {
	kio::clock::timeout(TIMEOUT, server.accept())
		.await
		.expect("dial timeout")
		.expect("incoming")
		.accept()
		.expect("accept")
		.await
		.expect("handshake")
}

/// The link ends with the dialer's own shutdown close, not a timeout or a SETUP error.
async fn assert_closed_by_shutdown(conn: driver::connection::Connection) {
	let err = kio::clock::timeout(TIMEOUT, conn.closed())
		.await
		.expect("the peer heard no close before its idle timeout");
	match err {
		driver::connection::Error::ApplicationClosed(close) => assert_eq!(close.reason, "client shutdown"),
		other => panic!("the link ended with {other:?}, not the shutdown close"),
	}
}

#[tokio::test]
async fn closing_a_cluster_closes_its_static_dials() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	let (connect, quic) = dial_template();
	let server = bare_server(&connect);

	let mut options = moq_relay::cluster::Options::new(Default::default());
	options.config.connect = vec![moq_relay::cluster::Peer::new(peer_url(&server))];
	let runtime = dialer_runtime();
	let cluster = runtime
		.spawn(async move {
			let cluster = moq_relay::cluster::Cluster::new(options)
				.expect("cluster init")
				.with_connect(connect, quic);
			let started = cluster.clone().start().await.expect("cluster start");
			tokio::spawn(started.run());
			cluster
		})
		.await
		.expect("cluster task");

	let conn = accept(&server).await;
	runtime
		.spawn(async move { cluster.close().await })
		.await
		.expect("close");
	runtime.shutdown_background();

	assert_closed_by_shutdown(conn).await;
}

/// `Client::close` reaches only QUIC, so a WebSocket link ends only because the
/// close stops its dial task. The runtime keeps running, as an embedder's would.
#[cfg(feature = "websocket")]
#[tokio::test]
async fn closing_a_cluster_ends_its_websocket_dials() {
	use tokio::io::AsyncReadExt;

	let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
	let port = listener.local_addr().expect("addr").port();
	let mut options = moq_relay::cluster::Options::new(Default::default());
	options.config.connect = vec![moq_relay::cluster::Peer::new(format!("ws://127.0.0.1:{port}/"))];
	let (connect, quic) = dial_template();
	let cluster = moq_relay::cluster::Cluster::new(options)
		.expect("cluster init")
		.with_connect(connect, quic);
	let started = cluster.clone().start().await.expect("cluster start");
	let running = tokio::spawn(started.run());

	// Never answers the upgrade, so the dial holds the socket until stopped.
	let (mut socket, _) = kio::clock::timeout(TIMEOUT, listener.accept())
		.await
		.expect("dial timeout")
		.expect("accept");
	kio::clock::timeout(TIMEOUT, cluster.close())
		.await
		.expect("close waits on a stopped dial");

	let mut buf = vec![0; 4096];
	loop {
		// Brief: the socket is already closed once `close` returns.
		let read = kio::clock::timeout(Duration::from_secs(2), socket.read(&mut buf))
			.await
			.expect("the dial still holds its socket");
		if read.map_or(true, |n| n == 0) {
			break;
		}
	}
	running.abort();
}

#[tokio::test]
async fn a_relay_shutdown_closes_its_cluster_dials() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	let (connect, quic) = dial_template();
	let server = bare_server(&connect);

	let dir = tempfile::tempdir().expect("tempdir");
	let mut config = config(dir.path());
	config.connect = connect;
	config.quic.idle_timeout = quic.idle_timeout;
	config.cluster.connect = vec![moq_relay::cluster::Peer::new(peer_url(&server))];
	config.auth.public = vec![moq_auth::Pattern::all()];

	let runtime = dialer_runtime();
	let (trigger, running) = runtime
		.spawn(async move {
			let relay = moq_relay::Relay::load(config)
				.await
				.expect("load relay")
				.with_signals(false);
			let trigger = relay.shutdown_trigger().clone();
			(trigger, tokio::spawn(relay.run()))
		})
		.await
		.expect("relay task");

	let conn = accept(&server).await;
	trigger.start();
	kio::clock::timeout(TIMEOUT, running)
		.await
		.expect("relay drain")
		.expect("relay task")
		.expect("relay run");
	runtime.shutdown_background();

	assert_closed_by_shutdown(conn).await;
}
