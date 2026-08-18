//! Unix-domain-socket qmux transport, reachable via the `unix://` URL scheme.
//!
//! Runs the QMux wire format over an `AF_UNIX` stream. Unlike the `tcp://`
//! transport, the kernel reports the connecting process's credentials
//! (`SO_PEERCRED` / `LOCAL_PEERCRED`), so a server can authenticate the peer's
//! uid/gid/pid without a shared secret. Unix-only.

use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::{fs, io};

use url::Url;

/// The QMux wire-format version both ends speak. Fixed (not negotiated) since a
/// raw stream has no TLS ALPN to carry it.
const WIRE_VERSION: qmux::Version = qmux::Version::QMux01;

/// Plaintext Unix-socket qmux listener settings, with an optional
/// peer-credential allowlist.
///
/// Flattened onto [`crate::listen::Config::unix`].
// The derived arg group is named after the struct, so it needs an explicit id to
// stay unique across the flattened sections.
#[derive(clap::Args, Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[group(id = "listen-unix")]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct Config {
	/// Bind a plaintext qmux Unix-socket listener at this path.
	#[arg(long = "listen-unix-bind", id = "listen-unix-bind", env = "MOQ_LISTEN_UNIX_BIND")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub bind: Option<PathBuf>,

	/// Peer-credential allowlist. `None` (the default) enforces nothing, so the
	/// parent directory's permissions are the only filesystem gate.
	#[command(flatten)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub allow: Option<Allow>,

	/// The released `--server-unix-*` spellings and their env vars, folded in by
	/// [`Config::resolved`].
	#[command(flatten)]
	#[serde(skip)]
	pub(crate) legacy: Legacy,
}

/// The `--server-unix-*` flags from before the accept side was named `listen`.
///
/// Separate args rather than clap aliases, since an alias renames the flag but
/// leaves its env var behind.
#[derive(clap::Args, Clone, Debug, Default)]
#[group(id = "listen-unix-legacy")]
pub(crate) struct Legacy {
	#[arg(
		long = "server-unix-bind",
		id = "server-unix-bind",
		env = "MOQ_SERVER_UNIX_BIND",
		hide = true
	)]
	bind: Option<PathBuf>,

	#[arg(
		long = "server-unix-allow-uid",
		id = "server-unix-allow-uid",
		env = "MOQ_SERVER_UNIX_ALLOW_UID",
		value_delimiter = ',',
		hide = true
	)]
	uid: Vec<u32>,

	#[arg(
		long = "server-unix-allow-gid",
		id = "server-unix-allow-gid",
		env = "MOQ_SERVER_UNIX_ALLOW_GID",
		value_delimiter = ',',
		hide = true
	)]
	gid: Vec<u32>,

	#[arg(
		long = "server-unix-allow-pid",
		id = "server-unix-allow-pid",
		env = "MOQ_SERVER_UNIX_ALLOW_PID",
		value_delimiter = ',',
		hide = true
	)]
	pid: Vec<i32>,
}

impl Config {
	/// The released spellings in use, each paired with what replaced it. Reached
	/// through [`crate::listen::Config::deprecated`].
	pub(crate) fn deprecated(&self) -> crate::Deprecated {
		let legacy = &self.legacy;
		let mut found = crate::Deprecated::default();

		for (used, old, env, new) in [
			(
				legacy.bind.is_some(),
				"--server-unix-bind",
				"MOQ_SERVER_UNIX_BIND",
				"--listen-unix-bind / MOQ_LISTEN_UNIX_BIND",
			),
			(
				!legacy.uid.is_empty(),
				"--server-unix-allow-uid",
				"MOQ_SERVER_UNIX_ALLOW_UID",
				"--listen-unix-allow-uid / MOQ_LISTEN_UNIX_ALLOW_UID",
			),
			(
				!legacy.gid.is_empty(),
				"--server-unix-allow-gid",
				"MOQ_SERVER_UNIX_ALLOW_GID",
				"--listen-unix-allow-gid / MOQ_LISTEN_UNIX_ALLOW_GID",
			),
			(
				!legacy.pid.is_empty(),
				"--server-unix-allow-pid",
				"MOQ_SERVER_UNIX_ALLOW_PID",
				"--listen-unix-allow-pid / MOQ_LISTEN_UNIX_ALLOW_PID",
			),
		] {
			if used {
				found.flag(old, Some(env), new);
			}
		}
		found
	}
}

