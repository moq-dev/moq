//! Handing a relay's sockets to the relay that replaces it.
//!
//! An upgrade starts the new relay beside the old one rather than after it. With
//! `handoff.socket` set, a starting relay first connects to that Unix socket. When
//! an older relay answers, the new one takes every listening socket the old one
//! holds by file descriptor instead of binding its own: nothing queued on them is
//! lost, and the QUIC port's reuseport group keeps the numbering its connection IDs
//! name (see [`moq_sock::shard`]). The new relay then serves, takes every new QUIC
//! handshake, and tells the old one, which stops accepting and drains: each session
//! gets a GOAWAY and stays until it leaves or the drain deadline passes. A cluster
//! peer that redials (see [`moq_tokio::Restart::Wait`]) reaches the new relay, so the
//! drain no longer waits on inbound peers.
//!
//! The two relays share the node's identity for the length of the drain. Each
//! serves its own sessions on its own origin, so a session is counted and named by
//! the one that holds it. The new relay holds its stats broadcast back until the old
//! one exits, so the node's broadcast has one instance at a time and the old one's
//! last counters are read before the new one replaces it.
//!
//! A relay with a predecessor still serving beside it refuses a successor: the
//! group has room for two generations. Either relay exiting leaves the other's
//! sessions alone, since each holds every socket. A successor that dies while its
//! predecessor still drains hands new handshakes back to it, and the next successor
//! takes over from it.
//!
//! # Protocol
//!
//! Over the stream socket, the old relay sends a big-endian `u32` length and a JSON
//! hello, then one byte per batch of at most [`moq_sock::fd::MAX`] descriptors,
//! each batch attached to its byte, in the order the hello lists them. The new relay
//! answers with one byte once it serves, and both hold the connection open: its end
//! is how each learns the other exited. Both check that the peer runs as the same
//! user.

#[cfg(target_os = "linux")]
use std::net::SocketAddr;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Handing this relay's sockets to the relay that replaces it. Linux only.
#[derive(usage_rs::Args, Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct Config {
	/// A Unix socket where this relay serves its sockets to the relay that replaces
	/// it, and where it first looks for the relay it replaces. Unset disables handoff.
	/// Needs `runtime.workers`, since the QUIC port is handed on as a reuseport group.
	#[usage(
		long = "handoff-socket",
		name = "handoff-socket",
		env = "MOQ_HANDOFF_SOCKET",
		setting = "handoff.socket"
	)]
	#[usage(value_hint = usage_rs::ValueHint::AnyPath)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub socket: Option<PathBuf>,

	/// Run the relay as the child of a process that stands in for it as a service's
	/// main process. The stand-in passes SIGTERM and SIGINT on, forwards readiness, and
	/// exits with the relay; on SIGUSR2 it exits at once and leaves the relay running.
	/// A systemd unit with `KillMode=process` and `RestartKillSignal=SIGUSR2` then
	/// restarts by starting the next relay beside this one, which takes over through
	/// `handoff.socket`. Needs `handoff.socket`.
	#[usage(
		long = "handoff-supervise",
		env = "MOQ_HANDOFF_SUPERVISE",
		setting = "handoff.supervise",
		bool_value
	)]
	#[serde(default, skip_serializing_if = "std::ops::Not::not")]
	pub supervise: bool,
}

/// The flag that runs a stand-in, which its relay must not see again.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const SUPERVISE_FLAG: &str = "--handoff-supervise";

/// The arguments a stand-in starts its relay with: its own, but for the flag that
/// made it a stand-in.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn relay_args(args: impl IntoIterator<Item = std::ffi::OsString>) -> Vec<std::ffi::OsString> {
	let mut positional = false;
	args.into_iter()
		.filter(|arg| {
			if positional {
				return true;
			}
			if arg == "--" {
				positional = true;
				return true;
			}
			let Some(arg) = arg.to_str() else {
				return true;
			};
			!(arg == SUPERVISE_FLAG
				|| arg
					.strip_prefix(SUPERVISE_FLAG)
					.is_some_and(|rest| rest.starts_with('=')))
		})
		.collect()
}

/// What one handed-on socket serves, which is how the successor finds it again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Role {
	/// The QUIC port's reuseport group.
	Quic,
	/// `listen.tcp.bind`.
	Tcp,
	/// `listen.unix.bind`.
	Unix,
	/// `web.http.listen`.
	WebHttp,
	/// `web.https.listen`.
	WebHttps,
	/// `internal.listen`.
	Internal,
	/// `stun.bind`.
	Stun,
	/// `handoff.socket` itself.
	Control,
}

