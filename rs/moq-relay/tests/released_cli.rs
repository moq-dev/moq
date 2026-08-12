//! The released command line, frozen.
//!
//! Every flag and environment variable a published `moq-relay` accepted, checked
//! against the parser this build actually has. Renaming a flag is fine, and so is
//! moving it to another section; dropping the released spelling is not, because a
//! deployment's command line or environment goes on using it.
//!
//! Environment variables are the half a clap `alias` silently misses: an alias
//! renames the flag but leaves the variable behind, so a renamed arg keeps parsing
//! on the command line while quietly ignoring the environment. That is what this
//! pins.
//!
//! The list is from `moq-relay` at the last release before the connect/listen
//! rename. Add to it when a flag ships; only remove an entry when the spelling is
//! deliberately dropped, which is a breaking change to call out in the release
//! notes.

use clap::CommandFactory;
use std::collections::{HashMap, HashSet};

/// `(flag, env)` for every released argument. `None` means it had no env var.
const RELEASED: &[(&str, Option<&str>)] = &[
	("auth-api", Some("MOQ_AUTH_API")),
	("auth-domain", Some("MOQ_AUTH_DOMAIN")),
	("auth-key", Some("MOQ_AUTH_KEY")),
	("auth-key-dir", Some("MOQ_AUTH_KEY_DIR")),
	("auth-mtls-tier", Some("MOQ_AUTH_MTLS_TIER")),
	("auth-public", Some("MOQ_AUTH_PUBLIC")),
	("auth-public-api", Some("MOQ_AUTH_PUBLIC_API")),
	("auth-public-publish", Some("MOQ_AUTH_PUBLIC_PUBLISH")),
	("auth-public-subscribe", Some("MOQ_AUTH_PUBLIC_SUBSCRIBE")),
	("auth-tls-cert", Some("MOQ_AUTH_TLS_CERT")),
	("auth-tls-disable-verify", Some("MOQ_AUTH_TLS_DISABLE_VERIFY")),
	("auth-tls-key", Some("MOQ_AUTH_TLS_KEY")),
	("auth-tls-root", Some("MOQ_AUTH_TLS_ROOT")),
	("backoff-initial", Some("MOQ_BACKOFF_INITIAL")),
	("backoff-max", Some("MOQ_BACKOFF_MAX")),
	("backoff-multiplier", Some("MOQ_BACKOFF_MULTIPLIER")),
	("backoff-timeout", Some("MOQ_BACKOFF_TIMEOUT")),
	("cache-capacity", Some("MOQ_CACHE_CAPACITY")),
	("cache-duration", Some("MOQ_CACHE_DURATION")),
	("cache-headroom", Some("MOQ_CACHE_HEADROOM")),
	("client-backend", Some("MOQ_CLIENT_BACKEND")),
	("client-bind", Some("MOQ_CLIENT_BIND")),
	("client-connect", Some("MOQ_CLIENT_CONNECT")),
	("client-connect-timeout", Some("MOQ_CLIENT_CONNECT_TIMEOUT")),
	("client-failover-delay", Some("MOQ_CLIENT_FAILOVER_DELAY")),
	("client-max-streams", Some("MOQ_CLIENT_QUIC_MAX_STREAMS")),
	(
		"client-quic-congestion-control",
		Some("MOQ_CLIENT_QUIC_CONGESTION_CONTROL"),
	),
	("client-quic-gso", Some("MOQ_CLIENT_QUIC_GSO")),
	("client-quic-idle-timeout", Some("MOQ_CLIENT_QUIC_IDLE_TIMEOUT")),
	("client-quic-keep-alive", Some("MOQ_CLIENT_QUIC_KEEP_ALIVE")),
	("client-quic-max-streams", Some("MOQ_CLIENT_QUIC_MAX_STREAMS")),
	("client-quic-mtu-discovery", Some("MOQ_CLIENT_QUIC_MTU_DISCOVERY")),
	("client-quic-qlog", Some("MOQ_CLIENT_QUIC_QLOG")),
	("client-tls-cert", Some("MOQ_CLIENT_TLS_CERT")),
	("client-tls-disable-verify", Some("MOQ_CLIENT_TLS_DISABLE_VERIFY")),
	("client-tls-fingerprint", Some("MOQ_CLIENT_TLS_FINGERPRINT")),
	("client-tls-host-name", Some("MOQ_CLIENT_TLS_HOST_NAME")),
	("client-tls-key", Some("MOQ_CLIENT_TLS_KEY")),
	("client-tls-root", Some("MOQ_CLIENT_TLS_ROOT")),
	("client-tls-system-roots", Some("MOQ_CLIENT_TLS_SYSTEM_ROOTS")),
	("client-version", Some("MOQ_CLIENT_VERSION")),
	("cluster-connect", Some("MOQ_CLUSTER_CONNECT")),
	("cluster-connect-api", Some("MOQ_CLUSTER_CONNECT_API")),
	("cluster-id", Some("MOQ_CLUSTER_ID")),
	("cluster-linger", Some("MOQ_CLUSTER_LINGER")),
	("cluster-mesh", Some("MOQ_CLUSTER_MESH")),
	("cluster-node", Some("MOQ_CLUSTER_NODE")),
	("cluster-tier", Some("MOQ_CLUSTER_TIER")),
	("cluster-token", Some("MOQ_CLUSTER_TOKEN")),
	("internal-listen", Some("MOQ_INTERNAL_LISTEN")),
	("iroh-bind-v4", Some("MOQ_IROH_BIND_V4")),
	("iroh-bind-v6", Some("MOQ_IROH_BIND_V6")),
	("iroh-disable-relay", Some("MOQ_IROH_DISABLE_RELAY")),
	("iroh-enabled", Some("MOQ_IROH_ENABLED")),
	("iroh-secret", Some("MOQ_IROH_SECRET")),
	("listen", Some("MOQ_SERVER_BIND")),
	("log-level", Some("MOQ_LOG_LEVEL")),
	("server-backend", Some("MOQ_SERVER_BACKEND")),
	("server-bind", Some("MOQ_SERVER_BIND")),
	("server-max-streams", Some("MOQ_SERVER_QUIC_MAX_STREAMS")),
	("server-preferred-v4", Some("MOQ_SERVER_PREFERRED_V4")),
	("server-preferred-v6", Some("MOQ_SERVER_PREFERRED_V6")),
	(
		"server-quic-congestion-control",
		Some("MOQ_SERVER_QUIC_CONGESTION_CONTROL"),
	),
	("server-quic-gso", Some("MOQ_SERVER_QUIC_GSO")),
	("server-quic-idle-timeout", Some("MOQ_SERVER_QUIC_IDLE_TIMEOUT")),
	("server-quic-keep-alive", Some("MOQ_SERVER_QUIC_KEEP_ALIVE")),
	("server-quic-lb-id", Some("MOQ_SERVER_QUIC_LB_ID")),
	("server-quic-lb-nonce", Some("MOQ_SERVER_QUIC_LB_NONCE")),
	("server-quic-max-streams", Some("MOQ_SERVER_QUIC_MAX_STREAMS")),
	("server-quic-mtu-discovery", Some("MOQ_SERVER_QUIC_MTU_DISCOVERY")),
	("server-quic-qlog", Some("MOQ_SERVER_QUIC_QLOG")),
	("server-tcp-bind", Some("MOQ_SERVER_TCP_BIND")),
	("server-tls-root", Some("MOQ_SERVER_TLS_ROOT")),
	("server-unix-allow-gid", Some("MOQ_SERVER_UNIX_ALLOW_GID")),
	("server-unix-allow-pid", Some("MOQ_SERVER_UNIX_ALLOW_PID")),
	("server-unix-allow-uid", Some("MOQ_SERVER_UNIX_ALLOW_UID")),
	("server-unix-bind", Some("MOQ_SERVER_UNIX_BIND")),
	("server-version", Some("MOQ_SERVER_VERSION")),
	("stats-depth", Some("MOQ_STATS_DEPTH")),
	("stats-enabled", Some("MOQ_STATS_ENABLED")),
	("stats-interval", Some("MOQ_STATS_INTERVAL")),
	("stats-node", Some("MOQ_STATS_NODE")),
	("stats-prefix", Some("MOQ_STATS_PREFIX")),
	("tls-cert", Some("MOQ_SERVER_TLS_CERT")),
	("tls-disable-verify", None),
	("tls-fingerprint", None),
	("tls-generate", Some("MOQ_SERVER_TLS_GENERATE")),
	("tls-key", Some("MOQ_SERVER_TLS_KEY")),
	("tls-root", None),
	("tls-system-roots", None),
	("web-http-listen", Some("MOQ_WEB_HTTP_LISTEN")),
	("web-https-cert", Some("MOQ_WEB_HTTPS_CERT")),
	("web-https-key", Some("MOQ_WEB_HTTPS_KEY")),
	("web-https-listen", Some("MOQ_WEB_HTTPS_LISTEN")),
	("web-https-root", Some("MOQ_WEB_HTTPS_ROOT")),
	("web-ws", Some("MOQ_WEB_WS")),
	("websocket-delay", Some("MOQ_CLIENT_WEBSOCKET_DELAY")),
	("websocket-enabled", Some("MOQ_CLIENT_WEBSOCKET_ENABLED")),
];

