//! Settings registry for the flattened `moq-tokio` CLI groups.
//!
//! `usage::Cli` and `usage::Config` reject each other's attributes, so every
//! merged flag is declared twice: once on the `Args` structs the binaries parse,
//! and once here. [`usage::config::Registry::drift`] is the test that the two stay in step.

#![allow(dead_code)]

/// Accept-side settings (`[listen]`, `--listen-*`).
#[derive(usage::Config)]
#[usage(prefix = "listen")]
#[doc(hidden)]
pub struct Listen {
	#[usage(env = "MOQ_LISTEN", cli("--listen"))]
	bind: Option<String>,

	#[usage(env = "MOQ_LISTEN_BACKEND", cli("--listen-backend"))]
	backend: Option<String>,

	#[usage(env = "MOQ_LISTEN_VERSION", cli("--listen-version"), parse = "list_by_comma")]
	version: Option<Vec<String>>,

	#[usage(env = "MOQ_LISTEN_PREFERRED_V4", cli("--listen-preferred-v4"))]
	preferred_v4: Option<String>,

	#[usage(env = "MOQ_LISTEN_PREFERRED_V6", cli("--listen-preferred-v6"))]
	preferred_v6: Option<String>,

	#[usage(env = "MOQ_LISTEN_QUIC_LB_ID", cli("--listen-quic-lb-id"))]
	lb_id: Option<String>,

	#[usage(env = "MOQ_LISTEN_QUIC_LB_NONCE", cli("--listen-quic-lb-nonce"))]
	lb_nonce: Option<u64>,

	#[usage(flatten)]
	tls: ListenTls,

	#[cfg(feature = "tcp")]
	#[usage(flatten)]
	tcp: ListenTcp,

	#[cfg(all(feature = "uds", unix))]
	#[usage(flatten)]
	unix: ListenUnix,
}

/// `[listen.tls]` / `--listen-tls-*`.
#[derive(usage::Config)]
#[usage(prefix = "listen.tls")]
struct ListenTls {
	#[usage(env = "MOQ_LISTEN_TLS_CERT", cli("--listen-tls-cert"), parse = "list_by_comma")]
	cert: Option<Vec<String>>,

	#[usage(env = "MOQ_LISTEN_TLS_KEY", cli("--listen-tls-key"), parse = "list_by_comma")]
	key: Option<Vec<String>>,

	#[usage(
		env = "MOQ_LISTEN_TLS_GENERATE",
		cli("--listen-tls-generate"),
		parse = "list_by_comma"
	)]
	generate: Option<Vec<String>>,

	#[usage(env = "MOQ_LISTEN_TLS_ROOT", cli("--listen-tls-root"), parse = "list_by_comma")]
	root: Option<Vec<String>>,
}

/// `[listen.tcp]` / `--listen-tcp-bind`.
#[cfg(feature = "tcp")]
#[derive(usage::Config)]
#[usage(prefix = "listen.tcp")]
struct ListenTcp {
	#[usage(env = "MOQ_LISTEN_TCP_BIND", cli("--listen-tcp-bind"))]
	bind: Option<String>,
}

/// `[listen.unix]` / `--listen-unix-*`.
#[cfg(all(feature = "uds", unix))]
#[derive(usage::Config)]
#[usage(prefix = "listen.unix")]
struct ListenUnix {
	#[usage(env = "MOQ_LISTEN_UNIX_BIND", cli("--listen-unix-bind"))]
	bind: Option<String>,

	#[usage(flatten)]
	allow: ListenUnixAllow,
}

/// `[listen.unix.allow]`.
#[cfg(all(feature = "uds", unix))]
#[derive(usage::Config)]
#[usage(prefix = "listen.unix.allow")]
struct ListenUnixAllow {
	#[usage(
		env = "MOQ_LISTEN_UNIX_ALLOW_UID",
		cli("--listen-unix-allow-uid"),
		parse = "list_by_comma"
	)]
	uid: Option<Vec<u32>>,

	#[usage(
		env = "MOQ_LISTEN_UNIX_ALLOW_GID",
		cli("--listen-unix-allow-gid"),
		parse = "list_by_comma"
	)]
	gid: Option<Vec<u32>>,

	#[usage(
		env = "MOQ_LISTEN_UNIX_ALLOW_PID",
		cli("--listen-unix-allow-pid"),
		parse = "list_by_comma"
	)]
	pid: Option<Vec<i64>>,
}

