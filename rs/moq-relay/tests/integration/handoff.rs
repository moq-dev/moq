//! Upgrading a relay in place: the new relay takes over the old one's sockets and
//! serves beside it while the old one drains.
//!
//! Linux-only, as handoff is: the QUIC port is handed on as a reuseport group,
//! steered by a socket filter.
#![cfg(all(target_os = "linux", feature = "_quic"))]

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::FutureExt;
use moq_net::stats::{Presence, Registry, Tier};
use moq_relay::{Config, Relay};

const TIMEOUT: Duration = Duration::from_secs(10);

/// A self-signed certificate on disk, which both relays load: workers refuse
/// `listen.tls.generate`.
fn certificate(dir: &Path) -> (PathBuf, PathBuf) {
	let key = rind_rcgen::KeyPair::generate(&rcgen::PKCS_ECDSA_P256_SHA256).expect("keypair");
	let mut params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).expect("cert params");
	params.serial_number = Some(1u64.into());
	let cert = params.self_signed(&key).expect("self-signed cert");
	let cert_path = dir.join("cert.pem");
	let key_path = dir.join("key.pem");
	std::fs::write(&cert_path, cert.pem()).expect("write cert");
	std::fs::write(
		&key_path,
		pem::encode(&pem::Pem::new("PRIVATE KEY", key.to_pkcs8().secret_pkcs8_der())),
	)
	.expect("write key");
	(cert_path, key_path)
}

/// The QUIC backend a relay serves on.
#[derive(Clone, Copy, Debug)]
enum Backend {
	Tokio,
	#[cfg(feature = "_uring")]
	Uring,
}

/// One config for every relay, as an upgrade restarts with the same file: each
/// listener on an ephemeral port, which a relay taking over reads as "whatever
/// the old one bound".
fn config(dir: &Path, cert: &Path, key: &Path, backend: Backend) -> Config {
	let mut config = Config::default();
	config.listen.bind = Some("127.0.0.1:0".parse().unwrap());
	config.listen.tls.cert = vec![cert.to_path_buf()];
	config.listen.tls.key = vec![key.to_path_buf()];
	config.listen.tcp.bind = Some("127.0.0.1:0".parse().unwrap());
	config.web.http.listen = Some("127.0.0.1:0".parse().unwrap());
	config.internal.listen = Some("127.0.0.1:0".parse().unwrap());
	config.runtime.workers = moq_relay::runtime::Workers::Count(std::num::NonZeroU16::new(2).unwrap());
	config.runtime.pin = false;
	config.auth.public = vec![moq_auth::Pattern::all()];
	config.stats.enabled = true;
	config.stats.node = Some("test".into());
	config.handoff.socket = Some(dir.join("handoff.sock"));
	match backend {
		Backend::Tokio => {}
		#[cfg(feature = "_uring")]
		Backend::Uring => config.runtime.io_uring = true,
	}
	config
}

/// The sessions of the relay behind `registry`.
fn presence(registry: &Registry) -> Presence {
	registry
		.snapshot()
		.sessions()
		.into_iter()
		.find(|(tier, _)| *tier == Tier::default())
		.map_or(Presence::default(), |(_, presence)| presence)
}

/// Sessions the relay behind `registry` serves now.
fn active(registry: &Registry) -> u64 {
	presence(registry).active()
}

/// Wait until the relay behind `registry` serves `count` sessions.
async fn serving(registry: &Registry, count: u64, what: &str) {
	kio::clock::timeout(TIMEOUT, async {
		while active(registry) != count {
			tokio::task::yield_now().await;
		}
	})
	.await
	.unwrap_or_else(|_| panic!("{what}: serving {} sessions, not {count}", active(registry)));
}

fn dial() -> moq_tokio::connect::Config {
	let mut config = moq_tokio::connect::Config::default();
	config.tls.insecure = Some(true);
	config.bind = Some("127.0.0.1:0".parse().unwrap());
	config
}

async fn established(client: moq_tokio::Client, url: &str) -> moq_tokio::Connection {
	kio::clock::timeout(TIMEOUT, client.connect(url.parse::<url::Url>().unwrap()).established())
		.await
		.expect("connect timeout")
		.expect("connect failed")
}

/// A session that leaves on the relay's GOAWAY.
async fn connect(url: &str) -> moq_tokio::Connection {
	established(dial().init(Default::default()).unwrap().with_reconnect(false), url).await
}

/// A session that stays through the relay's drain, as a cluster peer's does: never
/// healthy, so it reads the GOAWAY as a redial that reached the draining relay again.
async fn stays(url: &str, quic: moq_tokio::quic::Config) -> moq_tokio::Connection {
	let mut config = dial();
	config.backoff.healthy = Some(Duration::from_secs(600).into());
	established(config.init(quic).unwrap().with_restart(moq_tokio::Restart::Wait), url).await
}