/// Peer-credential allowlist for a `unix://` listener.
///
/// The kernel reports the connecting process's credentials. Each populated list
/// constrains the corresponding credential (AND across the three, OR within
/// each); all empty means no check.
#[derive(clap::Args, Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[group(id = "listen-unix-allow")]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct Allow {
	/// Allowed peer user IDs. Empty means any uid.
	#[arg(
		long = "listen-unix-allow-uid",
		id = "listen-unix-allow-uid",
		env = "MOQ_LISTEN_UNIX_ALLOW_UID",
		value_delimiter = ','
	)]
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub uid: Vec<u32>,

	/// Allowed peer group IDs. Empty means any gid.
	#[arg(
		long = "listen-unix-allow-gid",
		id = "listen-unix-allow-gid",
		env = "MOQ_LISTEN_UNIX_ALLOW_GID",
		value_delimiter = ','
	)]
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub gid: Vec<u32>,

	/// Allowed peer PIDs. Empty means any pid; a populated list rejects peers
	/// whose PID the platform doesn't report.
	#[arg(
		long = "listen-unix-allow-pid",
		id = "listen-unix-allow-pid",
		env = "MOQ_LISTEN_UNIX_ALLOW_PID",
		value_delimiter = ','
	)]
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub pid: Vec<i32>,
}

impl Allow {
	/// Whether any field is populated (i.e. the allowlist enforces something).
	pub(crate) fn is_empty(&self) -> bool {
		self.uid.is_empty() && self.gid.is_empty() && self.pid.is_empty()
	}

	/// Whether `cred` satisfies every populated field (AND across fields, OR
	/// within a field). A required pid is unsatisfiable when the platform
	/// reports none.
	pub(crate) fn permits(&self, cred: &PeerCred) -> bool {
		let uid_ok = self.uid.is_empty() || self.uid.contains(&cred.uid);
		let gid_ok = self.gid.is_empty() || self.gid.contains(&cred.gid);
		let pid_ok = self.pid.is_empty() || cred.pid.is_some_and(|pid| self.pid.contains(&pid));
		uid_ok && gid_ok && pid_ok
	}
}

