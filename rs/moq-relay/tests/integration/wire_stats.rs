//! Transport wire counters across the shared-runtime and io_uring listener boundary.
#![cfg(feature = "_quic")]

use moq_net::stats::{Presence, Registry, Tier};
use moq_relay::{Config, Relay};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);

fn presence(registry: &Registry) -> Presence {
	registry
		.snapshot()
		.sessions()
		.into_iter()
		.find(|(tier, _)| *tier == Tier::default())
		.map_or(Presence::default(), |(_, value)| value)
}

async fn wire_sources(io_uring: bool) {
	moq_tokio::crypto::install_default().unwrap();
	let dir = tempfile::tempdir().unwrap();
	let key = rcgen::KeyPair::generate().unwrap();
	let cert = rcgen::CertificateParams::new(vec!["localhost".into()])
		.unwrap()
		.self_signed(&key)
		.unwrap();
	let cert_path = dir.path().join("cert.pem");
	let key_path = dir.path().join("key.pem");
	std::fs::write(&cert_path, cert.pem()).unwrap();
	std::fs::write(&key_path, key.serialize_pem()).unwrap();
	let mut config = Config::default();
	config.listen.bind = Some("127.0.0.1:0".parse().unwrap());
	config.listen.tls.cert = vec![cert_path];
	config.listen.tls.key = vec![key_path];
	config.auth.public = vec![moq_auth::Pattern::all()];
	config.stats.enabled = true;
	config.stats.interval = 3600;
	config.drain_timeout = Duration::ZERO;
	if io_uring {
		config.runtime.workers = Some(1);
		config.runtime.pin = false;
		config.runtime.io_uring = true;
	}
	let relay = Relay::load(config).await.unwrap().with_signals(false);
	let addr = relay.addr().unwrap();
	let registry = relay.cluster().stats.clone();
	let trigger = relay.shutdown_trigger().clone();
	let running = tokio::spawn(relay.run());
	// Both adapters must expose UDP totals without asking Session::stats(),
	// which would arm a periodic sampler. Snapshot reads the source directly.
	for scheme in ["moql", "https"] {
		let mut client = moq_tokio::connect::Config::default();
		client.tls.insecure = Some(true);
		client.bind = Some("127.0.0.1:0".parse().unwrap());
		let client = client.init(Default::default()).unwrap();
		let url: url::Url = format!("{scheme}://{addr}/wire").parse().unwrap();
		let connection = tokio::time::timeout(TIMEOUT, client.with_reconnect(false).connect(url).established())
			.await
			.unwrap()
			.unwrap();
		let before = tokio::time::timeout(TIMEOUT, async {
			loop {
				let row = presence(&registry);
				if row.active() == 1 && row.wire_tx > 0 && row.wire_rx > 0 {
					break row;
				}
				tokio::task::yield_now().await;
			}
		})
		.await
		.expect("admitted session has direct wire counters");
		drop(connection);
		let after = tokio::time::timeout(TIMEOUT, async {
			loop {
				let row = presence(&registry);
				if row.active() == 0 {
					break row;
				}
				tokio::task::yield_now().await;
			}
		})
		.await
		.expect("dropping the client closes its stats context");
		assert!(after.wire_tx >= before.wire_tx && after.wire_rx >= before.wire_rx);
		assert_eq!(presence(&registry), after, "a closed source is unregistered");
	}
	trigger.start();
	tokio::time::timeout(TIMEOUT, running).await.unwrap().unwrap().unwrap();
}

#[tokio::test]
async fn native_sessions_supply_direct_wire_counters() {
	wire_sources(false).await;
}

#[cfg(all(target_os = "linux", feature = "_uring"))]
#[tokio::test]
async fn uring_sessions_supply_direct_wire_counters() {
	match moq_uring::Worker::new(Default::default()) {
		Ok(_) => (),
		Err(moq_uring::Error::Unsupported(reason)) if std::env::var_os("MOQ_STRICT").is_none() => {
			eprintln!("skipping io_uring wire stats test: {reason}");
			return;
		}
		Err(error) => panic!("io_uring worker setup failed: {error}"),
	}
	wire_sources(true).await;
}