/// Dial-side settings (`[connect]`, `--connect-*`).
#[derive(usage::Config)]
#[usage(prefix = "connect")]
#[doc(hidden)]
pub struct Connect {
	#[usage(env = "MOQ_CONNECT", cli("--connect"))]
	url: Option<String>,

	#[usage(env = "MOQ_CONNECT_BIND", cli("--connect-bind"))]
	bind: Option<String>,

	#[usage(env = "MOQ_CONNECT_BACKEND", cli("--connect-backend"))]
	backend: Option<String>,

	#[usage(env = "MOQ_CONNECT_RACE", cli("--connect-race"))]
	race: Option<String>,

	#[usage(env = "MOQ_CONNECT_RESOLUTION_DELAY", cli("--connect-resolution-delay"))]
	resolution_delay: Option<String>,

	#[usage(env = "MOQ_CONNECT_TIMEOUT", cli("--connect-timeout"))]
	timeout: Option<String>,

	#[usage(env = "MOQ_CONNECT_VERSION", cli("--connect-version"), parse = "list_by_comma")]
	version: Option<Vec<String>>,

	#[usage(env = "MOQ_CONNECT_ONCE", cli("--connect-once"))]
	once: Option<bool>,

	#[usage(flatten)]
	tls: ConnectTls,

	#[usage(flatten)]
	backoff: ConnectBackoff,

	#[usage(flatten)]
	goaway: ConnectGoaway,

	#[cfg(feature = "websocket")]
	#[usage(flatten)]
	websocket: ConnectWebsocket,
}

/// `[connect.tls]` / `--connect-tls-*`.
#[derive(usage::Config)]
#[usage(prefix = "connect.tls")]
struct ConnectTls {
	#[usage(env = "MOQ_CONNECT_TLS_ROOT", cli("--connect-tls-root"), parse = "list_by_comma")]
	root: Option<Vec<String>>,

	#[usage(env = "MOQ_CONNECT_TLS_SYSTEM_ROOTS", cli("--connect-tls-system-roots"))]
	system_roots: Option<bool>,

	#[usage(
		env = "MOQ_CONNECT_TLS_FINGERPRINT",
		cli("--connect-tls-fingerprint"),
		parse = "list_by_comma"
	)]
	fingerprint: Option<Vec<String>>,

	#[usage(env = "MOQ_CONNECT_TLS_CERT", cli("--connect-tls-cert"))]
	cert: Option<String>,

	#[usage(env = "MOQ_CONNECT_TLS_KEY", cli("--connect-tls-key"))]
	key: Option<String>,

	#[usage(env = "MOQ_CONNECT_TLS_INSECURE", cli("--connect-tls-insecure"))]
	insecure: Option<bool>,

	#[usage(env = "MOQ_CONNECT_TLS_HOST_NAME", cli("--connect-tls-host-name"))]
	host_name: Option<String>,
}

/// `[connect.backoff]` / `--backoff-*`.
#[derive(usage::Config)]
#[usage(prefix = "connect.backoff")]
struct ConnectBackoff {
	#[usage(env = "MOQ_BACKOFF_INITIAL", cli("--backoff-initial"))]
	initial: Option<String>,

	#[usage(env = "MOQ_BACKOFF_MULTIPLIER", cli("--backoff-multiplier"))]
	multiplier: Option<u32>,

	#[usage(env = "MOQ_BACKOFF_MAX", cli("--backoff-max"))]
	max: Option<String>,

	#[usage(env = "MOQ_BACKOFF_TIMEOUT", cli("--backoff-timeout"))]
	timeout: Option<String>,
}