#[cfg(target_os = "linux")]
/// The protocol version a [`Hello`] speaks; a successor refuses any other.
const VERSION: u32 = 1;

#[cfg(target_os = "linux")]
/// The byte a successor sends once it serves.
const TOOK: u8 = b'T';

#[cfg(target_os = "linux")]
/// The byte each batch of descriptors rides on.
const BATCH: u8 = b'F';

#[cfg(target_os = "linux")]
/// What the old relay sends first.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Hello {
	version: u32,
	/// Why the old relay will not hand off, in place of sockets.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	refused: Option<String>,
	#[serde(default)]
	sockets: Vec<Entry>,
}

#[cfg(target_os = "linux")]
/// One handed-on socket, or the QUIC group's members.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Entry {
	role: Role,
	/// The address an internet socket is bound to.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	addr: Option<SocketAddr>,
	/// The path a Unix socket is bound at.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	path: Option<PathBuf>,
	/// The QUIC group's shape.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	group: Option<Group>,
	/// How many descriptors this entry carries, after the previous entry's.
	fds: usize,
}

#[cfg(target_os = "linux")]
/// A reuseport group's shape, beside its members' descriptors.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
struct Group {
	/// Members per generation.
	count: u16,
	/// The generation the sender serves.
	generation: u16,
	/// Whether the port lock follows the members.
	lock: bool,
}

/// What a serving [`Handoff`] tells the rest of the relay.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) struct Events {
	/// Set once a successor took over: stop accepting and drain.
	pub handed: tokio::sync::watch::Sender<bool>,
	/// Dropped once no predecessor serves beside this relay.
	pub alone: Option<tokio::sync::oneshot::Sender<()>>,
	/// Starts this relay's drain.
	pub drain: crate::shutdown::Trigger,
}

#[cfg(target_os = "linux")]
/// Whether a socket bound to `held` serves a listener configured for `requested`:
/// the same address, where an ephemeral request takes whatever port was resolved.
fn serves(held: SocketAddr, requested: SocketAddr) -> bool {
	held.ip() == requested.ip() && (requested.port() == 0 || requested.port() == held.port())
}

#[cfg(target_os = "linux")]
pub use imp::supervise;
#[cfg(target_os = "linux")]
pub(crate) use imp::{Handoff, Sockets};

#[cfg(target_os = "linux")]
mod imp {
	use std::collections::HashMap;
	use std::io;
	use std::net::SocketAddr;
	use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
	use std::path::{Path, PathBuf};

	use anyhow::Context;
	use moq_sock::shard;
	use tokio::io::Interest;
	use tokio::net::UnixStream;

	use super::{BATCH, Config, Entry, Events, Hello, Role, TOOK, VERSION, serves};

	/// The sockets a starting relay binds or takes over, and the copies it keeps to
	/// hand on in turn. Disabled without `handoff.socket`, when every method returns
	/// `None` and each listener binds as it always has.
	pub(crate) struct Sockets {
		/// `None` when handoff is off.
		path: Option<PathBuf>,
		/// What the predecessor handed over and nothing has taken yet.
		inherited: HashMap<Role, (Entry, Vec<OwnedFd>)>,
		/// What a successor gets, in the order it is sent.
		exports: Vec<(Entry, Vec<OwnedFd>)>,
		/// Steers this relay's generation of the QUIC group.
		steering: Option<shard::Steering>,
		/// The connection to the relay this one replaces.
		predecessor: Option<UnixStream>,
	}

	impl Sockets {
		/// Handoff off: every listener binds as it always has.
		pub(crate) fn disabled() -> Self {
			Self {
				path: None,
				inherited: HashMap::new(),
				exports: Vec::new(),
				steering: None,
				predecessor: None,
			}
		}

		/// Find the relay this one replaces, taking its sockets when one answers.
		pub(crate) async fn open(config: &Config) -> anyhow::Result<Self> {
			let Some(path) = &config.socket else {
				return Ok(Self::disabled());
			};
			let mut sockets = Self {
				path: Some(path.clone()),
				..Self::disabled()
			};

			let stream = match UnixStream::connect(path).await {
				Ok(stream) => stream,
				// Nobody serves there: this relay is the first.
				Err(err) if matches!(err.kind(), io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused) => {
					tracing::info!(path = %path.display(), "no relay to take over from; binding every listener");
					return Ok(sockets);
				}
				Err(err) => {
					return Err(err).with_context(|| format!("failed to reach the relay at {}", path.display()));
				}
			};
			same_user(&stream).context("refusing the relay at the handoff socket")?;

			let (hello, fds) = receive(&stream)
				.await
				.with_context(|| format!("failed to take over from the relay at {}", path.display()))?;
			let mut fds = fds.into_iter();
			for entry in hello.sockets {
				let taken = fds.by_ref().take(entry.fds).collect();
				sockets.inherited.insert(entry.role, (entry, taken));
			}
			tracing::info!(
				path = %path.display(),
				sockets = ?sockets.inherited.keys().collect::<Vec<_>>(),
				"taking over from the relay already serving"
			);
			sockets.predecessor = Some(stream);
			Ok(sockets)
		}