/// Wait for the GOAWAY a draining relay sends `connection`.
async fn drained(connection: &moq_tokio::Connection) {
	let draining = connection.draining().unwrap();
	let goaway = kio::clock::timeout(TIMEOUT, kio::wait(|waiter| draining.poll(waiter)))
		.await
		.expect("the old relay never drained")
		.unwrap();
	assert_eq!(goaway.uri(), "");
}

#[tokio::test]
async fn a_new_relay_takes_over_while_the_old_one_drains() {
	takes_over(Backend::Tokio, Backend::Tokio).await;
}

#[cfg(feature = "_uring")]
#[tokio::test]
async fn an_io_uring_relay_takes_over() {
	if crate::runtime_uring::supported() {
		takes_over(Backend::Uring, Backend::Uring).await;
	}
}

/// Both backends hand on the same group, with connection IDs in the same layout.
#[cfg(feature = "_uring")]
#[tokio::test]
async fn a_relay_takes_over_across_backends() {
	if crate::runtime_uring::supported() {
		takes_over(Backend::Tokio, Backend::Uring).await;
	}
}

/// The new relay serves every listener the old one did, on the same sockets: new
/// sessions reach it while the old one's stay, until those leave and it exits.
/// Only then does the new relay publish the node's stats, and only then may a
/// third relay take over.
async fn takes_over(from: Backend, to: Backend) {
	kio::clock::host_time(); // real relays on real sockets
	moq_tokio::crypto::install_default().unwrap();
	let dir = tempfile::tempdir().unwrap();
	let (cert, key) = certificate(dir.path());

	let old = Relay::load(config(dir.path(), &cert, &key, from))
		.await
		.unwrap()
		.with_signals(false);
	let addr = old.addr().unwrap();
	let tcp = old.tcp_addr().unwrap();
	let http = old.web_addrs().http.unwrap();
	let internal = old.internal().addr().unwrap();
	let old_stats = old.cluster().stats.clone();
	// Holds the old relay's drain open, as a session that stays to the deadline would:
	// a client leaves on GOAWAY at once.
	let staying = old.shutdown().admission().enter().unwrap();
	let ready = old.ready();
	let old_running = tokio::spawn(old.run());
	ready.wait().await.unwrap();

	let quic = format!("https://{addr}/handoff");
	let held = connect(&quic).await;
	serving(&old_stats, 1, "the old relay").await;

	let new = Relay::load(config(dir.path(), &cert, &key, to))
		.await
		.unwrap()
		.with_signals(false);
	assert_eq!(new.addr(), Some(addr));
	assert_eq!(new.tcp_addr(), Some(tcp));
	assert_eq!(new.web_addrs().http, Some(http));
	assert_eq!(new.internal().addr(), Some(internal));
	let new_stats = new.cluster().stats.clone();
	let new_origin = new.cluster().origin.consume();
	let trigger = new.shutdown_trigger().clone();
	let ready = new.ready();
	let new_running = tokio::spawn(new.run());
	ready.wait().await.unwrap();

	// The old relay drains once the new one serves.
	drained(&held).await;

	// New sessions, by either transport, reach the new relay.
	let fresh = connect(&quic).await;
	let stream = connect(&format!("tcp://{tcp}/handoff")).await;
	serving(&new_stats, 2, "the new relay").await;
	assert_eq!(
		presence(&old_stats).sessions_started,
		1,
		"a new session reached the draining relay"
	);

	// The group has room for two generations, both in use.
	let refused = Relay::load(config(dir.path(), &cert, &key, to))
		.await
		.err()
		.expect("a third relay took over while the old one served");
	assert!(format!("{refused:#}").contains("refused"), "{refused:#}");

	// One instance of the node's stats at a time: the old relay's, so far.
	let mut stats = new_origin.follow(".stats/node/test", None).unwrap();
	assert!(
		stats.available().now_or_never().is_none(),
		"published the node's stats beside the old relay"
	);

	drop((held, staying));
	kio::clock::timeout(TIMEOUT, old_running)
		.await
		.expect("the old relay outlived its last session")
		.unwrap()
		.unwrap();
	kio::clock::timeout(TIMEOUT, stats.available())
		.await
		.expect("the node's stats stayed held once the old relay exited")
		.unwrap()
		.unwrap();

	// Every handshake reaches the new relay now, and a third may take over.
	let last = connect(&quic).await;
	serving(&new_stats, 3, "the new relay alone").await;

	trigger.start();
	drop((fresh, stream, last));
	kio::clock::timeout(TIMEOUT, new_running)
		.await
		.expect("the new relay outlived its sessions")
		.unwrap()
		.unwrap();
}

