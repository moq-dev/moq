//! Upgrading a relay in place: the new relay takes over the old one's sockets and
//! serves beside it while the old one drains.
//!
//! Linux-only, as handoff is: the QUIC port is handed on as a reuseport group,
//! steered by a socket filter.
#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::FutureExt;
use moq_net::stats::{Presence, Registry, Tier};
use moq_relay::{Config, Relay};

/// Out of reach of any test: a stall on a loaded host must not end a session or a
/// drain the test expects to outlast it. Each wait below follows an event instead of
/// a deadline, and nextest's slow-timeout bounds a real hang.
const NEVER: Duration = Duration::from_secs(600);

/// QUIC whose idle timeout no stall reaches.
fn quic() -> moq_tokio::quic::Config {
	let mut quic = moq_tokio::quic::Config::default();
	quic.idle_timeout = Some(NEVER.into());
	quic
}

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
	#[cfg(uring)]
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
	config.auth.public = vec![moq_auth::Prefix::all()];
	config.stats.enabled = true;
	config.stats.node = Some("test".into());
	config.handoff.socket = Some(dir.join("handoff.sock"));
	config.quic = quic();
	config.drain_timeout = Some(NEVER.into());
	match backend {
		Backend::Tokio => {}
		#[cfg(uring)]
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
///
/// The registry has no change notification, so this polls; a sleep lets the runtime
/// park between polls instead of spinning a core the relays need on a loaded host.
async fn serving(registry: &Registry, count: u64) {
	while active(registry) != count {
		kio::clock::sleep(Duration::from_millis(5)).await;
	}
}

fn dial() -> moq_tokio::connect::Config {
	let mut config = moq_tokio::connect::Config::default();
	config.tls.insecure = Some(true);
	config.bind = Some("127.0.0.1:0".parse().unwrap());
	config
}

async fn established(client: moq_tokio::Client, url: &str) -> moq_tokio::Connection {
	client
		.connect(url.parse::<url::Url>().unwrap())
		.established()
		.await
		.expect("connect failed")
}

/// A session that leaves on the relay's GOAWAY.
async fn connect(url: &str) -> moq_tokio::Connection {
	established(dial().init(quic()).unwrap().with_reconnect(false), url).await
}

/// A session that ignores the relay's GOAWAY and stays through its drain until the relay
/// closes it.
///
/// Under [`moq_tokio::Restart::Handoff`] a GOAWAY that drains a session before it is
/// healthy changes nothing, and this healthy bar outlasts the test.
async fn stays(url: &str) -> moq_tokio::Connection {
	let mut config = dial();
	config.backoff.healthy = Some(Duration::from_secs(3600).into());
	established(
		config.init(quic()).unwrap().with_restart(moq_tokio::Restart::Handoff),
		url,
	)
	.await
}

/// Wait for the GOAWAY a draining relay sends `connection`.
async fn drained(connection: &moq_tokio::Connection) {
	let draining = connection.draining().unwrap();
	let goaway = kio::wait(|waiter| draining.poll(waiter)).await.unwrap();
	assert_eq!(goaway.uri(), "");
}

#[tokio::test]
async fn a_new_relay_takes_over_while_the_old_one_drains() {
	takes_over(Backend::Tokio, Backend::Tokio).await;
}

#[cfg(uring)]
#[tokio::test]
async fn an_io_uring_relay_takes_over() {
	if crate::runtime_uring::supported() {
		takes_over(Backend::Uring, Backend::Uring).await;
	}
}

/// Both backends hand on the same group, with connection IDs in the same layout.
#[cfg(uring)]
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
	serving(&old_stats, 1).await;

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
	serving(&new_stats, 2).await;
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
	old_running.await.unwrap().unwrap();
	// The node's stats, held until the old relay exited.
	stats.available().await.unwrap().unwrap();

	// Every handshake reaches the new relay now, and a third may take over.
	let last = connect(&quic).await;
	serving(&new_stats, 3).await;

	trigger.start();
	drop((fresh, stream, last));
	new_running.await.unwrap().unwrap();
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
	// Neither an idle session nor the drain deadline ends a session (see `NEVER`), so
	// only a close that reaches the old relay ends one there.
	let config = || config(dir.path(), &cert, &key, Backend::Tokio);

	let old = Relay::load(config()).await.unwrap().with_signals(false);
	let addr = old.addr().unwrap();
	let tcp = old.tcp_addr().unwrap();
	let old_stats = old.cluster().stats.clone();
	let staying = old.shutdown().admission().enter().unwrap();
	let ready = old.ready();
	let old_running = tokio::spawn(old.run());
	ready.wait().await.unwrap();

	let quic_url = format!("https://{addr}/handoff");
	let first = stays(&quic_url).await;
	let second = stays(&quic_url).await;
	serving(&old_stats, 2).await;

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
	serving(&old_stats, 1).await;

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
	serving(&next_stats, 2).await;

	drop((second, staying));
	old_running.await.unwrap().unwrap();

	trigger.start();
	drop((fresh, stream));
	next_running.await.unwrap().unwrap();
}