		/// Whether handoff is on.
		pub(crate) fn enabled(&self) -> bool {
			self.path.is_some()
		}

		/// Whether this relay took over from one still serving.
		pub(crate) fn replacing(&self) -> bool {
			self.predecessor.is_some()
		}

		/// The predecessor's socket for `role`, if it serves `requested`. One that
		/// serves something else is dropped, and the listener binds afresh.
		fn take(&mut self, role: Role, requested: impl Fn(&Entry) -> bool) -> Option<(Entry, Vec<OwnedFd>)> {
			let (entry, fds) = self.inherited.remove(&role)?;
			if requested(&entry) {
				return Some((entry, fds));
			}
			tracing::warn!(
				?role,
				?entry,
				"the predecessor's socket serves a different address; binding afresh"
			);
			None
		}

		/// Keep a copy of `socket` to hand on as `entry`.
		fn export(&mut self, entry: Entry, socket: BorrowedFd<'_>) -> anyhow::Result<()> {
			let fd = socket
				.try_clone_to_owned()
				.context("failed to keep a copy to hand on")?;
			self.exports.push((Entry { fds: 1, ..entry }, vec![fd]));
			Ok(())
		}

		/// The QUIC port's reuseport group on `addr`: the predecessor's, or a fresh one
		/// of `count` members with room for a successor.
		#[cfg(any(feature = "_quic", feature = "_uring"))]
		pub(crate) fn group(&mut self, addr: SocketAddr, count: u16) -> anyhow::Result<Option<shard::Bound>> {
			if !self.enabled() {
				return Ok(None);
			}
			// Both generations share the one byte the steering filter reads.
			let most = shard::MAX_SHARDS / 2;
			if count > most {
				tracing::warn!(count, most, "serving fewer workers, to leave room for a successor");
			}
			let count = count.min(most);
			let inherited = self.take(Role::Quic, |entry| {
				entry.addr.is_some_and(|held| serves(held, addr)) && entry.group.is_some()
			});
			let bound = match inherited {
				Some((entry, mut fds)) => {
					let group = entry.group.expect("checked by take");
					let lock = match group.lock {
						true => fds.pop(),
						false => None,
					};
					if group.count != count {
						tracing::warn!(
							predecessor = group.count,
							configured = count,
							"serving the predecessor's worker count: a group's size cannot change across a handoff"
						);
					}
					let handoff = shard::Handoff {
						addr: entry.addr.expect("checked by take"),
						count: group.count,
						generation: group.generation,
						sockets: fds,
						lock,
					};
					shard::Bound::adopt(handoff).context("failed to adopt the predecessor's QUIC sockets")?
				}
				None => {
					let mut config = shard::Config::new(addr, count);
					config.handoff = true;
					config
						.bind()
						.with_context(|| format!("failed to bind the reuseport group on {addr}"))?
				}
			};

			let handoff = bound.handoff().context("failed to keep the QUIC sockets to hand on")?;
			let lock = handoff.lock.is_some();
			let mut fds = handoff.sockets;
			fds.extend(handoff.lock);
			self.exports.push((
				Entry {
					role: Role::Quic,
					addr: Some(handoff.addr),
					path: None,
					group: Some(super::Group {
						count: handoff.count,
						generation: handoff.generation,
						lock,
					}),
					fds: fds.len(),
				},
				fds,
			));
			self.steering = Some(bound.steering().context("failed to steer the QUIC sockets")?);
			Ok(Some(bound))
		}

