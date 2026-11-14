//! Every integration test, linked into one binary.
//!
//! Each `tests/*.rs` file is its own crate that links this package and its
//! dependencies again, so add new integration tests here as a module.
//! `drills.rs` stays apart so each drill sensitivity mutation rebuilds only
//! the drills.
//! Relay fixtures opt out of process signals with `with_signals(false)`: only
//! `shutdown_signal` owns those signals, which reach every listener in this process.

mod auth_lifetime;
mod bitrate_caps;
mod cluster_close;
#[cfg(feature = "test-support")]
mod cluster_drain;
mod cluster_fresh_port;
mod cluster_idle;
#[cfg(feature = "test-support")]
mod cluster_unknown;
#[cfg(feature = "test-support")]
mod connection;
#[cfg(feature = "test-support")]
mod drain;
mod embed;
#[cfg(feature = "test-support")]
mod goaway_cluster;
mod hidden_cluster;
mod implementation;
mod lan_mesh;
mod logs;
mod memory;
mod peer_limits;
mod public_rules;
#[cfg(all(feature = "test-support", feature = "quic"))]
mod quic_sim;
mod refusals;
mod released_cli;
mod runtime_uring;
mod runtime_workers;
mod session_revalidate;
mod shutdown_signal;
mod smoke;
#[cfg(feature = "test-support")]
mod socket_auth;
mod stack;
mod tenant_matrix;
mod upstream_links;
mod wire_stats;

/// A certificate handle for a web stack, which only reads it to publish fingerprints.
fn certificates() -> moq_tokio::tls::Certificates {
	let key = rind_rcgen::KeyPair::generate(&rcgen::PKCS_ECDSA_P256_SHA256).expect("keypair");
	let mut params = rcgen::CertificateParams::new(vec!["localhost".into()]).expect("cert params");
	params.serial_number = Some(1u64.into());
	let cert = params.self_signed(&key).expect("self-signed cert");
	moq_tokio::tls::Certificates::from_pem(cert.pem().as_bytes()).expect("certificates")
}

/// Make the test runtime's clock one that only the test moves, wall time included, so
/// the relay's timers fire only on `advance` while its sockets stay real. Call it before
/// anything reads the clock.
fn manual() -> kio::clock::Installed {
	kio::clock::Manual::new().install(&tokio::runtime::Handle::current())
}

/// Build a cluster and spawn its driver, the way an embedding edge runs one. A
/// failed driver panics its task.
fn spawn_cluster(options: moq_relay::cluster::Options) -> moq_relay::cluster::Cluster {
	let (cluster, driver) = moq_relay::cluster::Cluster::new(options).expect("cluster init");
	tokio::spawn(async move {
		if let Err(err) = driver.run().await {
			panic!("cluster driver failed: {err:#}");
		}
	});
	cluster
}