/// A new relay that dies while the old one drains takes none of the old one's
/// sessions with it: they still reach the old relay, and the next relay takes over
/// from it on the same sockets.
#[tokio::test]
async fn the_old_relay_outlives_a_new_one_that_dies() {
	kio::clock::host_time(); // real relays on real sockets
	moq_tokio::crypto::install_default().unwrap();
	let dir = tempfile::tempdir().unwrap();
	let (cert, key) = certificate(dir.path());
	// Neither an idle session nor the drain deadline ends a session within the test's
	// timeout, so only a close that reaches the old relay ends one there.
	let mut quic = moq_tokio::quic::Config::default();
	quic.idle_timeout = Some(Duration::from_secs(600).into());
	let config = || {
		let mut config = config(dir.path(), &cert, &key, Backend::Tokio);
		config.quic = quic.clone();
		config.drain_timeout = Some(Duration::from_secs(600).into());
		config
	};

	let old = Relay::load(config()).await.unwrap().with_signals(false);
	let addr = old.addr().unwrap();
	let tcp = old.tcp_addr().unwrap();
	let old_stats = old.cluster().stats.clone();
	let staying = old.shutdown().admission().enter().unwrap();
	let ready = old.ready();
	let old_running = tokio::spawn(old.run());
	ready.wait().await.unwrap();

	let quic_url = format!("https://{addr}/handoff");
	let first = stays(&quic_url, quic.clone()).await;
	let second = stays(&quic_url, quic.clone()).await;
	serving(&old_stats, 2, "the old relay").await;

	let new = Relay::load(config()).await.unwrap().with_signals(false);
	let ready = new.ready();
	let new_running = tokio::spawn(new.run());
	ready.wait().await.unwrap();
	drained(&first).await;

	// The new relay dies, and every socket it held goes with it.
	new_running.abort();
	assert!(new_running.await.unwrap_err().is_cancelled());

	// A session of the old relay still reaches it: its close arrives there.
	drop(first);
	serving(&old_stats, 1, "the old relay after the new one died").await;

	// The next relay takes over from the old one, on the same sockets.
	let next = Relay::load(config())
		.await
		.expect("the old relay refused the next one")
		.with_signals(false);
	assert_eq!(next.addr(), Some(addr));
	assert_eq!(next.tcp_addr(), Some(tcp));
	let next_stats = next.cluster().stats.clone();
	let trigger = next.shutdown_trigger().clone();
	let ready = next.ready();
	let next_running = tokio::spawn(next.run());
	ready.wait().await.unwrap();
	let fresh = connect(&quic_url).await;
	let stream = connect(&format!("tcp://{tcp}/handoff")).await;
	serving(&next_stats, 2, "the next relay").await;

	drop((second, staying));
	kio::clock::timeout(TIMEOUT, old_running)
		.await
		.expect("the old relay outlived its last session")
		.unwrap()
		.unwrap();

	trigger.start();
	drop((fresh, stream));
	kio::clock::timeout(TIMEOUT, next_running)
		.await
		.expect("the next relay outlived its sessions")
		.unwrap()
		.unwrap();
}

/// Start `moq-relay --handoff-supervise` with its config in the environment, which
/// is all it reads: the test's own would reach it too.
fn stand_in(dir: &Path, cert: &Path, key: &Path, notify: &Path) -> tokio::process::Child {
	tokio::process::Command::new(env!("CARGO_BIN_EXE_moq-relay"))
		.arg("--handoff-supervise")
		.env_clear()
		.env("MOQ_LISTEN", "127.0.0.1:0")
		.env("MOQ_LISTEN_TLS_CERT", cert)
		.env("MOQ_LISTEN_TLS_KEY", key)
		.env("MOQ_RUNTIME_WORKERS", "1")
		.env("MOQ_AUTH_PUBLIC", "**")
		.env("MOQ_HANDOFF_SOCKET", dir.join("handoff.sock"))
		.env("NOTIFY_SOCKET", notify)
		.kill_on_drop(true)
		.spawn()
		.expect("start the stand-in")
}

/// Wait for a READY notification on `notify`, as a service manager does.
async fn ready(notify: &tokio::net::UnixDatagram) {
	let mut message = [0; 1024];
	kio::clock::timeout(TIMEOUT, async {
		loop {
			let len = notify.recv(&mut message).await.unwrap();
			if std::str::from_utf8(&message[..len])
				.unwrap()
				.lines()
				.any(|line| line == "READY=1")
			{
				return;
			}
		}
	})
	.await
	.expect("no READY reached the service manager");
}