		/// The tokio QUIC workers: on the predecessor's group, or a fresh one with room
		/// for a successor when handoff is on.
		#[cfg(feature = "_quic")]
		pub(crate) fn workers(
			&mut self,
			server: moq_tokio::server::Config,
			config: moq_tokio::worker::Config,
		) -> anyhow::Result<moq_tokio::worker::Workers> {
			if !self.enabled() {
				return Ok(moq_tokio::worker::Workers::bind(server, config)?);
			}
			let addr = match &server.listen.bind {
				Some(bind) => bind.resolve().context("failed to resolve listen.bind")?,
				None => moq_tokio::server::DEFAULT_BIND,
			};
			let group = self.group(addr, config.count)?.expect("handoff is on");
			Ok(moq_tokio::worker::Workers::from_group(server, group, config.pin)?)
		}

		/// A TCP listener for `role` on `addr`: the predecessor's, or a fresh one.
		pub(crate) fn tcp(&mut self, role: Role, addr: SocketAddr) -> anyhow::Result<Option<std::net::TcpListener>> {
			if !self.enabled() {
				return Ok(None);
			}
			let listener = match self.take(role, |entry| entry.addr.is_some_and(|held| serves(held, addr))) {
				Some((_, mut fds)) => {
					let fd = fds.pop().context("the predecessor sent no socket")?;
					let listener = std::net::TcpListener::from(fd);
					listener.set_nonblocking(true)?;
					listener
				}
				None => moq_tokio::bind::tcp(addr).with_context(|| format!("failed to bind {addr}"))?,
			};
			let entry = Entry {
				role,
				addr: Some(listener.local_addr()?),
				path: None,
				group: None,
				fds: 1,
			};
			self.export(entry, listener.as_fd())?;
			Ok(Some(listener))
		}

		/// A UDP socket for `role` on `addr`: the predecessor's, or a fresh one.
		#[cfg(feature = "stun")]
		pub(crate) fn udp(&mut self, role: Role, addr: SocketAddr) -> anyhow::Result<Option<std::net::UdpSocket>> {
			if !self.enabled() {
				return Ok(None);
			}
			let socket = match self.take(role, |entry| entry.addr.is_some_and(|held| serves(held, addr))) {
				Some((_, mut fds)) => std::net::UdpSocket::from(fds.pop().context("the predecessor sent no socket")?),
				None => std::net::UdpSocket::bind(addr).with_context(|| format!("failed to bind {addr}"))?,
			};
			socket.set_nonblocking(true)?;
			let entry = Entry {
				role,
				addr: Some(socket.local_addr()?),
				path: None,
				group: None,
				fds: 1,
			};
			self.export(entry, socket.as_fd())?;
			Ok(Some(socket))
		}

		/// A Unix listener for `role` at `path`: the predecessor's, or a fresh one.
		pub(crate) fn unix(
			&mut self,
			role: Role,
			path: &Path,
		) -> anyhow::Result<Option<std::os::unix::net::UnixListener>> {
			if !self.enabled() {
				return Ok(None);
			}
			let listener = match self.take(role, |entry| entry.path.as_deref() == Some(path)) {
				Some((_, mut fds)) => {
					std::os::unix::net::UnixListener::from(fds.pop().context("the predecessor sent no socket")?)
				}
				None => bind_unix(path)?,
			};
			listener.set_nonblocking(true)?;
			let entry = Entry {
				role,
				addr: None,
				path: Some(path.to_path_buf()),
				group: None,
				fds: 1,
			};
			self.export(entry, listener.as_fd())?;
			Ok(Some(listener))
		}

		/// Take the handoff socket itself, ready to serve a successor. `None` without
		/// handoff.
		pub(crate) fn finish(mut self) -> anyhow::Result<Option<Handoff>> {
			let Some(path) = self.path.clone() else {
				return Ok(None);
			};
			let control = self.unix(Role::Control, &path)?.expect("handoff is on");
			// Whatever the predecessor held that this relay no longer serves stays with
			// the predecessor until it exits.
			for (role, _) in self.inherited.drain() {
				tracing::info!(?role, "not serving a listener the predecessor held");
			}
			Ok(Some(Handoff {
				control,
				exports: self.exports,
				steering: self.steering,
				predecessor: self.predecessor,
			}))
		}
	}

	/// Bind a Unix listener at `path`, replacing a socket file nothing listens on.
	///
	/// Only called once a connect to the path found nobody, or for a listener the
	/// predecessor did not hold, so a stale file is a crashed relay's.
	fn bind_unix(path: &Path) -> anyhow::Result<std::os::unix::net::UnixListener> {
		use std::os::unix::fs::FileTypeExt;
		match std::fs::symlink_metadata(path) {
			Ok(meta) if meta.file_type().is_socket() => std::fs::remove_file(path)
				.with_context(|| format!("failed to remove the stale socket at {}", path.display()))?,
			Ok(_) => anyhow::bail!("{} exists and is not a socket", path.display()),
			Err(err) if err.kind() == io::ErrorKind::NotFound => {}
			Err(err) => return Err(err).with_context(|| format!("failed to inspect {}", path.display())),
		}
		std::os::unix::net::UnixListener::bind(path).with_context(|| format!("failed to bind {}", path.display()))
	}

