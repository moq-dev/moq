//! Settings registry for `moq-relay`.
//!
//! `usage_rs::Cli` and `usage_rs::Config` reject each other's attributes, so every
//! merged flag is declared twice. [`usage_rs::config::Registry::drift`] is the test
//! that the two stay in step.

#![allow(
	dead_code,
	reason = "declares the settings for the usage registry; nothing reads the fields"
)]

/// Every setting `moq-relay` merges, flattened tokio groups included.
#[derive(usage_rs::Config)]
pub struct Settings {
	#[usage(flatten)]
	limits: Limits,
	#[usage(flatten)]
	listen: moq_tokio::settings::Listen,

	#[usage(flatten)]
	connect: moq_tokio::settings::Connect,

	#[usage(flatten)]
	quic: moq_tokio::settings::Quic,

	#[usage(flatten)]
	log: moq_tokio::settings::Log,

	#[usage(flatten)]
	runtime: Runtime,

	#[usage(flatten)]
	cluster: Cluster,

	#[usage(flatten)]
	auth: Auth,

	#[usage(flatten)]
	web: Web,

	#[usage(flatten)]
	acme: Acme,

	#[usage(flatten)]
	stats: Stats,

	#[cfg(feature = "stun")]
	#[usage(flatten)]
	stun: Stun,

	#[usage(flatten)]
	cache: Cache,

	#[usage(flatten)]
	internal: Internal,

	#[usage(env = "MOQ_DRAIN_TIMEOUT", cli("--drain-timeout"))]
	drain_timeout: Option<String>,

	#[cfg(feature = "iroh")]
	#[usage(flatten)]
	iroh: moq_tokio::settings::Iroh,
}

#[derive(usage_rs::Config)]
#[usage(prefix = "runtime")]
struct Runtime {
	#[usage(env = "MOQ_RUNTIME_WORKERS", cli("--runtime-workers"))]
	workers: Option<u16>,

	#[usage(env = "MOQ_RUNTIME_PIN", cli("--runtime-pin"))]
	pin: Option<bool>,

	#[usage(env = "MOQ_RUNTIME_IO_URING", cli("--runtime-io-uring"))]
	io_uring: Option<bool>,
}

#[derive(usage_rs::Config)]
#[usage(prefix = "cluster")]
struct Cluster {
	#[usage(env = "MOQ_CLUSTER_ID", cli("--cluster-id"))]
	id: Option<u64>,

	#[usage(env = "MOQ_CLUSTER_MAX_HOPS", cli("--cluster-max-hops"))]
	max_hops: Option<usize>,

	#[usage(env = "MOQ_CLUSTER_CONNECT", cli("--cluster-connect"), parse = "list_by_comma")]
	connect: Option<Vec<String>>,

	#[usage(flatten)]
	api: ClusterApi,

	#[usage(env = "MOQ_CLUSTER_NODE", cli("--cluster-node"))]
	node: Option<String>,

	#[usage(env = "MOQ_CLUSTER_TOKEN", cli("--cluster-token"))]
	token: Option<String>,

	#[usage(env = "MOQ_CLUSTER_TIER", cli("--cluster-tier"))]
	tier: Option<String>,

	#[cfg(feature = "cluster-lan")]
	#[usage(flatten)]
	lan: ClusterLan,
}

#[derive(usage_rs::Config)]
#[usage(prefix = "cluster.api")]
struct ClusterApi {
	#[usage(env = "MOQ_CLUSTER_API_URL", cli("--cluster-api-url"))]
	url: Option<String>,
	#[usage(flatten)]
	tls: ClusterApiTls,
}

#[derive(usage_rs::Config)]
#[usage(prefix = "cluster.api.tls")]
struct ClusterApiTls {
	#[usage(env = "MOQ_CLUSTER_API_TLS_ROOT", cli("--cluster-api-tls-root"))]
	root: Option<Vec<String>>,
}

#[cfg(feature = "cluster-lan")]
#[derive(usage_rs::Config)]
#[usage(prefix = "cluster.lan")]
struct ClusterLan {
	#[usage(env = "MOQ_CLUSTER_LAN", cli("--cluster-lan"))]
	enabled: Option<bool>,

	#[usage(env = "MOQ_CLUSTER_LAN_SECRET", cli("--cluster-lan-secret"))]
	secret: Option<String>,

	#[usage(env = "MOQ_CLUSTER_LAN_APP", cli("--cluster-lan-app"))]
	app: Option<String>,
}

#[derive(usage_rs::Config)]
#[usage(prefix = "auth")]
struct Auth {
	#[usage(env = "MOQ_AUTH_URL", cli("--auth-url"))]
	url: Option<String>,

	#[usage(env = "MOQ_AUTH_ROOT", cli("--auth-root"))]
	root: Option<Vec<String>>,

	#[usage(env = "MOQ_AUTH_PUBLIC", cli("--auth-public"), parse = "list_by_comma")]
	public: Option<Vec<String>>,

	#[usage(
		env = "MOQ_AUTH_PUBLIC_SUBSCRIBE",
		cli("--auth-public-subscribe"),
		parse = "list_by_comma"
	)]
	public_subscribe: Option<Vec<String>>,

	#[usage(
		env = "MOQ_AUTH_PUBLIC_PUBLISH",
		cli("--auth-public-publish"),
		parse = "list_by_comma"
	)]
	public_publish: Option<Vec<String>>,
}