/// A stand-in that takes its relay with it when dropped, such as on a failed
/// assertion; a relay it already left is [`Reap`]'s.
struct StandIn(tokio::process::Child);

impl Drop for StandIn {
	fn drop(&mut self) {
		if let Some(pid) = self.0.id() {
			for relay in children(pid) {
				// SAFETY: kill has no memory-safety preconditions.
				unsafe { libc::kill(relay, libc::SIGKILL) };
			}
		}
	}
}

impl std::ops::Deref for StandIn {
	type Target = tokio::process::Child;
	fn deref(&self) -> &Self::Target {
		&self.0
	}
}

impl std::ops::DerefMut for StandIn {
	fn deref_mut(&mut self) -> &mut Self::Target {
		&mut self.0
	}
}

/// A relay a stand-in left to this test, killed if the test ends before reaping it.
struct Reap(Option<libc::pid_t>);

impl Reap {
	/// Wait for the relay to exit, reaping it so the guard never signals a reused pid.
	async fn exited(&mut self) -> libc::c_int {
		let relay = self.0.expect("reaped already");
		let status = tokio::task::spawn_blocking(move || {
			let mut status = 0;
			// SAFETY: waitpid writes only to `status`.
			let reaped = unsafe { libc::waitpid(relay, &mut status, 0) };
			assert_eq!(reaped, relay);
			status
		})
		.await
		.unwrap();
		self.0 = None;
		status
	}
}

impl Drop for Reap {
	fn drop(&mut self) {
		if let Some(relay) = self.0 {
			// SAFETY: kill has no memory-safety preconditions; the pid is not yet reaped,
			// so it is still the relay's.
			unsafe { libc::kill(relay, libc::SIGKILL) };
		}
	}
}

/// Start `moq-relay --handoff-supervise` with its config in the environment, which
/// is all it reads: the test's own would reach it too.
fn stand_in(dir: &Path, cert: &Path, key: &Path, notify: &Path) -> StandIn {
	stand_in_draining(dir, cert, key, notify, &format!("{}s", NEVER.as_secs()))
}

/// [`stand_in`], with a `drain_timeout` of `drain`.
fn stand_in_draining(dir: &Path, cert: &Path, key: &Path, notify: &Path, drain: &str) -> StandIn {
	let child = tokio::process::Command::new(env!("CARGO_BIN_EXE_moq-relay"))
		.arg("--handoff-supervise")
		.env_clear()
		.env("MOQ_LISTEN", "127.0.0.1:0")
		.env("MOQ_LISTEN_TLS_CERT", cert)
		.env("MOQ_LISTEN_TLS_KEY", key)
		.env("MOQ_RUNTIME_WORKERS", "1")
		.env("MOQ_AUTH_PUBLIC", "")
		.env("MOQ_HANDOFF_SOCKET", dir.join("handoff.sock"))
		.env("NOTIFY_SOCKET", notify)
		.env("MOQ_DRAIN_TIMEOUT", drain)
		.kill_on_drop(true)
		.spawn()
		.expect("start the stand-in");
	StandIn(child)
}

/// A restart whose next relay never takes over, such as one that fails to start,
/// leaves the old relay outside the service: it drains once `drain_timeout`
/// passes with no successor, rather than holding the port forever.
#[tokio::test]
async fn a_relay_no_successor_takes_over_from_drains() {
	kio::clock::host_time(); // real relays in real processes
	// SAFETY: prctl with these arguments has no memory-safety preconditions.
	assert_eq!(unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) }, 0);

	let dir = tempfile::tempdir().unwrap();
	let (cert, key) = certificate(dir.path());
	let notify_path = dir.path().join("notify");
	let notify = tokio::net::UnixDatagram::bind(&notify_path).unwrap();

	let mut stand_in = stand_in_draining(dir.path(), &cert, &key, &notify_path, "1s");
	ready(&notify, &mut stand_in).await;
	let relays = children(stand_in.id().unwrap());
	assert_eq!(relays.len(), 1, "the stand-in runs one relay: {relays:?}");
	let mut left = Reap(Some(relays[0]));

	let started = kio::clock::now();
	// SAFETY: kill has no memory-safety preconditions.
	assert_eq!(
		unsafe { libc::kill(stand_in.id().unwrap() as libc::pid_t, libc::SIGUSR2) },
		0
	);
	let status = stand_in.wait().await.unwrap();
	assert_eq!(status.code(), Some(0), "{status}");

	let status = left.exited().await;
	assert!(
		libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
		"the relay exited with {status:#x}"
	);
	assert!(
		started.elapsed() >= Duration::from_secs(1),
		"drained before its window for a successor passed"
	);
}