/// Every flag name the parser accepts today, including aliases and hidden args.
fn flags(cmd: &clap::Command) -> HashSet<String> {
	let mut out = HashSet::new();
	for arg in cmd.get_arguments() {
		if let Some(long) = arg.get_long() {
			out.insert(long.to_string());
		}
		for alias in arg.get_all_aliases().unwrap_or_default() {
			out.insert(alias.to_string());
		}
	}
	for sub in cmd.get_subcommands() {
		out.extend(flags(sub));
	}
	out
}

/// Every environment variable the parser reads, mapped to the flag that reads it.
fn envs(cmd: &clap::Command) -> HashMap<String, String> {
	let mut out = HashMap::new();
	for arg in cmd.get_arguments() {
		if let Some(env) = arg.get_env() {
			let flag = arg.get_long().unwrap_or_else(|| arg.get_id().as_str());
			out.insert(env.to_string_lossy().to_string(), flag.to_string());
		}
	}
	for sub in cmd.get_subcommands() {
		out.extend(envs(sub));
	}
	out
}

#[test]
fn released_flags_still_parse() {
	let cmd = moq_relay::Config::command();
	let flags = flags(&cmd);

	let missing: Vec<&str> = RELEASED
		.iter()
		.map(|(flag, _)| *flag)
		.filter(|flag| !flags.contains(*flag))
		.collect();

	assert!(
		missing.is_empty(),
		"released flags no longer parse: {missing:?}\nKeep the old spelling as a clap alias, or as a hidden arg when it also needs its original env var."
	);
}

#[test]
fn released_env_vars_are_still_read() {
	let cmd = moq_relay::Config::command();
	let envs = envs(&cmd);

	let missing: Vec<&str> = RELEASED
		.iter()
		.filter_map(|(_, env)| *env)
		.filter(|env| !envs.contains_key(*env))
		.collect();

	assert!(
		missing.is_empty(),
		"released env vars are silently ignored: {missing:?}\nA clap alias carries the flag name only; give the old variable a hidden arg of its own and fold it in."
	);
}