	/// A relay's handoff while it serves: the sockets it hands on, how it steers new
	/// handshakes, and the relay it replaces.
	pub(crate) struct Handoff {
		control: std::os::unix::net::UnixListener,
		exports: Vec<(Entry, Vec<OwnedFd>)>,
		steering: Option<shard::Steering>,
		predecessor: Option<UnixStream>,
	}

	impl Handoff {
		/// Take every new QUIC handshake from the relay this one replaces, while it
		/// keeps the connections it holds. A no-op without one.
		pub(crate) fn steer_here(&self) -> anyhow::Result<()> {
			if self.predecessor.is_none() {
				return Ok(());
			}
			self.steer(shard::Steer::Beside)
				.context("failed to steer new handshakes here")
		}

		/// Tell the relay this one replaces that this one serves, so it stops
		/// accepting and drains. A no-op without one.
		pub(crate) async fn took(&self) {
			let Some(predecessor) = &self.predecessor else {
				return;
			};
			// A predecessor gone already is noticed as the connection's end.
			match send(predecessor, &[TOOK], &[]).await {
				Ok(()) => tracing::info!("serving beside the relay this one replaces; it drains now"),
				Err(err) => tracing::warn!(%err, "the relay this one replaces is gone"),
			}
		}

		/// Steer this relay's generation, when it serves QUIC.
		fn steer(&self, steer: shard::Steer) -> io::Result<()> {
			match &self.steering {
				Some(steering) => steering.steer(steer),
				None => Ok(()),
			}
		}

		/// Serve successors and watch the relay this one replaces, for as long as
		/// this relay runs. Only an error returns.
		pub(crate) async fn run(self, events: Events) -> anyhow::Result<std::convert::Infallible> {
			let Events {
				handed,
				mut alone,
				drain,
			} = events;
			let control = tokio::net::UnixListener::from_std(self.control.try_clone()?)
				.context("failed to serve the handoff socket")?;
			let mut predecessor = self.predecessor.as_ref();
			if predecessor.is_none() {
				alone.take();
			}
			let mut successor: Option<UnixStream> = None;

			loop {
				tokio::select! {
					() = closed(predecessor), if predecessor.is_some() => {
						tracing::info!("the relay this one replaced exited");
						predecessor = None;
						alone.take();
						if let Err(err) = self.steer(shard::Steer::Alone) {
							tracing::warn!(%err, "failed to take back the replaced relay's connections");
						}
					}
					() = closed(successor.as_ref()), if successor.is_some() => {
						tracing::warn!("the relay that took over exited before this one; taking new handshakes back");
						successor = None;
						if let Err(err) = self.steer(shard::Steer::Alone) {
							tracing::warn!(%err, "failed to take new handshakes back");
						}
					}
					accepted = control.accept(), if successor.is_none() => {
						let stream = match accepted {
							Ok((stream, _)) => stream,
							Err(err) => {
								tracing::warn!(%err, "failed to accept on the handoff socket");
								continue;
							}
						};
						match self.offer(&stream, predecessor.is_some()).await {
							Ok(()) => {
								successor = Some(stream);
								handed.send_replace(true);
								drain.start();
							}
							Err(err) => {
								tracing::warn!(err = %format!("{err:#}"), "a relay failed to take over");
								// It may have steered new handshakes to itself before failing. Not when
								// refused for serving beside a predecessor, which still holds the other
								// generation's connections.
								if predecessor.is_none()
									&& let Err(err) = self.steer(shard::Steer::Alone)
								{
									tracing::warn!(%err, "failed to take new handshakes back");
								}
							}
						}
					}
				}
			}
		}