/// Start a relay process with its config in the environment, serving QUIC on `port`
/// with a shared reset key, and sessions that outlast the test unless closed.
fn relay(dir: &Path, cert: &Path, key: &Path, port: u16, notify: &Path) -> tokio::process::Child {
	tokio::process::Command::new(env!("CARGO_BIN_EXE_moq-relay"))
		.env_clear()
		.env("MOQ_LISTEN", format!("127.0.0.1:{port}"))
		.env("MOQ_LISTEN_TLS_CERT", cert)
		.env("MOQ_LISTEN_TLS_KEY", key)
		.env("MOQ_LISTEN_RESET_SECRET", dir.join("reset.key"))
		.env("MOQ_QUIC_IDLE_TIMEOUT", format!("{}s", NEVER.as_secs()))
		.env("MOQ_DRAIN_TIMEOUT", format!("{}s", NEVER.as_secs()))
		.env("MOQ_RUNTIME_WORKERS", "2")
		.env("MOQ_AUTH_PUBLIC", "")
		.env("MOQ_HANDOFF_SOCKET", dir.join("handoff.sock"))
		.env("NOTIFY_SOCKET", notify)
		.kill_on_drop(true)
		.spawn()
		.expect("start the relay")
}

/// A new relay killed while the old one drains leaves its clients to the old one,
/// which takes their packets back and, sharing the reset key, answers them with a
/// stateless reset, so they reconnect rather than wait out their idle timeout.
#[tokio::test]
async fn a_killed_relay_s_clients_are_reset_by_the_one_it_replaced() {
	kio::clock::host_time(); // real relays in real processes
	moq_tokio::crypto::install_default().unwrap();
	let dir = tempfile::tempdir().unwrap();
	let (cert, key) = certificate(dir.path());
	std::fs::write(dir.path().join("reset.key"), "ab".repeat(32)).unwrap();
	let notify_path = dir.path().join("notify");
	let notify = tokio::net::UnixDatagram::bind(&notify_path).unwrap();
	let port = std::net::UdpSocket::bind("127.0.0.1:0")
		.unwrap()
		.local_addr()
		.unwrap()
		.port();
	let url = format!("https://127.0.0.1:{port}/handoff");
	let mut quic = quic();
	// Keeps sending, so a reset comes back at once.
	quic.keep_alive = Some(Duration::from_millis(200).into());

	let mut old = relay(dir.path(), &cert, &key, port, &notify_path);
	ready(&notify, &mut old).await;
	// Holds the old relay in its drain once the new one takes over.
	let _kept = stays(&url).await;

	let mut new = relay(dir.path(), &cert, &key, port, &notify_path);
	ready(&notify, &mut new).await;
	let orphan = established(dial().init(quic).unwrap(), &url).await;
	// Past the default healthy bar, so the reset reconnects rather than ending the loop.
	kio::clock::sleep(Duration::from_secs(1)).await;

	new.kill().await.unwrap();
	// Reset, rather than left to its idle timeout, which no test reaches.
	let monitor = orphan.monitor();
	let mut seen = monitor.state();
	while !matches!(seen.phase, moq_tokio::connection::Phase::Disconnected { .. }) {
		seen = monitor.state_changed(seen.revision).await.unwrap();
	}
}

/// Wait for a READY notification on `notify`, as a service manager does, failing
/// if `process` exits first: however long a loaded host takes to start a relay, one
/// that will report READY is still running.
async fn ready(notify: &tokio::net::UnixDatagram, process: &mut tokio::process::Child) {
	let mut message = [0; 1024];
	loop {
		tokio::select! {
			len = notify.recv(&mut message) => {
				if std::str::from_utf8(&message[..len.unwrap()])
					.unwrap()
					.lines()
					.any(|line| line == "READY=1")
				{
					return;
				}
			}
			status = process.wait() => panic!("exited before READY: {}", status.unwrap()),
		}
	}
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
	ready(&notify, &mut old).await;
	let relays = children(old.id().unwrap());
	assert_eq!(relays.len(), 1, "the stand-in runs one relay: {relays:?}");
	let relay = relays[0];
	let mut left = Reap(Some(relay));

	// The restart's stop: the stand-in leaves, its relay stays.
	// SAFETY: kill has no memory-safety preconditions.
	assert_eq!(
		unsafe { libc::kill(old.id().unwrap() as libc::pid_t, libc::SIGUSR2) },
		0
	);
	let status = old.wait().await.unwrap();
	assert_eq!(status.code(), Some(0), "{status}");
	let mut status = 0;
	// SAFETY: waitpid writes only to `status`.
	let reaped = unsafe { libc::waitpid(relay, &mut status, libc::WNOHANG) };
	assert_eq!(reaped, 0, "the old relay exited with its stand-in");

	// The restart's start: the new relay takes over, and the old one drains.
	let mut new = stand_in(dir.path(), &cert, &key, &notify_path);
	ready(&notify, &mut new).await;
	let status = left.exited().await;
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
	let status = new.wait().await.unwrap();
	assert_eq!(status.code(), Some(0), "{status}");

	// SIGUSR1, a heap profile's signal, reaches the relay rather than ending the
	// stand-in. Built without jemalloc, the relay dies of it, and the stand-in exits
	// as a shell reports that.
	#[cfg(not(feature = "jemalloc"))]
	{
		let mut last = stand_in(dir.path(), &cert, &key, &notify_path);
		ready(&notify, &mut last).await;
		// SAFETY: kill has no memory-safety preconditions.
		assert_eq!(
			unsafe { libc::kill(last.id().unwrap() as libc::pid_t, libc::SIGUSR1) },
			0
		);
		let status = last.wait().await.unwrap();
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
