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
	// Process-wide: another test in this binary may have installed it first.
	let _ = moq_tokio::crypto::install_default();
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
	config.internal.listen = Some("127.0.0.1:0".parse().unwrap());
	config.stats.interval = Some(Duration::from_secs(3600).into());
	config.drain_timeout = Some(Duration::ZERO.into());
	if io_uring {
		config.runtime.workers = Some(1);
		config.runtime.pin = false;
		config.runtime.io_uring = true;
	}
	let relay = Relay::load(config).await.unwrap().with_signals(false);
	let addr = relay.addr().unwrap();
	let metrics = format!("http://{}/metrics", relay.internal().addr().unwrap());
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
		let connection = kio::clock::timeout(TIMEOUT, client.with_reconnect(false).connect(url).established())
			.await
			.unwrap()
			.unwrap();
		let before = kio::clock::timeout(TIMEOUT, async {
			loop {
				let row = presence(&registry);
				if row.active() == 1
					&& row.wire_tx > 0
					&& row.wire_rx > 0
					&& registry.snapshot().control().iter().any(|(_, class, bytes)| {
						*class == moq_net::stats::Message::Announce && bytes.tx > 0 && bytes.rx > 0
					}) {
					break row;
				}
				tokio::task::yield_now().await;
			}
		})
		.await
		.expect("admitted session has direct wire counters");
		let body = reqwest::get(&metrics)
			.await
			.unwrap()
			.error_for_status()
			.unwrap()
			.text()
			.await
			.unwrap();
		for direction in ["tx", "rx"] {
			let prefix = format!("moq_relay_wire_{direction}_bytes_total{{tier=\"\"}} ");
			let total: u64 = body
				.lines()
				.find_map(|line| line.strip_prefix(&prefix))
				.expect("wire metric")
				.parse()
				.unwrap();
			assert!(total > 0, "live {scheme} transport must reach the ops endpoint: {body}");
		}
		for direction in ["tx", "rx"] {
			let prefix = format!("moq_relay_control_{direction}_bytes_total{{tier=\"\",class=\"announce\"}} ");
			let total: u64 = body
				.lines()
				.find_map(|line| line.strip_prefix(&prefix))
				.expect("control metric")
				.parse()
				.unwrap();
			assert!(total > 0, "{scheme}: {body}");
		}
		drop(connection);
		let after = kio::clock::timeout(TIMEOUT, async {
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
	kio::clock::timeout(TIMEOUT, running).await.unwrap().unwrap().unwrap();
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
		Err(moq_uring::Error::Unsupported(reason)) if std::env::var_os("CI_STRICT").is_none() => {
			eprintln!("skipping io_uring wire stats test: {reason}");
			return;
		}
		Err(error) => panic!("io_uring worker setup failed: {error}"),
	}
	wire_sources(true).await;
}
