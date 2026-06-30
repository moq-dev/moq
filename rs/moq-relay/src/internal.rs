use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;
use serde::{Deserialize, Serialize};
use tracing::Instrument;

use crate::{Auth, AuthParams, AuthToken, Cluster};

/// Configuration for the internal listener(s).
///
/// A TCP and a Unix-socket listener can each be enabled independently. Both
/// AUTHENTICATE every accepted connection the same way: a JWT carried in the
/// moq-lite-05 SETUP path (`/broadcast?jwt=<token>`) is verified through the
/// relay's [`Auth`] and scopes the session, exactly as a native QUIC client would
/// be. A connection with NO JWT is granted the fixed [`anon`](Self::anon) subtree
/// if one is configured (e.g. `.stats` for a local telemetry publisher), otherwise
/// it is rejected. There is no unauthenticated full-access path -- the legacy
/// "unrestricted internal" behaviour is reproducible only by explicitly setting
/// `anon` to the empty root.
///
/// These listeners are the entry point for trusted local workers, in particular
/// the out-of-process protocol gateways (RTMP/SRT/WHIP/WHEP): the RELAY, not the
/// worker, enforces per-user authorization, so a memory-safety bug in a gateway's
/// parser can reach only what its users' tokens permit.
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

	/// Subtree granted to connections that present NO JWT.
	///
	/// A trusted local helper (e.g. the gateways' `.stats` telemetry publisher)
	/// carries no user JWT; such a connection is granted subscribe + publish under
	/// THIS prefix only. The scope is fixed by the relay, so a caller cannot widen
	/// it by advertising a different path. The empty string grants the whole root
	/// (the legacy unrestricted behaviour, now an explicit opt-in); unset (the
	/// default) REJECTS no-JWT connections. Applies to both listeners.
	///
	/// A no-JWT connection that DOES advertise a path is treated as anonymous
	/// (public) access for that path via [`Auth`], not as this anon scope -- so
	/// tokenless public playback still works regardless of this setting.
	#[arg(long = "internal-anon", id = "internal-anon", env = "MOQ_INTERNAL_ANON")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub anon: Option<String>,
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
	/// Bind a plain-TCP (qmux, no TLS) internal listener on this address.
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
	/// Bind a Unix-socket (qmux, no TLS) internal listener at this path.
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
/// check, so the socket's filesystem permissions are the only gate. It is
/// defense-in-depth on top of the JWT verification, not a replacement for it.
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
/// run concurrently when both are configured; both authenticate via [`Auth`].
pub async fn run_internal(config: InternalConfig, cluster: Cluster, auth: Auth) -> anyhow::Result<()> {
	let tcp = {
		let cluster = cluster.clone();
		let auth = auth.clone();
		let anon = config.anon.clone();
		async move {
			match config.tcp.listen {
				Some(addr) => run_tcp(addr, cluster, auth, anon).await,
				None => std::future::pending().await,
			}
		}
	};

	let uds = async move {
		match config.uds.listen {
			Some(path) => run_uds(path, config.uds.allow, config.anon, cluster, auth).await,
			None => std::future::pending().await,
		}
	};

	tokio::select! {
		res = tcp => res,
		res = uds => res,
	}
}

async fn run_tcp(addr: SocketAddr, cluster: Cluster, auth: Auth, anon: Option<String>) -> anyhow::Result<()> {
	// No transport security, so a non-loopback bind is worth flagging. We still
	// allow it (private VPC interfaces are a valid use), just loudly. Connections
	// are still JWT-authenticated; the network is the unencrypted part.
	if addr.ip().is_loopback() {
		tracing::info!(%addr, anon = ?anon, "internal listener (tcp)");
	} else {
		tracing::warn!(%addr, "internal listener bound to a non-loopback address; qmux is UNENCRYPTED, ensure the network is trusted");
	}

	let listener = moq_native::tcp::Listener::bind(addr)
		.await?
		.with_protocols(internal_versions().alpns());
	while let Some(session) = listener.accept().await {
		match session {
			Ok(session) => spawn_session(session, cluster.clone(), auth.clone(), anon.clone()),
			Err(err) => tracing::warn!(%err, "internal listener accept failed"),
		}
	}

	anyhow::bail!("internal TCP listener stopped accepting connections")
}

#[cfg(all(feature = "uds", unix))]
async fn run_uds(
	path: PathBuf,
	allow: InternalAllow,
	anon: Option<String>,
	cluster: Cluster,
	auth: Auth,
) -> anyhow::Result<()> {
	if allow.is_empty() {
		tracing::warn!(path = %path.display(), anon = ?anon, "internal Unix listener has no peer-credential allow list; any local user able to reach the socket can present a JWT");
	} else {
		tracing::info!(path = %path.display(), ?allow, anon = ?anon, "internal listener (unix)");
	}

	let listener = moq_native::unix::Listener::bind(&path)
		.await?
		.with_protocols(internal_versions().alpns());
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

		spawn_session(session, cluster.clone(), auth.clone(), anon.clone());
	}

	anyhow::bail!("internal Unix listener stopped accepting connections")
}

