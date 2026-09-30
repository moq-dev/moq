//! A cluster notices a silent peer within `--cluster-idle-timeout`, not the 30s
//! `--quic-idle-timeout` its shared client was built with.
#![cfg(feature = "noq")]

use std::time::{Duration, Instant};

use moq_relay::cluster::{self, Peer};

/// The cluster setting under test, far below the client's 30s QUIC default.
const IDLE_TIMEOUT: Duration = Duration::from_secs(1);

/// How long the cluster may take to drop the dead peer's route. Generous over
/// [`IDLE_TIMEOUT`] for a slow runner, and well under 30s, so a dial that ignored
/// the cluster setting fails here.
const DETECT: Duration = Duration::from_secs(10);

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

/// A peer announcing one broadcast, on its own runtime so dropping the runtime
/// silences it the way a crash or a partition does: no CONNECTION_CLOSE, so only
/// the idle timeout can tell the cluster it is gone.
async fn spawn_peer() -> (u16, tokio::runtime::Runtime) {
	let runtime = tokio::runtime::Builder::new_multi_thread()
		.worker_threads(1)
		.enable_all()
		.build()
		.expect("build peer runtime");

	let (bound, port) = tokio::sync::oneshot::channel();
	runtime.spawn(async move {
		let origin = moq_tokio::origin::spawn();
		let broadcast = origin.create_broadcast("peer").expect("create broadcast");
		broadcast.announce(Default::default()).expect("announce");

		let mut config = moq_tokio::listen::Config::default();
		config.bind = Some("127.0.0.1:0".parse().expect("parse bind"));
		config.tls.generate = vec!["localhost".into()];
		let mut listener = config
			.init(Default::default())
			.expect("peer init")
			.listen()
			.await
			.expect("peer listen");
		let _ = bound.send(listener.local_addr().expect("peer addr").port());

		while let Some(request) = listener.accept().await {
			let origin = origin.clone();
			tokio::spawn(async move {
				if let Ok(session) = request.with_publisher(&origin).ok().await {
					let _ = session.closed().await;
				}
			});
		}
		drop(broadcast);
	});

	let port = tokio::time::timeout(DETECT, port)
		.await
		.expect("peer startup timed out")
		.expect("peer failed to start");
	(port, runtime)
}

#[test]
fn silent_peer_detected_within_cluster_idle_timeout() {
	run_cluster_test(silent_peer_detected());
}

async fn silent_peer_detected() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	let (port, peer) = spawn_peer().await;

	// The client keeps the 30s `--quic-*` default; only the cluster setting is short.
	let mut connect = moq_tokio::connect::Config::default();
	connect.bind = Some("127.0.0.1:0".parse().expect("parse bind"));
	connect.tls.insecure = Some(true);
	#[cfg(feature = "websocket")]
	{
		connect.websocket.enabled = Some(false);
	}
	let client = connect.init(Default::default()).expect("client init");

	let mut config = cluster::Config::default();
	config.connect = vec![Peer::new(format!("https://127.0.0.1:{port}/"))];
	config.idle_timeout = Some(IDLE_TIMEOUT);
	let cluster = cluster::Cluster::new(cluster::Options::new(config))
		.expect("cluster init")
		.with_client(client);
	let mut announced = cluster.origin.consume().announced();
	let run = tokio::spawn(cluster.clone().start().await.expect("cluster start").run());

	tokio::time::timeout(DETECT, async {
		loop {
			let update = announced.next().await.expect("origin closed");
			if update.kind.is_active() && update.prefix.as_str() == "peer" {
				break;
			}
		}
	})
	.await
	.expect("the peer's broadcast never arrived");

	tokio::task::spawn_blocking(move || peer.shutdown_timeout(Duration::from_secs(1)))
		.await
		.expect("peer shutdown panicked");
	let silenced = Instant::now();

	tokio::time::timeout(DETECT, async {
		loop {
			let update = announced.next().await.expect("origin closed");
			if !update.kind.is_active() && update.prefix.as_str() == "peer" {
				break;
			}
		}
	})
	.await
	.expect("the silent peer's route outlived the cluster idle timeout");
	println!("silent peer dropped after {:?}", silenced.elapsed());

	run.abort();
}
