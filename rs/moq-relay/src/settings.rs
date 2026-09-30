//! Settings registry for `moq-relay`.
//!
//! `usage::Cli` and `usage::Config` reject each other's attributes, so every
//! merged flag is declared twice. [`usage::config::Registry::drift`] is the test
//! that the two stay in step.

#![allow(dead_code)]

/// Every setting `moq-relay` merges, flattened tokio groups included.
#[derive(usage::Config)]
pub struct Settings {
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
	stats: Stats,

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

#[derive(usage::Config)]
#[usage(prefix = "runtime")]
struct Runtime {
	#[usage(env = "MOQ_RUNTIME_WORKERS", cli("--runtime-workers"))]
	workers: Option<u16>,

	#[usage(env = "MOQ_RUNTIME_PIN", cli("--runtime-pin"))]
	pin: Option<bool>,

	#[usage(env = "MOQ_RUNTIME_IO_URING", cli("--runtime-io-uring"))]
	io_uring: Option<bool>,
}

#[derive(usage::Config)]
#[usage(prefix = "cluster")]
struct Cluster {
	#[usage(env = "MOQ_CLUSTER_ID", cli("--cluster-id"))]
	id: Option<u64>,

	#[usage(env = "MOQ_CLUSTER_CONNECT", cli("--cluster-connect"), parse = "list_by_comma")]
	connect: Option<Vec<String>>,

	#[usage(env = "MOQ_CLUSTER_CONNECT_API", cli("--cluster-connect-api"))]
	connect_api: Option<String>,

	#[usage(env = "MOQ_CLUSTER_NODE", cli("--cluster-node"))]
	node: Option<String>,

	#[usage(env = "MOQ_CLUSTER_MESH", cli("--cluster-mesh"))]
	mesh: Option<String>,

	#[usage(env = "MOQ_CLUSTER_TOKEN", cli("--cluster-token"))]
	token: Option<String>,

	#[usage(env = "MOQ_CLUSTER_TIER", cli("--cluster-tier"))]
	tier: Option<String>,

	#[usage(env = "MOQ_CLUSTER_LINGER", cli("--cluster-linger"))]
	linger: Option<String>,

	#[cfg(feature = "cluster-lan")]
	#[usage(flatten)]
	lan: ClusterLan,
}

#[cfg(feature = "cluster-lan")]
#[derive(usage::Config)]
#[usage(prefix = "cluster.lan")]
struct ClusterLan {
	#[usage(env = "MOQ_CLUSTER_LAN", cli("--cluster-lan"))]
	enabled: Option<bool>,

	#[usage(env = "MOQ_CLUSTER_LAN_SECRET", cli("--cluster-lan-secret"))]
	secret: Option<String>,

	#[usage(env = "MOQ_CLUSTER_LAN_APP", cli("--cluster-lan-app"))]
	app: Option<String>,
}

#[derive(usage::Config)]
#[usage(prefix = "auth")]
struct Auth {
	#[usage(env = "MOQ_AUTH_URL", cli("--auth-url"))]
	url: Option<String>,

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

#[derive(usage::Config)]
#[usage(prefix = "web")]
struct Web {
	#[usage(env = "MOQ_WEB_WS", cli("--web-ws"))]
	ws: Option<bool>,

	#[usage(flatten)]
	http: WebHttp,

	#[usage(flatten)]
	https: WebHttps,
}

#[derive(usage::Config)]
#[usage(prefix = "web.http")]
struct WebHttp {
	#[usage(env = "MOQ_WEB_HTTP_LISTEN", cli("--web-http-listen"))]
	listen: Option<String>,
}

#[derive(usage::Config)]
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

#[derive(usage::Config)]
#[usage(prefix = "stats")]
struct Stats {
	#[usage(env = "MOQ_STATS_ENABLED", cli("--stats-enabled"))]
	enabled: Option<bool>,

	#[usage(env = "MOQ_STATS_PREFIX", cli("--stats-prefix"))]
	prefix: Option<String>,

	#[usage(env = "MOQ_STATS_INTERVAL", cli("--stats-interval"))]
	interval: Option<u64>,

	#[usage(env = "MOQ_STATS_NODE", cli("--stats-node"))]
	node: Option<String>,

	#[usage(env = "MOQ_STATS_DEPTH", cli("--stats-depth"))]
	depth: Option<u64>,
}

#[derive(usage::Config)]
#[usage(prefix = "cache")]
struct Cache {
	#[usage(env = "MOQ_CACHE_CAPACITY", cli("--cache-capacity"))]
	capacity: Option<String>,

	#[usage(env = "MOQ_CACHE_HEADROOM", cli("--cache-headroom"))]
	headroom: Option<String>,

	#[usage(env = "MOQ_CACHE_DURATION", cli("--cache-duration"))]
	duration: Option<String>,
}

#[derive(usage::Config)]
#[usage(prefix = "internal")]
struct Internal {
	#[usage(env = "MOQ_INTERNAL_LISTEN", cli("--internal-listen"))]
	listen: Option<String>,
}