		/// Hand every socket to the relay on `stream`, and wait until it serves.
		async fn offer(&self, stream: &UnixStream, beside: bool) -> anyhow::Result<()> {
			same_user(stream)?;
			let mut hello = Hello {
				version: VERSION,
				..Default::default()
			};
			if beside {
				hello.refused = Some(
					"the relay there still serves beside the one it replaced; retry once that one exits".to_string(),
				);
				let body = serde_json::to_vec(&hello)?;
				send(stream, &frame(&body), &[]).await?;
				anyhow::bail!("refused: still serving beside the relay this one replaced");
			}

			hello.sockets = self.exports.iter().map(|(entry, _)| entry.clone()).collect();
			let body = serde_json::to_vec(&hello)?;
			send(stream, &frame(&body), &[]).await?;
			let fds: Vec<BorrowedFd<'_>> = self
				.exports
				.iter()
				.flat_map(|(_, fds)| fds.iter().map(AsFd::as_fd))
				.collect();
			for batch in fds.chunks(moq_sock::fd::MAX) {
				send(stream, &[BATCH], batch).await?;
			}
			tracing::info!(sockets = fds.len(), "handed every socket to the relay taking over");

			let mut byte = Vec::new();
			let mut unexpected = Vec::new();
			match recv(stream, &mut byte, &mut unexpected).await? {
				0 => anyhow::bail!("it exited before serving"),
				_ if byte.first() == Some(&TOOK) => {}
				_ => anyhow::bail!("it answered {byte:?}"),
			}
			tracing::info!("the relay that took over serves; draining");
			Ok(())
		}
	}

	/// Prefix `body` with its big-endian length.
	fn frame(body: &[u8]) -> Vec<u8> {
		let mut framed = u32::try_from(body.len())
			.expect("a hello fits u32")
			.to_be_bytes()
			.to_vec();
		framed.extend_from_slice(body);
		framed
	}

	/// Read a [`Hello`] and every descriptor it lists.
	async fn receive(stream: &UnixStream) -> anyhow::Result<(Hello, Vec<OwnedFd>)> {
		let mut data = Vec::new();
		let mut fds = Vec::new();
		let hello = loop {
			if data.len() >= 4 {
				let len = u32::from_be_bytes(data[..4].try_into().expect("four bytes")) as usize;
				if data.len() >= 4 + len {
					let hello: Hello = serde_json::from_slice(&data[4..4 + len]).context("unreadable hello")?;
					data.drain(..4 + len);
					break hello;
				}
			}
			anyhow::ensure!(recv(stream, &mut data, &mut fds).await? > 0, "it closed the connection");
		};
		anyhow::ensure!(
			hello.version == VERSION,
			"it speaks handoff version {}, not {VERSION}",
			hello.version
		);
		if let Some(refused) = hello.refused {
			anyhow::bail!("it refused: {refused}");
		}

		let total: usize = hello.sockets.iter().map(|entry| entry.fds).sum();
		let batches = total.div_ceil(moq_sock::fd::MAX);
		while data.len() < batches || fds.len() < total {
			anyhow::ensure!(
				recv(stream, &mut data, &mut fds).await? > 0,
				"it closed the connection with {} of {total} sockets sent",
				fds.len()
			);
		}
		anyhow::ensure!(fds.len() == total, "it sent {} sockets, not {total}", fds.len());
		Ok((hello, fds))
	}

