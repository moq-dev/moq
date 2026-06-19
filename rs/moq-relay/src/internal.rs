use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;

use clap::Parser;
use serde::{Deserialize, Serialize};
use tracing::Instrument;

use crate::{AuthToken, Cluster};

/// Where the internal listener binds.
///
/// Parsed from a single string: a `host:port` socket address binds a plain-TCP
/// listener, anything else (or a `unix:` prefix) is treated as a Unix-socket
/// path. So `127.0.0.1:4444` is TCP and `/run/moq/internal.sock` (or
/// `unix:/run/moq/internal.sock`) is a Unix socket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InternalListen {
	/// A plain-TCP qmux listener. No peer identity is available.
	Tcp(SocketAddr),
	/// A Unix-domain-socket qmux listener. Supports the [`InternalAllow`] uid/gid/pid check.
	Unix(PathBuf),
}

impl FromStr for InternalListen {
	type Err = std::convert::Infallible;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		// An explicit `unix:` prefix forces a path, even one that might parse as
		// an address. Otherwise a valid socket address is TCP and everything
		// else is a path.
		if let Some(rest) = s.strip_prefix("unix:") {
			return Ok(Self::Unix(PathBuf::from(rest)));
		}
		Ok(match s.parse::<SocketAddr>() {
			Ok(addr) => Self::Tcp(addr),
			Err(_) => Self::Unix(PathBuf::from(s)),
		})
	}
}

impl fmt::Display for InternalListen {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Tcp(addr) => write!(f, "{addr}"),
			// Prefix so it round-trips back through FromStr as a path.
			Self::Unix(path) => write!(f, "unix:{}", path.display()),
		}
	}
}

fn parse_listen(s: &str) -> Result<InternalListen, std::convert::Infallible> {
	s.parse()
}

/// Peer-credential allowlist for the Unix-socket internal listener.
///
/// Each populated field constrains the corresponding credential; an empty field
/// imposes no constraint. A connection is allowed when it satisfies every
/// populated field (AND across fields, OR within a field). All empty means no
/// check, so the socket's filesystem permissions are the only gate.
///
/// Only applies to a Unix-socket [`InternalListen`]; TCP carries no peer identity.
#[derive(Parser, Clone, Debug, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct InternalAllow {
	/// Allowed peer user IDs. Empty means any uid.
	#[arg(long = "internal-allow-uid", env = "MOQ_INTERNAL_ALLOW_UID", value_delimiter = ',')]
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub uid: Vec<u32>,

	/// Allowed peer group IDs. Empty means any gid.
	#[arg(long = "internal-allow-gid", env = "MOQ_INTERNAL_ALLOW_GID", value_delimiter = ',')]
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub gid: Vec<u32>,

	/// Allowed peer process IDs. Empty means any pid. A populated list rejects
	/// peers whose pid the platform doesn't report.
	#[arg(long = "internal-allow-pid", env = "MOQ_INTERNAL_ALLOW_PID", value_delimiter = ',')]
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub pid: Vec<i32>,
}

impl InternalAllow {
	/// Whether this allowlist imposes any constraint.
	fn is_empty(&self) -> bool {
		self.uid.is_empty() && self.gid.is_empty() && self.pid.is_empty()
	}
}

/// Configuration for the unauthenticated internal listener.
///
/// When [`listen`](Self::listen) is set, the relay binds a listener (plain TCP
/// or a Unix socket) that grants every accepted connection full internal
/// access: publish and subscribe to everything, with no JWT or client
/// certificate.
///
/// There is no transport encryption. A TCP listener has no peer identity, so it
/// must only be reachable from trusted clients (loopback or a private
/// interface). A Unix-socket listener can additionally restrict callers by
/// uid/gid/pid via [`allow`](Self::allow).
#[serde_with::serde_as]
#[derive(Parser, Clone, Debug, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct InternalConfig {
	/// Bind the internal listener here: a `host:port` for TCP, or a path
	/// (optionally `unix:`-prefixed) for a Unix socket.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	#[serde_as(as = "Option<serde_with::DisplayFromStr>")]
	#[arg(long = "internal-listen", env = "MOQ_INTERNAL_LISTEN", value_parser = parse_listen)]
	pub listen: Option<InternalListen>,

	/// Peer-credential allowlist (Unix-socket listeners only).
	#[command(flatten)]
	#[serde(default)]
	pub allow: InternalAllow,
}

/// Run the internal listener if one is configured, otherwise wait forever.
///
/// Used directly in the relay's top-level `select!` so it composes whether or
/// not `internal.listen` is set.
pub async fn run_internal(config: InternalConfig, cluster: Cluster) -> anyhow::Result<()> {
	match config.listen {
		None => std::future::pending().await,
		Some(InternalListen::Tcp(addr)) => run_tcp(addr, config.allow, cluster).await,
		Some(InternalListen::Unix(path)) => run_unix(path, config.allow, cluster).await,
	}
}

