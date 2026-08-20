//! The thread-per-core QUIC workers, against real sockets.
//!
//! Linux-only, because the mode is: every worker binds the listen address with
//! `SO_REUSEPORT`, and no other platform load-balances a unicast UDP port across
//! the group.
#![cfg(all(target_os = "linux", feature = "quinn"))]

use std::net::{SocketAddr, UdpSocket};

use moq_tokio::worker::{self, Workers};

const WORKERS: u16 = 4;

/// A UDP port nothing is bound to.
///
/// Every worker binds the same port, so this cannot be `:0`: each would pick an
/// ephemeral port of its own and they would not form a group.
fn free_udp_port() -> u16 {
	let probe = UdpSocket::bind("127.0.0.1:0").expect("bind probe");
	let port = probe.local_addr().expect("local addr").port();
	drop(probe);
	port
}

/// A self-signed certificate on disk. Workers refuse `tls.generate`, since each
/// would generate one of its own and serve a different identity.
fn certificate(dir: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
	let key = rcgen::KeyPair::generate().expect("keypair");
	let params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).expect("cert params");
	let cert = params.self_signed(&key).expect("self-signed cert");
	let cert_path = dir.join("cert.pem");
	let key_path = dir.join("key.pem");
	std::fs::write(&cert_path, cert.pem()).expect("write cert");
	std::fs::write(&key_path, key.serialize_pem()).expect("write key");
	(cert_path, key_path)
}

fn listen_config(cert: &std::path::Path, key: &std::path::Path, port: u16) -> moq_tokio::listen::Config {
	let mut config = moq_tokio::listen::Config::default();
	config.bind = Some(format!("127.0.0.1:{port}"));
	config.tls.cert = vec![cert.to_path_buf()];
	config.tls.key = vec![key.to_path_buf()];
	config
}

/// Pinning is off throughout: a CI container may restrict which cores it may run
/// on, and none of these tests are about placement.
fn config(count: u16) -> worker::Config {
	worker::Config::new(count).with_pin(false)
}

/// Workers that outlive their owner are a leak with teeth: the threads keep
/// accepting into nothing, and a replacement group joins the same reuseport
/// group as the orphans and loses a share of its traffic to them. Dropping them
/// has to actually release the port.
#[tokio::test]
async fn dropping_the_workers_releases_the_port() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	let dir = tempfile::tempdir().expect("tempdir");
	let (cert, key) = certificate(dir.path());
	let port = free_udp_port();

	let mut workers =
		Workers::bind(listen_config(&cert, &key, port), Default::default(), config(WORKERS)).expect("bind workers");
	assert_eq!(workers.len(), usize::from(WORKERS));

	// Serving first is the case that used to strand the threads.
	for (server, spawner) in workers.split() {
		spawner.run(async move {
			let _ = server.listen().await;
		});
	}
	workers.shutdown().await;

	// A plain bind refuses a port any socket still holds, reuseport or not, so this
	// succeeds only if every worker's socket is really gone.
	let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
	moq_tokio::bind::udp(moq_tokio::bind::Udp::new(addr)).expect("workers left the port bound");
}

/// Never splitting them has to release the port too, or a failure between bind
/// and serve strands the group. Dropping is the path `shutdown` is the async
/// alternative to, so both are covered.
#[tokio::test]
async fn dropping_unserved_workers_releases_the_port() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	let dir = tempfile::tempdir().expect("tempdir");
	let (cert, key) = certificate(dir.path());
	let port = free_udp_port();

	let workers =
		Workers::bind(listen_config(&cert, &key, port), Default::default(), config(WORKERS)).expect("bind workers");
	drop(workers);

	let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
	moq_tokio::bind::udp(moq_tokio::bind::Udp::new(addr)).expect("workers left the port bound");
}

/// An ephemeral bind gives every worker a port of its own instead of a shared
/// one, leaving all but the first unreachable behind an address that reads as
/// bound. That has to fail at startup rather than come up looking healthy.
#[tokio::test]
async fn an_ephemeral_port_is_refused() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	let dir = tempfile::tempdir().expect("tempdir");
	let (cert, key) = certificate(dir.path());

	let err = Workers::bind(listen_config(&cert, &key, 0), Default::default(), config(WORKERS))
		.expect_err("an ephemeral port cannot be shared");
	assert!(
		matches!(err, moq_tokio::Error::WorkerPortMismatch { .. }),
		"unexpected error: {err}"
	);
}

/// A bind that fails midway drops the members it already spawned, and the error
/// must not return before their sockets are closed: an owner that immediately
/// rebinds the address would otherwise join the half-dead reuseport group and be
/// renumbered when it finished dying.
#[tokio::test]
async fn a_failed_bind_releases_its_port_first() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	let dir = tempfile::tempdir().expect("tempdir");
	let (cert, key) = certificate(dir.path());

	// Ephemeral, so the first member binds a port of its own and the second
	// fails the group: a real partial construction, deterministically.
	let err = Workers::bind(listen_config(&cert, &key, 0), Default::default(), config(2))
		.expect_err("an ephemeral port cannot be shared");
	let moq_tokio::Error::WorkerPortMismatch { first, .. } = err else {
		panic!("unexpected error: {err}");
	};

	// A plain bind refuses a port any socket still holds, so this succeeds only
	// if the first member was joined before `bind` returned its error.
	moq_tokio::bind::udp(moq_tokio::bind::Udp::new(first)).expect("a failed bind left its port held");
}

/// One worker has no group to disagree with, so an ephemeral port is fine there.
#[tokio::test]
async fn a_single_worker_may_use_an_ephemeral_port() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	let dir = tempfile::tempdir().expect("tempdir");
	let (cert, key) = certificate(dir.path());

	let workers = Workers::bind(listen_config(&cert, &key, 0), Default::default(), config(1))
		.expect("a lone worker may take any port");
	assert_ne!(workers.local_addr().port(), 0);
}

/// The steering filter picks a member with one byte of the connection ID, so a
/// group past 256 has members it could never name. Refused at bind rather than
/// panicking later, when the first server-issued connection ID has no stride
/// left to spend.
#[tokio::test]
async fn an_unaddressable_group_is_refused() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	let dir = tempfile::tempdir().expect("tempdir");
	let (cert, key) = certificate(dir.path());

	let err = Workers::bind(listen_config(&cert, &key, 0), Default::default(), config(257))
		.expect_err("a group larger than the prefix can name");
	assert!(
		matches!(err, moq_tokio::Error::WorkerCount { count: 257, max: 256 }),
		"unexpected error: {err}"
	);
}

/// Generating a certificate would give every member a different identity, so it
/// is refused rather than silently served.
#[tokio::test]
async fn generated_certificates_are_refused() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	let mut listen = moq_tokio::listen::Config::default();
	listen.bind = Some(format!("127.0.0.1:{}", free_udp_port()));
	listen.tls.generate = vec!["localhost".to_string()];

	let err = Workers::bind(listen, Default::default(), config(WORKERS))
		.expect_err("generated certificates cannot be shared");
	assert!(
		matches!(err, moq_tokio::Error::WorkerTlsGenerate),
		"unexpected error: {err}"
	);
}