#[derive(usage_rs::Config)]
#[usage(prefix = "web")]
struct Web {
	#[usage(env = "MOQ_WEB_WS", cli("--web-ws"))]
	ws: Option<bool>,

	#[usage(flatten)]
	http: WebHttp,

	#[usage(flatten)]
	https: WebHttps,
}

#[derive(usage_rs::Config)]
#[usage(prefix = "web.http")]
struct WebHttp {
	#[usage(env = "MOQ_WEB_HTTP_LISTEN", cli("--web-http-listen"))]
	listen: Option<String>,
}

#[derive(usage_rs::Config)]
#[usage(prefix = "web.https")]
struct WebHttps {
	#[usage(env = "MOQ_WEB_HTTPS_LISTEN", cli("--web-https-listen"))]
	listen: Option<String>,

	#[usage(env = "MOQ_WEB_HTTPS_CERT", cli("--web-https-cert"), parse = "list_by_comma")]
	cert: Option<Vec<String>>,

	#[usage(env = "MOQ_WEB_HTTPS_KEY", cli("--web-https-key"), parse = "list_by_comma")]
	key: Option<Vec<String>>,

	#[usage(env = "MOQ_WEB_HTTPS_ROOT", cli("--web-https-root"), parse = "list_by_comma")]
	root: Option<Vec<String>>,
}

#[derive(usage_rs::Config)]
#[usage(prefix = "acme")]
struct Acme {
	#[usage(env = "MOQ_ACME_DOMAINS", cli("--acme-domains"), parse = "list_by_comma")]
	domains: Option<Vec<String>>,

	#[usage(env = "MOQ_ACME_CONTACT", cli("--acme-contact"), parse = "list_by_comma")]
	contact: Option<Vec<String>>,

	#[usage(env = "MOQ_ACME_CACHE", cli("--acme-cache"))]
	cache: Option<String>,

	#[usage(env = "MOQ_ACME_DIRECTORY", cli("--acme-directory"))]
	directory: Option<String>,

	#[usage(env = "MOQ_ACME_TIMEOUT", cli("--acme-timeout"))]
	timeout: Option<String>,
}

#[derive(usage_rs::Config)]
#[usage(prefix = "stats")]
struct Stats {
	#[usage(env = "MOQ_STATS_ENABLED", cli("--stats-enabled"))]
	enabled: Option<bool>,

	#[usage(env = "MOQ_STATS_PREFIX", cli("--stats-prefix"))]
	prefix: Option<String>,

	#[usage(env = "MOQ_STATS_INTERVAL", cli("--stats-interval"))]
	interval: Option<String>,

	#[usage(env = "MOQ_STATS_NODE", cli("--stats-node"))]
	node: Option<String>,

	#[usage(env = "MOQ_STATS_DEPTH", cli("--stats-depth"))]
	depth: Option<u64>,

	#[usage(env = "MOQ_STATS_LINGER", cli("--stats-linger"))]
	linger: Option<String>,
}

#[derive(usage_rs::Config)]
#[usage(prefix = "cache")]
struct Cache {
	#[usage(env = "MOQ_CACHE_CAPACITY", cli("--cache-capacity"))]
	capacity: Option<String>,

	#[usage(env = "MOQ_CACHE_HEADROOM", cli("--cache-headroom"))]
	headroom: Option<String>,

	#[usage(env = "MOQ_CACHE_DURATION", cli("--cache-duration"))]
	duration: Option<String>,
}

#[derive(usage_rs::Config)]
#[usage(prefix = "internal")]
struct Internal {
	#[usage(env = "MOQ_INTERNAL_LISTEN", cli("--internal-listen"))]
	listen: Option<String>,
}

#[derive(usage_rs::Config)]
struct Limits {
	#[usage(flatten)]
	client: Client,
	#[usage(flatten)]
	peer: Peer,
}

#[derive(usage_rs::Config)]
#[usage(prefix = "limits.client")]
struct Client {
	#[usage(env = "MOQ_LIMITS_CLIENT_ANNOUNCES", cli("--limits-client-announces"))]
	announces: Option<usize>,
}

#[derive(usage_rs::Config)]
#[usage(prefix = "limits.peer")]
struct Peer {
	#[usage(env = "MOQ_LIMITS_PEER_ANNOUNCES", cli("--limits-peer-announces"))]
	announces: Option<usize>,
	#[usage(flatten)]
	quic: PeerQuic,
}

#[derive(usage_rs::Config)]
#[usage(prefix = "limits.peer.quic")]
struct PeerQuic {
	#[usage(env = "MOQ_LIMITS_PEER_QUIC_MAX_STREAMS", cli("--limits-peer-quic-max-streams"))]
	max_streams: Option<u64>,
	#[usage(
		env = "MOQ_LIMITS_PEER_QUIC_RECEIVE_WINDOW",
		cli("--limits-peer-quic-receive-window")
	)]
	receive_window: Option<u64>,
	#[usage(env = "MOQ_LIMITS_PEER_QUIC_SEND_WINDOW", cli("--limits-peer-quic-send-window"))]
	send_window: Option<u64>,
}

#[cfg(feature = "stun")]
#[derive(usage_rs::Config)]
#[usage(prefix = "stun")]
struct Stun {
	#[usage(env = "MOQ_STUN_BIND", cli("--stun-bind"))]
	bind: Option<String>,
}