async fn run_tcp(addr: SocketAddr, allow: InternalAllow, cluster: Cluster) -> anyhow::Result<()> {
	// TCP carries no peer identity, so an allowlist can't be honored here.
	if !allow.is_empty() {
		tracing::warn!("internal.allow is ignored for a TCP listener; it only applies to Unix sockets");
	}

	// No transport security, so a non-loopback bind is worth flagging. We still
	// allow it (private VPC interfaces are a valid use), just loudly.
	if addr.ip().is_loopback() {
		tracing::info!(%addr, "internal listener (tcp)");
	} else {
		tracing::warn!(%addr, "internal listener bound to a non-loopback address; it is UNAUTHENTICATED, ensure the network is trusted");
	}

	let listener = moq_native::tcp::Listener::bind(addr)
		.await?
		.with_protocols(moq_net::ALPNS.iter().copied());
	while let Some(session) = listener.accept().await {
		match session {
			Ok(session) => spawn_session(session, cluster.clone()),
			Err(err) => tracing::warn!(%err, "internal listener accept failed"),
		}
	}

	anyhow::bail!("internal listener stopped accepting connections")
}

#[cfg(all(feature = "uds", unix))]
async fn run_unix(path: PathBuf, allow: InternalAllow, cluster: Cluster) -> anyhow::Result<()> {
	if allow.is_empty() {
		tracing::warn!(path = %path.display(), "internal Unix listener has no allow list; any local user able to reach the socket gets full access");
	} else {
		tracing::info!(path = %path.display(), ?allow, "internal listener (unix)");
	}

	let listener = moq_native::unix::Listener::bind(&path)
		.await?
		.with_protocols(moq_net::ALPNS.iter().copied());
	// Loose file permissions: the uid/gid/pid allow list is the real gate, and
	// the worker typically runs as a different user than the relay.
	listener.set_mode(0o666)?;

	while let Some(accepted) = listener.accept().await {
		let (session, cred) = match accepted {
			Ok(accepted) => accepted,
			Err(err) => {
				tracing::warn!(%err, "internal listener accept failed");
				continue;
			}
		};

		if !cred_allowed(&allow, &cred) {
			tracing::warn!(uid = cred.uid, gid = cred.gid, pid = ?cred.pid, "internal connection rejected by allow list");
			drop(session);
			continue;
		}

		spawn_session(session, cluster.clone());
	}

	anyhow::bail!("internal listener stopped accepting connections")
}

#[cfg(not(all(feature = "uds", unix)))]
async fn run_unix(path: PathBuf, _allow: InternalAllow, _cluster: Cluster) -> anyhow::Result<()> {
	anyhow::bail!(
		"internal.listen requests a Unix socket ({}) but this relay was built without the `uds` feature",
		path.display()
	)
}

#[cfg(all(feature = "uds", unix))]
fn cred_allowed(allow: &InternalAllow, cred: &moq_native::unix::PeerCred) -> bool {
	let uid_ok = allow.uid.is_empty() || allow.uid.contains(&cred.uid);
	let gid_ok = allow.gid.is_empty() || allow.gid.contains(&cred.gid);
	// A required pid can't be satisfied if the platform doesn't report one.
	let pid_ok = allow.pid.is_empty() || cred.pid.is_some_and(|pid| allow.pid.contains(&pid));
	uid_ok && gid_ok && pid_ok
}

/// Spawn a task that serves one accepted session with full internal access.
fn spawn_session<S>(session: S, cluster: Cluster)
where
	S: web_transport_trait::Session,
{
	// Full access to everything under the empty root, on the internal tier.
	let token = AuthToken::unrestricted(moq_net::Path::new("").to_owned());
	let publish = cluster.publisher(&token);
	let subscribe = cluster.subscriber(&token);
	let stats = cluster.stats.tier(moq_net::Tier::Internal);

	let serve = async move {
		// subscribe/publish look backwards on purpose: see connection.rs. We publish
		// the tracks the client may subscribe to, and subscribe to what it may publish.
		let session = moq_net::Server::new()
			.with_publish(subscribe)
			.with_consume(publish)
			.with_stats(stats)
			.accept(session)
			.await?;

		tracing::info!(version = %session.version(), "negotiated");
		session.closed().await?;
		anyhow::Ok(())
	};

	tokio::spawn(
		async move {
			if let Err(err) = serve.await {
				tracing::warn!(%err, "internal connection closed");
			}
		}
		.instrument(tracing::info_span!("internal")),
	);
}