/// `[connect.goaway]` / `--goaway-*`.
#[derive(usage::Config)]
#[usage(prefix = "connect.goaway")]
struct ConnectGoaway {
	#[usage(env = "MOQ_GOAWAY_REDIRECT", cli("--goaway-redirect"))]
	redirect: Option<String>,

	#[usage(env = "MOQ_GOAWAY_HANDOVER", cli("--goaway-handover"))]
	handover: Option<String>,
}

/// `[connect.websocket]` / `--connect-websocket-*`.
#[cfg(feature = "websocket")]
#[derive(usage::Config)]
#[usage(prefix = "connect.websocket")]
struct ConnectWebsocket {
	#[usage(env = "MOQ_CONNECT_WEBSOCKET_ENABLED", cli("--connect-websocket-enabled"))]
	enabled: Option<bool>,

	#[usage(env = "MOQ_CONNECT_WEBSOCKET_DELAY", cli("--connect-websocket-delay"))]
	delay: Option<String>,
}

/// Shared QUIC tuning (`[quic]`, `--quic-*`).
#[derive(usage::Config)]
#[usage(prefix = "quic")]
#[doc(hidden)]
pub struct Quic {
	#[usage(env = "MOQ_QUIC_MAX_STREAMS", cli("--quic-max-streams"))]
	max_streams: Option<u64>,

	#[usage(env = "MOQ_QUIC_GSO", cli("--quic-gso"))]
	gso: Option<bool>,

	#[usage(env = "MOQ_QUIC_IDLE_TIMEOUT", cli("--quic-idle-timeout"))]
	idle_timeout: Option<String>,

	#[usage(env = "MOQ_QUIC_KEEP_ALIVE", cli("--quic-keep-alive"))]
	keep_alive: Option<String>,

	#[usage(env = "MOQ_QUIC_MTU_DISCOVERY", cli("--quic-mtu-discovery"))]
	mtu_discovery: Option<bool>,

	#[usage(env = "MOQ_QUIC_CONGESTION_CONTROL", cli("--quic-congestion-control"))]
	congestion_control: Option<String>,

	#[usage(env = "MOQ_QUIC_RECEIVE_WINDOW", cli("--quic-receive-window"))]
	receive_window: Option<u64>,

	#[usage(env = "MOQ_QUIC_STREAM_RECEIVE_WINDOW", cli("--quic-stream-receive-window"))]
	stream_receive_window: Option<u64>,

	#[usage(env = "MOQ_QUIC_SEND_WINDOW", cli("--quic-send-window"))]
	send_window: Option<u64>,

	#[usage(env = "MOQ_QUIC_QLOG", cli("--quic-qlog"))]
	qlog: Option<String>,
}

/// Log settings (`[log]`, `--log-level`).
#[derive(usage::Config)]
#[usage(prefix = "log")]
#[doc(hidden)]
pub struct Log {
	#[usage(env = "MOQ_LOG_LEVEL", cli("--log-level"))]
	level: Option<String>,
}

/// Iroh endpoint settings (`[iroh]`, `--iroh-*`).
#[cfg(feature = "iroh")]
#[derive(usage::Config)]
#[usage(prefix = "iroh")]
#[doc(hidden)]
pub struct Iroh {
	#[usage(env = "MOQ_IROH_ENABLED", cli("--iroh-enabled"))]
	enabled: Option<bool>,

	#[usage(env = "MOQ_IROH_SECRET", cli("--iroh-secret"))]
	secret: Option<String>,

	#[usage(env = "MOQ_IROH_BIND_V4", cli("--iroh-bind-v4"))]
	bind_v4: Option<String>,

	#[usage(env = "MOQ_IROH_BIND_V6", cli("--iroh-bind-v6"))]
	bind_v6: Option<String>,

	#[usage(env = "MOQ_IROH_DISABLE_RELAY", cli("--iroh-disable-relay"))]
	disable_relay: Option<bool>,
}