/// The processes whose parent is `parent`.
fn children(parent: u32) -> Vec<libc::pid_t> {
	std::fs::read_dir("/proc")
		.unwrap()
		.filter_map(|entry| {
			let pid: libc::pid_t = entry.ok()?.file_name().to_str()?.parse().ok()?;
			let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
			// The command name is parenthesized and may hold anything; the parent's pid
			// is the second field after it.
			let rest = stat.get(stat.rfind(')')? + 2..)?;
			let ppid: u32 = rest.split(' ').nth(1)?.parse().ok()?;
			(ppid == parent).then_some(pid)
		})
		.collect()
}

async fn exited(child: &mut tokio::process::Child, what: &str) -> std::process::ExitStatus {
	kio::clock::timeout(TIMEOUT, child.wait())
		.await
		.unwrap_or_else(|_| panic!("{what} never exited"))
		.unwrap()
}

/// What a service manager does across a restart with `KillMode=process` and
/// `RestartKillSignal=SIGUSR2`: the stand-in exits on SIGUSR2 and leaves its relay
/// serving, the next stand-in's relay takes over and reports ready through it, and
/// the old relay exits once drained. A stop's SIGTERM drains the relay and exits.
#[tokio::test]
async fn a_restart_starts_the_new_relay_beside_the_old() {
	kio::clock::host_time(); // real relays in real processes
	// Adopt the relay a stand-in leaves behind, as the service manager does, so its
	// exit is this test's to wait on.
	// SAFETY: prctl with these arguments has no memory-safety preconditions.
	assert_eq!(unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) }, 0);

	let dir = tempfile::tempdir().unwrap();
	let (cert, key) = certificate(dir.path());
	let notify_path = dir.path().join("notify");
	let notify = tokio::net::UnixDatagram::bind(&notify_path).unwrap();

	let mut old = stand_in(dir.path(), &cert, &key, &notify_path);
	ready(&notify).await;
	let relays = children(old.id().unwrap());
	assert_eq!(relays.len(), 1, "the stand-in runs one relay: {relays:?}");
	let relay = relays[0];

	// The restart's stop: the stand-in leaves, its relay stays.
	// SAFETY: kill has no memory-safety preconditions.
	assert_eq!(
		unsafe { libc::kill(old.id().unwrap() as libc::pid_t, libc::SIGUSR2) },
		0
	);
	let status = exited(&mut old, "the old stand-in").await;
	assert_eq!(status.code(), Some(0), "{status}");
	let mut status = 0;
	// SAFETY: waitpid writes only to `status`.
	let reaped = unsafe { libc::waitpid(relay, &mut status, libc::WNOHANG) };
	assert_eq!(reaped, 0, "the old relay exited with its stand-in");

	// The restart's start: the new relay takes over, and the old one drains.
	let mut new = stand_in(dir.path(), &cert, &key, &notify_path);
	ready(&notify).await;
	let status = tokio::task::spawn_blocking(move || {
		let mut status = 0;
		// SAFETY: waitpid writes only to `status`.
		let reaped = unsafe { libc::waitpid(relay, &mut status, 0) };
		assert_eq!(reaped, relay);
		status
	});
	let status = kio::clock::timeout(TIMEOUT, status)
		.await
		.expect("the old relay kept serving after the new one took over")
		.unwrap();
	assert!(
		libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
		"the old relay exited with {status:#x}"
	);

	// A stop drains the relay, and the stand-in exits with it.
	// SAFETY: kill has no memory-safety preconditions.
	assert_eq!(
		unsafe { libc::kill(new.id().unwrap() as libc::pid_t, libc::SIGTERM) },
		0
	);
	let status = exited(&mut new, "the new stand-in").await;
	assert_eq!(status.code(), Some(0), "{status}");

	// SIGUSR1, a heap profile's signal, reaches the relay rather than ending the
	// stand-in. Built without jemalloc, the relay dies of it, and the stand-in exits
	// as a shell reports that.
	#[cfg(not(feature = "jemalloc"))]
	{
		let mut last = stand_in(dir.path(), &cert, &key, &notify_path);
		ready(&notify).await;
		// SAFETY: kill has no memory-safety preconditions.
		assert_eq!(
			unsafe { libc::kill(last.id().unwrap() as libc::pid_t, libc::SIGUSR1) },
			0
		);
		let status = exited(&mut last, "the last stand-in").await;
		assert_eq!(status.code(), Some(128 + libc::SIGUSR1), "{status}");
	}

	// Each stand-in removed the socket its relay notified it on.
	let left: Vec<_> = std::fs::read_dir(dir.path())
		.unwrap()
		.map(|entry| entry.unwrap().file_name())
		.filter(|name| name.to_string_lossy().ends_with(".notify"))
		.collect();
	assert!(left.is_empty(), "{left:?}");
}
