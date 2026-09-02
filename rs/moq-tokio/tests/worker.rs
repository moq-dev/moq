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
		spawner.run(|| async move {
			let _ = server.listen().await;
		});
	}
	workers.shutdown().await;

	// A plain bind refuses a port any socket still holds, reuseport or not, so this
	// succeeds only if every worker's socket is really gone.
	let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
	moq_tokio::bind::udp(moq_tokio::bind::Udp::new(addr)).expect("workers left the port bound");
}

/// The future factory runs on the worker, so the future may hold local state
/// across an await without making that state thread-safe, and spawn more of it
/// onto the same task set.
#[tokio::test]
async fn spawner_runs_a_send_less_future() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	let dir = tempfile::tempdir().expect("tempdir");
	let (cert, key) = certificate(dir.path());
	let mut workers = Workers::bind(listen_config(&cert, &key, 0), Default::default(), config(1)).expect("bind worker");

	let task = {
		let mut split = workers.split();
		let (_server, spawner) = split.pop().expect("one worker");
		spawner.run(|| async move {
			let value = std::rc::Rc::new(std::cell::Cell::new(1));
			tokio::task::yield_now().await;
			value.set(value.get() + 1);

			// A nested `!Send` task, which panics outside a local task set.
			let shared = value.clone();
			tokio::task::spawn_local(async move { shared.set(shared.get() + 1) })
				.await
				.expect("local spawn");

			value.get()
		})
	};

	assert_eq!(task.await.expect("local task"), 3);
	workers.shutdown().await;
}

/// A factory that panics while building its future takes the task down, not the
/// worker thread: the group keeps running and the next future still starts.
#[tokio::test]
async fn spawner_contains_a_factory_panic() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	let dir = tempfile::tempdir().expect("tempdir");
	let (cert, key) = certificate(dir.path());
	let mut workers = Workers::bind(listen_config(&cert, &key, 0), Default::default(), config(1)).expect("bind worker");

	let (panicked, survived) = {
		let mut split = workers.split();
		let (_server, spawner) = split.pop().expect("one worker");
		let panicked = spawner.run(|| -> std::future::Pending<()> { panic!("factory") });
		let survived = spawner.run(|| async { "still here" });
		(panicked, survived)
	};

	assert!(panicked.await.expect_err("factory panic").is_panic());
	assert_eq!(survived.await.expect("worker survived"), "still here");
	workers.shutdown().await;
}

/// Aborting the returned handle stops the worker-local future, rather than
/// detaching it to run unreachable until the group stops.
#[tokio::test]
async fn spawner_abort_reaches_the_worker() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	let dir = tempfile::tempdir().expect("tempdir");
	let (cert, key) = certificate(dir.path());
	let mut workers = Workers::bind(listen_config(&cert, &key, 0), Default::default(), config(1)).expect("bind worker");

	// Dropped when the future is, so it reports the cancellation from the worker.
	let (dropped, was_dropped) = tokio::sync::oneshot::channel::<()>();
	let (started, has_started) = tokio::sync::oneshot::channel::<()>();

	let task = {
		let mut split = workers.split();
		let (_server, spawner) = split.pop().expect("one worker");
		spawner.run(move || async move {
			let _dropped = dropped;
			let _ = started.send(());
			std::future::pending::<()>().await;
		})
	};

	has_started.await.expect("future started");
	task.abort();

	let waited = tokio::time::timeout(std::time::Duration::from_secs(5), was_dropped).await;
	assert!(waited.expect("abort reached the worker").is_err());
	workers.shutdown().await;
}

/// The same, but aborting before the factory has even reached its worker. The
/// task is owned from the moment it is spawned, so no handoff window detaches
/// it: a caller that aborts while the handle is still in flight cancels it.
#[tokio::test]
async fn spawner_abort_races_the_handoff() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	let dir = tempfile::tempdir().expect("tempdir");
	let (cert, key) = certificate(dir.path());
	let mut workers = Workers::bind(listen_config(&cert, &key, 0), Default::default(), config(1)).expect("bind worker");

	// Dropped with the future, whether or not it was ever polled.
	let (dropped, was_dropped) = tokio::sync::oneshot::channel::<()>();

	let task = {
		let mut split = workers.split();
		let (_server, spawner) = split.pop().expect("one worker");
		spawner.run(move || async move {
			let _dropped = dropped;
			std::future::pending::<()>().await;
		})
	};

	// No wait: the closure may still be in the channel, or its handle buffered in
	// the oneshot the forwarding task has yet to read.
	task.abort();

	let waited = tokio::time::timeout(std::time::Duration::from_secs(5), was_dropped).await;
	assert!(waited.expect("abort reached the worker").is_err());
	workers.shutdown().await;
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