	/// Send all of `data`, with `fds` riding on its first byte.
	async fn send(stream: &UnixStream, mut data: &[u8], mut fds: &[BorrowedFd<'_>]) -> io::Result<()> {
		while !data.is_empty() {
			stream.writable().await?;
			match stream.try_io(Interest::WRITABLE, || moq_sock::fd::send(stream.as_fd(), data, fds)) {
				Ok(sent) => {
					data = &data[sent..];
					fds = &[];
				}
				Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
				Err(err) => return Err(err),
			}
		}
		Ok(())
	}

	/// Read what is available into `data`, and any descriptors into `fds`. Zero at
	/// the end of the stream.
	async fn recv(stream: &UnixStream, data: &mut Vec<u8>, fds: &mut Vec<OwnedFd>) -> io::Result<usize> {
		let mut buf = [0; 4096];
		loop {
			stream.readable().await?;
			match stream.try_io(Interest::READABLE, || moq_sock::fd::recv(stream.as_fd(), &mut buf, fds)) {
				Ok(read) => {
					data.extend_from_slice(&buf[..read]);
					return Ok(read);
				}
				Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
				Err(err) => return Err(err),
			}
		}
	}

	/// Resolve once the relay on `stream` is gone; never without one.
	async fn closed(stream: Option<&UnixStream>) {
		let Some(stream) = stream else {
			return std::future::pending().await;
		};
		let mut data = Vec::new();
		let mut fds = Vec::new();
		// Anything it sends after the handoff is ignored; only its end counts.
		while let Ok(read) = recv(stream, &mut data, &mut fds).await {
			if read == 0 {
				return;
			}
			data.clear();
			fds.clear();
		}
	}

	/// Stand in for the relay as a service's main process, per `handoff.supervise`:
	/// start the relay as a child with this process's arguments, pass it SIGTERM and
	/// SIGINT, forward its readiness, and return its exit code once it exits. On
	/// SIGUSR2 return 0 at once and leave it running, to drain beside the relay
	/// that replaces it.
	///
	/// A service manager takes readiness only from its main process, which the relay
	/// no longer is; it also takes it from nothing else, so a relay left draining can
	/// never speak for the service.
	pub async fn supervise(config: &Config) -> anyhow::Result<i32> {
		use tokio::signal::unix::{SignalKind, signal};

		let path = config
			.socket
			.as_ref()
			.context("handoff.supervise needs handoff.socket")?;
		// Before the relay starts, so no signal for it is missed.
		let mut terminate = signal(SignalKind::terminate())?;
		let mut interrupt = signal(SignalKind::interrupt())?;
		let mut leave = signal(SignalKind::user_defined2())?;

		let exe = std::env::current_exe().context("failed to find this binary")?;
		let mut command = tokio::process::Command::new(exe);
		command
			.args(super::relay_args(std::env::args_os().skip(1)))
			.env("MOQ_HANDOFF_SUPERVISE", "false");

		let notify = match std::env::var_os("NOTIFY_SOCKET") {
			Some(manager) => {
				let mut name = path.clone().into_os_string();
				name.push(format!(".{}.notify", std::process::id()));
				let name = PathBuf::from(name);
				let _ = std::fs::remove_file(&name);
				let socket = tokio::net::UnixDatagram::bind(&name)
					.with_context(|| format!("failed to bind {}", name.display()))?;
				command.env("NOTIFY_SOCKET", &name);
				Some(Notify { name, socket, manager })
			}
			None => None,
		};

		let mut child = command.spawn().context("failed to start the relay")?;
		let pid = child.id().context("the relay exited at once")? as libc::pid_t;
		tracing::info!(pid, "supervising the relay");

		let mut message = [0; 4096];
		let code = loop {
			tokio::select! {
				status = child.wait() => {
					let status = status.context("failed to wait for the relay")?;
					tracing::info!(%status, "the relay exited");
					break status.code().unwrap_or(1);
				}
				Some(()) = terminate.recv() => pass(pid, libc::SIGTERM),
				Some(()) = interrupt.recv() => pass(pid, libc::SIGINT),
				Some(()) = leave.recv() => {
					tracing::info!(pid, "leaving the relay to drain beside the one replacing it");
					break 0;
				}
				Ok(len) = Notify::recv(notify.as_ref(), &mut message) => {
					if let Some(notify) = &notify
						&& let Err(err) = notify.forward(&message[..len])
					{
						tracing::warn!(%err, "failed to forward the relay's notification");
					}
				}
			}
		};
		if let Some(notify) = notify {
			let _ = std::fs::remove_file(&notify.name);
		}
		Ok(code)
	}

	/// Pass `signal` to the relay at `pid`. Not yet reaped, since the wait for it has
	/// not returned, so the pid is still its own.
	fn pass(pid: libc::pid_t, signal: libc::c_int) {
		// SAFETY: kill has no memory-safety preconditions.
		if unsafe { libc::kill(pid, signal) } == -1 {
			tracing::warn!(pid, signal, err = %io::Error::last_os_error(), "failed to signal the relay");
		}
	}

	/// Where a stand-in's relay sends its notifications, and the service manager's
	/// socket they are forwarded to.
	struct Notify {
		name: PathBuf,
		socket: tokio::net::UnixDatagram,
		manager: std::ffi::OsString,
	}

	impl Notify {
		/// The next notification's length; never without a socket.
		async fn recv(notify: Option<&Self>, buf: &mut [u8]) -> io::Result<usize> {
			match notify {
				Some(notify) => notify.socket.recv(buf).await,
				None => std::future::pending().await,
			}
		}

		/// Send `message` on to the service manager, as this process's own.
		fn forward(&self, message: &[u8]) -> io::Result<()> {
			use std::os::linux::net::SocketAddrExt;
			let socket = std::os::unix::net::UnixDatagram::unbound()?;
			let manager = self.manager.as_encoded_bytes();
			match manager.strip_prefix(b"@") {
				Some(name) => {
					let addr = std::os::unix::net::SocketAddr::from_abstract_name(name)?;
					socket.send_to_addr(message, &addr)?;
				}
				None => {
					socket.send_to(message, Path::new(&self.manager))?;
				}
			}
			Ok(())
		}
	}

	/// Refuse a peer that runs as another user: the sockets are this user's.
	fn same_user(stream: &UnixStream) -> anyhow::Result<()> {
		let peer = stream.peer_cred().context("failed to read the peer's credentials")?;
		// SAFETY: geteuid has no preconditions and cannot fail.
		let euid = unsafe { libc::geteuid() };
		anyhow::ensure!(peer.uid() == euid, "it runs as uid {}, not {euid}", peer.uid());
		Ok(())
	}
}

/// Off Linux, handoff is refused when configured, and otherwise does nothing.
#[cfg(not(target_os = "linux"))]
mod imp {
	use super::{Config, Role};

