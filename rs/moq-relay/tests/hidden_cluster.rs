//! A cluster peer that predates the hidden opt-in still discovers the relay's
//! `.`-named broadcasts, so a mixed-version mesh keeps `.internal/origins`.

use std::net::TcpListener;
use std::time::Duration;

use moq_relay::cluster::{self, Peer};

const TEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Multi-transport client and session types make the test future large; give it a
/// big stack, as `goaway_cluster.rs` does.
fn run_cluster_test<F>(fut: F)
where
	F: std::future::Future<Output = ()> + Send + 'static,
{
	std::thread::Builder::new()
		.stack_size(32 * 1024 * 1024)
		.spawn(move || {
			tokio::runtime::Builder::new_current_thread()
				.enable_all()
				.build()
				.expect("build test runtime")
				.block_on(fut);
		})
		.expect("spawn test thread")
		.join()
		.expect("test thread panicked");
}

/// A stream-only moq server on a free loopback TCP port, speaking only `version`.
fn bind_free_tcp_server(version: moq_net::Version) -> (u16, moq_tokio::Server) {
	for _ in 0..20 {
		let probe = TcpListener::bind("127.0.0.1:0").expect("bind probe");
		let port = probe.local_addr().expect("local addr").port();
		drop(probe);

		let mut config = moq_tokio::listen::Config::default();
		config.tcp.bind = Some(format!("127.0.0.1:{port}").parse().expect("parse addr"));
		config.version = vec![version];
		if let Ok(server) = config.init(Default::default()) {
			return (port, server);
		}
	}
	panic!("could not bind a free TCP port after 20 attempts");
}

#[test]
fn old_peers_keep_hidden_paths() {
	for version in ["moq-lite-05", "moq-lite-06", "moq-transport-17"] {
		run_cluster_test(old_peer_keeps_hidden_paths(version.parse().unwrap()));
	}
}

/// The relay dials a peer pinned to `version`. The peer's announce request cannot
/// carry the opt-in below lite-07, yet it discovers the relay's hidden broadcast.
async fn old_peer_keeps_hidden_paths(version: moq_net::Version) {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	tokio::time::timeout(TEST_TIMEOUT, async {
		let (port, server) = bind_free_tcp_server(version);
		let peer_origin = moq_tokio::origin::spawn();
		let mut discovered = peer_origin.consume().with_hidden(true).announced();
		let accept = tokio::spawn(async move {
			let mut server = server.listen().await.expect("listen");
			let request = server.accept().await.expect("cluster dial");
			let session = request.with_subscriber(peer_origin).ok().await.expect("accept");
			session.closed().await;
		});

		let mut client_config = moq_tokio::connect::Config::default();
		client_config.tls.insecure = Some(true);
		let client = client_config.init(Default::default()).expect("client init");
		let mut cluster_config = cluster::Config::default();
		cluster_config.connect = vec![Peer::new(format!("tcp://127.0.0.1:{port}/"))];
		let cluster = cluster::Cluster::new(cluster::Options::new(cluster_config))
			.expect("cluster init")
			.with_client(client);
		let node = cluster
			.origin
			.create_broadcast(".internal/origins/test")
			.expect("create hidden");
		node.announce(Default::default()).expect("announce hidden");
		let cluster_run = tokio::spawn(cluster.clone().start().await.expect("cluster start").run());

		loop {
			let update = discovered.next().await.expect("peer origin closed");
			if update.kind.is_active() && update.prefix.as_str() == ".internal/origins/test" {
				break;
			}
		}

		cluster_run.abort();
		accept.abort();
	})
	.await
	.unwrap_or_else(|_| panic!("{version} peer never discovered the hidden path"));
}
