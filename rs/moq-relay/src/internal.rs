use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;
use serde::{Deserialize, Serialize};
use tracing::Instrument;

use crate::{AuthToken, Cluster};

/// Configuration for the unauthenticated internal listener(s).
///
/// A TCP and a Unix-socket listener can each be enabled independently. Both
/// grant every accepted connection full internal access (publish and subscribe
/// to everything, no JWT or client certificate), so only expose them to trusted
/// clients. This is the local-worker analogue of a cluster peer dialing `/`.
#[derive(Parser, Clone, Debug, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct InternalConfig {
	/// Plain-TCP listener (`tcp://`).
	#[command(flatten)]
	#[serde(default)]
	pub tcp: InternalTcp,

	/// Unix-socket listener (`unix://`), with an optional peer-credential allowlist.
	#[command(flatten)]
	#[serde(default)]
	pub uds: InternalUds,

	/// Billing tier label that the internal listener's sessions record stats
	/// under. Default `internal`. An empty value selects the default
	/// (unprefixed) tier.
	#[arg(long = "internal-tier", id = "internal-tier", env = "MOQ_INTERNAL_TIER")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub tier: Option<String>,
}

/// Plain-TCP internal listener.
///
/// TCP carries no peer identity, so it must only be reachable from trusted
/// clients. Bind it to loopback or a private interface; a non-loopback bind
/// logs a warning but is allowed.
#[derive(Parser, Clone, Debug, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct InternalTcp {
	/// Bind an unauthenticated plain-TCP (qmux, no TLS) listener on this address.
	#[arg(long = "internal-listen", id = "internal-listen", env = "MOQ_INTERNAL_LISTEN")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub listen: Option<SocketAddr>,
}

/// Unix-socket internal listener.
///
/// The kernel reports the connecting process's credentials, so [`allow`](Self::allow)
/// can restrict callers to a specific worker user. Requires the `uds` build feature.
#[derive(Parser, Clone, Debug, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct InternalUds {
	/// Bind an unauthenticated Unix-socket (qmux, no TLS) listener at this path.
	#[arg(
		long = "internal-uds-listen",
		id = "internal-uds-listen",
		env = "MOQ_INTERNAL_UDS_LISTEN"
	)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub listen: Option<PathBuf>,

	/// Peer-credential allowlist applied to accepted connections.
	#[command(flatten)]
	#[serde(default)]
	pub allow: InternalAllow,
}

/// Peer-credential allowlist for the Unix-socket internal listener.
///
/// Each populated field constrains the corresponding credential; an empty field
/// imposes no constraint. A connection is allowed when it satisfies every
/// populated field (AND across fields, OR within a field). All empty means no
/// check, so the socket's filesystem permissions are the only gate.
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
	#[cfg_attr(not(all(feature = "uds", unix)), allow(dead_code))]
	fn is_empty(&self) -> bool {
		self.uid.is_empty() && self.gid.is_empty() && self.pid.is_empty()
	}
}

/// Run the configured internal listener(s) until one fails; wait forever if none.
///
/// Used directly in the relay's top-level `select!`. The TCP and Unix listeners
/// run concurrently when both are configured.
pub async fn run_internal(config: InternalConfig, cluster: Cluster) -> anyhow::Result<()> {
	// Billing tier these sessions record under (--internal-tier, default `internal`).
	let tier = crate::trusted_tier(config.tier.clone());

	let tcp = {
		let cluster = cluster.clone();
		let tier = tier.clone();
		async move {
			match config.tcp.listen {
				Some(addr) => run_tcp(addr, cluster, tier).await,
				None => std::future::pending().await,
			}
		}
	};

	let uds = async move {
		match config.uds.listen {
			Some(path) => run_uds(path, config.uds.allow, cluster, tier).await,
			None => std::future::pending().await,
		}
	};

	tokio::select! {
		res = tcp => res,
		res = uds => res,
	}
}

async fn run_tcp(addr: SocketAddr, cluster: Cluster, tier: moq_net::Tier) -> anyhow::Result<()> {
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
			Ok(session) => spawn_session(session, cluster.clone(), tier.clone()),
			Err(err) => tracing::warn!(%err, "internal listener accept failed"),
		}
	}

	anyhow::bail!("internal TCP listener stopped accepting connections")
}

#[cfg(all(feature = "uds", unix))]
async fn run_uds(path: PathBuf, allow: InternalAllow, cluster: Cluster, tier: moq_net::Tier) -> anyhow::Result<()> {
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

		spawn_session(session, cluster.clone(), tier.clone());
	}

	anyhow::bail!("internal Unix listener stopped accepting connections")
}

#[cfg(not(all(feature = "uds", unix)))]
async fn run_uds(path: PathBuf, _allow: InternalAllow, _cluster: Cluster, _tier: moq_net::Tier) -> anyhow::Result<()> {
	anyhow::bail!(
		"internal.uds.listen requests a Unix socket ({}) but this relay was built without the `uds` feature",
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

/// Spawn a task that serves one accepted session with full internal access,
/// recording its stats under `tier`.
fn spawn_session<S>(session: S, cluster: Cluster, tier: moq_net::Tier)
where
	S: web_transport_trait::Session,
{
	// Full access to everything under the empty root.
	let token = AuthToken::unrestricted(moq_net::Path::new("").to_owned());
	let publish = cluster.publisher(&token);
	let subscribe = cluster.subscriber(&token);
	let stats = cluster.stats.tier(tier);

	let serve = async move {
		// subscribe/publish look backwards on purpose: see connection.rs. We publish
		// the tracks the client may subscribe to, and subscribe to what it may publish.
		// Only set the side the token grants; moq-net defaults the unset side to a
		// fresh no-op origin.
		let mut server = moq_net::Server::new().with_stats(stats);
		if let Some(subscribe) = subscribe {
			server = server.with_publisher(&subscribe);
		}
		if let Some(publish) = publish {
			server = server.with_subscriber(publish);
		}
		let session = server.accept(session).await?;

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