#[cfg(not(all(feature = "uds", unix)))]
async fn run_uds(
	path: PathBuf,
	_allow: InternalAllow,
	_anon: Option<String>,
	_cluster: Cluster,
	_auth: Auth,
) -> anyhow::Result<()> {
	anyhow::bail!(
		"internal.uds.listen requests a Unix socket ({}) but this relay was built without the `uds` feature",
		path.display()
	)
}

/// The version set the internal listeners offer.
///
/// A per-connection JWT rides the moq-lite-05 SETUP path (the only version that
/// expresses a request path on a URL-less transport). lite-05 is work-in-progress
/// so it is excluded from the default ALPN set; opt into it here on TOP of the
/// defaults. Offering the older versions too keeps no-JWT/anon clients (which need
/// no path) working on any version; a client that wants to present a JWT must
/// negotiate lite-05 (the gateways pin it).
fn internal_versions() -> moq_net::Versions {
	let mut versions: Vec<moq_net::Version> = moq_net::Versions::all().iter().copied().collect();
	versions.push(
		"moq-lite-05-wip"
			.parse()
			.expect("moq-lite-05-wip is a known version"),
	);
	moq_net::Versions::from(versions)
}

#[cfg(all(feature = "uds", unix))]
fn cred_allowed(allow: &InternalAllow, cred: &moq_native::unix::PeerCred) -> bool {
	let uid_ok = allow.uid.is_empty() || allow.uid.contains(&cred.uid);
	let gid_ok = allow.gid.is_empty() || allow.gid.contains(&cred.gid);
	// A required pid can't be satisfied if the platform doesn't report one.
	let pid_ok = allow.pid.is_empty() || cred.pid.is_some_and(|pid| allow.pid.contains(&pid));
	uid_ok && gid_ok && pid_ok
}

/// Spawn a task that authenticates one accepted internal connection via its
/// SETUP-advertised JWT, then serves it scoped to the resulting token.
///
/// Mirrors the native [`Connection`](crate::Connection) auth path, but sources the
/// path + JWT from the moq-lite-05 SETUP (URL-less transport) instead of a request
/// URL, and never inspects an mTLS peer cert (the socket is local):
///
/// - a JWT is verified + scoped through [`Auth`];
/// - a no-JWT connection with NO path gets the fixed `anon` subtree if configured
///   (a trusted local helper, e.g. the stats publisher), else it is rejected;
/// - a no-JWT connection WITH a path resolves anonymous/public access for that path
///   through [`Auth`] (tokenless public playback).
fn spawn_session<S>(session: S, cluster: Cluster, auth: Auth, anon: Option<String>)
where
	S: web_transport_trait::Session,
{
	let serve = async move {
		// Read the SETUP first so we can authorize before granting anything. Offer
		// lite-05 (see internal_versions) so the SETUP can carry the request path;
		// accept_request then surfaces it for `from_path`.
		let request = moq_net::Server::new()
			.with_versions(internal_versions())
			.accept_request(session)
			.await?;
		let params = AuthParams::from_path(request.path().unwrap_or(""));

		// No JWT + no path is a trusted local helper (e.g. the stats publisher):
		// grant the fixed anon subtree. Everything else -- a JWT (verify + scope) or
		// no JWT but a real path (tokenless public playback) -- goes through Auth.
		let resolved = if params.jwt.is_none() && params.path.trim_matches('/').is_empty() {
			anon.as_deref()
				.map(AuthToken::anon)
				.ok_or_else(|| anyhow::anyhow!("internal connection requires a JWT"))
		} else {
			auth.verify(&params).await.map_err(anyhow::Error::from)
		};

		let token = match resolved {
			Ok(token) => token,
			Err(err) => {
				// Signal an auth rejection on the wire (the gateway maps it to 401)
				// instead of a bare transport drop, mirroring Connection::run.
				request.close(moq_net::Error::Unauthorized);
				return Err(err);
			}
		};

		// Workers carry end-user traffic, so bill on the tier the token resolved to
		// (the auth API can still promote a first-party token to internal).
		let tier = match token.internal {
			true => moq_net::Tier::Internal,
			false => moq_net::Tier::External,
		};
		let stats = cluster.stats.tier(tier);
		let _session_stats = stats.session(&token.root);

		let publish = cluster.publisher(&token);
		let subscribe = cluster.subscriber(&token);
		if publish.is_none() && subscribe.is_none() {
			request.close(moq_net::Error::Unauthorized);
			anyhow::bail!("token grants no publish or subscribe paths");
		}

		// subscribe/publish look backwards on purpose: see connection.rs. We publish
		// the tracks the client may subscribe to, and subscribe to what it may publish.
		let mut session = request
			.with_publish(subscribe)
			.with_consume(publish)
			.with_stats(stats)
			.ok()
			.await?;

		tracing::info!(version = %session.version(), internal = token.internal, root = %token.root, "internal connection authenticated");

		// Close once the credential expires, mirroring Connection::run; otherwise
		// hold the session open until it closes on its own.
		match token.expires {
			None => session.closed().await?,
			Some(expires) => {
				let remaining = expires.duration_since(std::time::SystemTime::now()).unwrap_or_default();
				match tokio::time::timeout(remaining, session.closed()).await {
					Ok(res) => res?,
					Err(_) => {
						tracing::info!("credential expired, closing session");
						session.close(moq_net::Error::Unauthorized);
					}
				}
			}
		}
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