/// Errors specific to the Unix-domain-socket qmux transport.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// The socket failed to bind, accept, connect, or chmod.
	#[error(transparent)]
	Io(#[from] io::Error),

	/// The `unix://` URL had no socket path.
	#[error("missing socket path in unix:// URL")]
	MissingPath,

	/// The qmux handshake failed while dialing.
	#[error("qmux connect failed")]
	Connect(#[source] qmux::Error),

	/// The qmux handshake failed while accepting.
	#[error("qmux accept failed")]
	Accept(#[source] qmux::Error),

	/// The bind path already exists and is not a socket, so we refuse to unlink it.
	#[error("refusing to replace existing non-socket file at {0}")]
	NotASocket(PathBuf),
}

type Result<T> = std::result::Result<T, Error>;

/// Credentials of a connected Unix-socket peer.
///
/// `pid` is `None` on platforms that don't report it (e.g. some macOS versions);
/// `uid`/`gid` are always available.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeerCred {
	/// The peer process's effective user ID.
	pub uid: u32,
	/// The peer process's effective group ID.
	pub gid: u32,
	/// The peer process's PID, if the platform reports it.
	pub pid: Option<i32>,
}

/// Dial a `unix://<path>` URL, advertising `protocols` for in-band ALPN
/// negotiation. Returns a qmux session over the socket.
///
/// The path is taken from the URL path, so use a triple slash for an absolute
/// path: `unix:///run/moq/internal.sock`.
pub(crate) async fn connect(url: Url, protocols: &[&str]) -> Result<qmux::Session> {
	let path = socket_path(&url).ok_or(Error::MissingPath)?;
	tracing::debug!(peer = %crate::connect::Endpoint(&url), "connecting via Unix socket");
	qmux::uds::Config::new(WIRE_VERSION)
		.protocols(protocols.iter().copied())
		.connect(path)
		.await
		.map_err(Error::Connect)
}

fn socket_path(url: &Url) -> Option<PathBuf> {
	let path = url.path();
	if path.is_empty() {
		None
	} else {
		Some(PathBuf::from(path))
	}
}

/// Listens for incoming qmux connections on a Unix domain socket.
///
/// Each accepted connection yields the session plus the peer's [`PeerCred`], so
/// the caller can enforce a uid/gid/pid allowlist. The socket file is removed on
/// drop.
pub struct Listener {
	listener: tokio::net::UnixListener,
	path: PathBuf,
	protocols: Vec<String>,
	health: crate::accept::Health,
}

impl Listener {
	/// Bind a Unix socket at `path`, replacing a stale socket file left by a
	/// previous run.
	///
	/// Refuses to unlink the path if it exists and is not a socket, to avoid
	/// clobbering an unrelated file.
	pub async fn bind(path: impl AsRef<Path>) -> Result<Self> {
		let path = path.as_ref().to_path_buf();

		// A leftover socket from a crashed run would make bind() fail with
		// EADDRINUSE, so unlink it first. Anything that isn't a socket we leave
		// alone and error out.
		match fs::symlink_metadata(&path) {
			Ok(meta) if meta.file_type().is_socket() => fs::remove_file(&path)?,
			Ok(_) => return Err(Error::NotASocket(path)),
			Err(err) if err.kind() == io::ErrorKind::NotFound => {}
			Err(err) => return Err(err.into()),
		}

		let listener = tokio::net::UnixListener::bind(&path)?;
		Ok(Self {
			listener,
			path,
			protocols: Vec::new(),
			health: crate::accept::Health::new("unix"),
		})
	}

	/// A live handle to this listener's accept-loop health, for an embedder that
	/// publishes it (see [`crate::accept`]).
	pub fn accept_health(&self) -> crate::accept::Health {
		self.health.clone()
	}

	/// Report into `health` instead of the one this listener made for itself.
	///
	/// For an owner that has to hand the handle out *before* the listener exists:
	/// [`crate::Server`] binds these lazily (they need a runtime), but an embedder
	/// registering them with a metrics endpoint does so at startup.
	pub fn with_accept_health(mut self, health: crate::accept::Health) -> Self {
		self.health = health;
		self
	}

	/// Advertise these application protocols (moq ALPNs) for in-band negotiation,
	/// in preference order. The first server entry the client also offers wins.
	pub fn with_protocols<I, S>(mut self, protocols: I) -> Self
	where
		I: IntoIterator<Item = S>,
		S: Into<String>,
	{
		self.protocols = protocols.into_iter().map(Into::into).collect();
		self
	}

	/// Set the socket file's permission bits (e.g. `0o660`).
	pub fn set_mode(&self, mode: u32) -> Result<()> {
		fs::set_permissions(&self.path, fs::Permissions::from_mode(mode))?;
		Ok(())
	}

	/// The bound socket path.
	pub fn path(&self) -> &Path {
		&self.path
	}

	/// Accept the next connection, returning the session and the peer's credentials.
	///
	/// A failed `accept(2)` is handled here rather than yielded: it is classified,
	/// counted, logged, and paced by [`accept_health`](Self::accept_health), then
	/// retried, because the caller has no better answer than to ask again. A
	/// per-connection failure is still yielded as `Some(Err(..))`.
	///
	/// As in [`crate::tcp`], the `Option` has no `None` case left to report.
	pub async fn accept(&self) -> Option<Result<(qmux::Session, PeerCred)>> {
		let stream = self.accept_socket().await;
		let cred = match stream.peer_cred() {
			Ok(cred) => PeerCred {
				uid: cred.uid(),
				gid: cred.gid(),
				pid: cred.pid(),
			},
			Err(err) => return Some(Err(err.into())),
		};
		let session = qmux::uds::Config::new(WIRE_VERSION)
			.protocols(self.protocols.iter().map(String::as_str))
			.accept(stream)
			.await
			.map_err(Error::Accept);
		Some(session.map(|session| (session, cred)))
	}

	/// The `accept(2)` half: keep asking until a connection comes back.
	async fn accept_socket(&self) -> tokio::net::UnixStream {
		loop {
			match self.listener.accept().await {
				Ok((stream, _addr)) => {
					self.health.accepted();
					return stream;
				}
				Err(err) => {
					if let Some(delay) = self.health.failed(&err) {
						tokio::time::sleep(delay).await;
					}
				}
			}
		}
	}
}

impl Drop for Listener {
	fn drop(&mut self) {
		// Best-effort: don't leave a stale socket file behind.
		let _ = fs::remove_file(&self.path);
	}
}

#[cfg(test)]
mod legacy_tests {
	use super::*;
	use clap::Parser;

	#[derive(Parser)]
	struct Cli {
		#[command(flatten)]
		unix: Config,
	}

	fn parse(args: &[&str]) -> Config {
		let mut argv = vec!["test"];
		argv.extend_from_slice(args);
		Cli::parse_from(argv).unix
	}

	/// The released `--server-unix-*` spellings parse so the process can name their
	/// replacements, and configure nothing.
	///
	/// The allowlist is the sharp one: the three credential lists are ANDed, so a
	/// released `uid` quietly dropped next to a canonical `gid` would *widen* access
	/// from one user to a whole group. Refusing the command line outright is the
	/// only answer that can't get that wrong.
	#[test]
	fn released_spellings_are_reported_not_applied() {
		let config = parse(&["--server-unix-bind", "/tmp/moq.sock", "--server-unix-allow-uid", "501"]);
		assert_eq!(config.bind, None);
		assert!(config.allow.as_ref().is_none_or(|allow| allow.uid.is_empty()));

		let reported = config.deprecated().to_string();
		assert!(
			reported.contains("--server-unix-bind / MOQ_SERVER_UNIX_BIND -> --listen-unix-bind / MOQ_LISTEN_UNIX_BIND"),
			"{reported}"
		);
		assert!(reported.contains("--server-unix-allow-uid"), "{reported}");
	}

	/// A canonical flag next to a released one does not excuse it.
	#[test]
	fn a_canonical_spelling_does_not_excuse_a_released_one() {
		let config = parse(&[
			"--listen-unix-bind",
			"/tmp/new.sock",
			"--server-unix-bind",
			"/tmp/old.sock",
		]);
		assert_eq!(config.bind, Some(PathBuf::from("/tmp/new.sock")));
		assert!(config.deprecated().to_string().contains("--server-unix-bind"));
	}

	/// The canonical allowlist, which is what `--help` teaches.
	#[test]
	fn canonical_allowlist_applies() {
		let config = parse(&["--listen-unix-allow-gid", "20", "--listen-unix-allow-uid", "501"]);
		assert!(config.deprecated().is_empty());
		let allow = config.allow.as_ref().expect("allowlist");
		assert_eq!(allow.uid, vec![501]);
		assert_eq!(allow.gid, vec![20]);
	}

	/// No flag at all leaves the allowlist unset, so the parent directory's
	/// permissions stay the only filesystem gate.
	#[test]
	fn no_allowlist_stays_unset() {
		let config = parse(&["--listen-unix-bind", "/tmp/moq.sock"]);
		assert!(config.allow.as_ref().is_none_or(|allow| allow.is_empty()));
	}
}