	/// Handoff is Linux-only: with no socket configured every method declines.
	pub(crate) struct Sockets;

	impl Sockets {
		pub(crate) async fn open(config: &Config) -> anyhow::Result<Self> {
			anyhow::ensure!(config.socket.is_none(), "handoff.socket is Linux-only");
			Ok(Self)
		}

		pub(crate) fn replacing(&self) -> bool {
			false
		}

		#[cfg(feature = "_quic")]
		pub(crate) fn workers(
			&mut self,
			server: moq_tokio::server::Config,
			config: moq_tokio::worker::Config,
		) -> anyhow::Result<moq_tokio::worker::Workers> {
			Ok(moq_tokio::worker::Workers::bind(server, config)?)
		}

		pub(crate) fn tcp(
			&mut self,
			_role: Role,
			_addr: std::net::SocketAddr,
		) -> anyhow::Result<Option<std::net::TcpListener>> {
			Ok(None)
		}

		#[cfg(feature = "stun")]
		pub(crate) fn udp(
			&mut self,
			_role: Role,
			_addr: std::net::SocketAddr,
		) -> anyhow::Result<Option<std::net::UdpSocket>> {
			Ok(None)
		}

		#[cfg(all(feature = "uds", unix))]
		pub(crate) fn unix(
			&mut self,
			_role: Role,
			_path: &std::path::Path,
		) -> anyhow::Result<Option<std::os::unix::net::UnixListener>> {
			Ok(None)
		}

		pub(crate) fn finish(self) -> anyhow::Result<Option<Handoff>> {
			Ok(None)
		}
	}

	/// Never called off Linux, where the config refuses `handoff.supervise`.
	pub async fn supervise(_config: &Config) -> anyhow::Result<i32> {
		anyhow::bail!("handoff.supervise is Linux-only")
	}

	/// Never built off Linux.
	pub(crate) enum Handoff {}

	impl Handoff {
		pub(crate) fn steer_here(&self) -> anyhow::Result<()> {
			match *self {}
		}

		pub(crate) async fn took(&self) {
			match *self {}
		}

		pub(crate) async fn run(self, _events: super::Events) -> anyhow::Result<std::convert::Infallible> {
			match self {}
		}
	}
}

#[cfg(not(target_os = "linux"))]
pub use imp::supervise;
#[cfg(not(target_os = "linux"))]
pub(crate) use imp::{Handoff, Sockets};

#[cfg(test)]
mod tests {
	use super::*;

	/// The relay a stand-in starts sees every argument but the one that made it a
	/// stand-in, including after a `--`, where that spelling is a positional.
	#[test]
	fn a_stand_in_starts_its_relay_without_its_flag() {
		let args = [
			"--handoff-supervise",
			"--handoff-socket=/run/h.sock",
			"--handoff-supervise=true",
			"--handoff-supervisor",
			"relay.toml",
			"--",
			"--handoff-supervise",
		];
		let kept = relay_args(args.map(std::ffi::OsString::from));
		assert_eq!(
			kept,
			[
				"--handoff-socket=/run/h.sock",
				"--handoff-supervisor",
				"relay.toml",
				"--",
				"--handoff-supervise"
			]
		);
	}

	/// A handed-on socket's address serves the listener configured for it, and an
	/// ephemeral port takes whatever the old relay resolved.
	#[cfg(target_os = "linux")]
	#[test]
	fn a_held_socket_serves_the_same_address() {
		let held: SocketAddr = "127.0.0.1:4443".parse().unwrap();
		assert!(serves(held, "127.0.0.1:4443".parse().unwrap()));
		assert!(serves(held, "127.0.0.1:0".parse().unwrap()));
		assert!(!serves(held, "127.0.0.1:443".parse().unwrap()));
		assert!(!serves(held, "[::]:4443".parse().unwrap()));
	}
}