/// `SO_REUSEPORT` groups by address and UID, so a second group on a served
/// address would silently join the first, as two relays overlapping in a rolling
/// restart do: the old group's filter keeps steering every packet to the old
/// process while the new one reports ready and serves nothing. The group locks
/// its address for its lifetime, which turns the overlap back into the loud
/// startup failure it is without workers.
#[tokio::test]
async fn an_occupied_port_is_refused() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	let dir = tempfile::tempdir().expect("tempdir");
	let (cert, key) = certificate(dir.path());
	let port = free_udp_port();

	let workers =
		Workers::bind(listen_config(&cert, &key, port), Default::default(), config(WORKERS)).expect("bind workers");

	let err = Workers::bind(listen_config(&cert, &key, port), Default::default(), config(WORKERS))
		.expect_err("a second group must not join the first");
	assert!(
		matches!(err, moq_tokio::Error::WorkerOverlap { .. }),
		"unexpected error: {err}"
	);

	// The lock and the probe must be gone with the group: a second group takes
	// the same address cleanly once the first shuts down.
	workers.shutdown().await;
	let again = Workers::bind(listen_config(&cert, &key, port), Default::default(), config(WORKERS))
		.expect("the released address must be bindable again");
	drop(again);
}

/// A dual-stack `[::]` group and a `0.0.0.0` group overlap on IPv4, so both
/// spellings must share one lock: constructing them concurrently under
/// different locks would silently strip the dual-stack group's IPv4 side. The
/// lock has to catch this, not the probe, so the error is asserted exactly.
#[tokio::test]
async fn the_other_wildcard_spelling_is_refused() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	let dir = tempfile::tempdir().expect("tempdir");
	let (cert, key) = certificate(dir.path());
	let port = free_udp_port();

	let mut v4 = listen_config(&cert, &key, port);
	v4.bind = Some(format!("0.0.0.0:{port}"));
	let workers = Workers::bind(v4, Default::default(), config(WORKERS)).expect("bind v4 wildcard workers");

	let mut v6 = listen_config(&cert, &key, port);
	v6.bind = Some(format!("[::]:{port}"));
	let err = Workers::bind(v6, Default::default(), config(WORKERS))
		.expect_err("the overlapping wildcard spelling must be refused");
	assert!(
		matches!(err, moq_tokio::Error::WorkerOverlap { .. }),
		"unexpected error: {err}"
	);

	drop(workers);
}

/// The lock is keyed by port alone, so a group on a *different* address sharing
/// the port is refused too. Deliberate over-exclusion: wildcard and specific
/// addresses overlap in ways a per-address lock cannot serialize, and this
/// failure is loud and names the port, while the failure it prevents was
/// silent traffic loss. Asserted exactly so the lock, not the probe, catches it
/// (the probe would let two specific addresses coexist).
#[tokio::test]
async fn a_shared_port_is_refused_across_addresses() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	let dir = tempfile::tempdir().expect("tempdir");
	let (cert, key) = certificate(dir.path());
	let port = free_udp_port();

	let workers =
		Workers::bind(listen_config(&cert, &key, port), Default::default(), config(WORKERS)).expect("bind workers");

	// A distinct loopback address: no bind conflict with 127.0.0.1, only the
	// shared port.
	let mut other = listen_config(&cert, &key, port);
	other.bind = Some(format!("127.0.0.2:{port}"));
	let err = Workers::bind(other, Default::default(), config(WORKERS))
		.expect_err("a second group sharing the port must be refused");
	assert!(
		matches!(err, moq_tokio::Error::WorkerOverlap { .. }),
		"unexpected error: {err}"
	);

	drop(workers);
}

/// The lifetime lock only excludes groups that take it, so a reuseport member
/// bound by something else entirely, a relay predating the lock or an unrelated
/// same-UID process, is caught by the first member's plain-bind probe instead:
/// a plain bind refuses a port any socket holds, reuseport or not.
#[tokio::test]
async fn a_foreign_reuseport_group_is_refused() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	let dir = tempfile::tempdir().expect("tempdir");
	let (cert, key) = certificate(dir.path());

	// A reuseport socket bound outside any worker group, holding no lock.
	let foreign = moq_tokio::bind::udp(moq_tokio::bind::Udp::new("127.0.0.1:0".parse().unwrap()).with_reuse_port(true))
		.expect("bind foreign reuseport socket");
	let port = foreign.local_addr().expect("local addr").port();

	Workers::bind(listen_config(&cert, &key, port), Default::default(), config(WORKERS))
		.expect_err("a group must not join a foreign reuseport member");
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
