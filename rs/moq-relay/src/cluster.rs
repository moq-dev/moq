use std::{
	collections::HashMap,
	path::PathBuf,
	sync::{Arc, Mutex},
};

use anyhow::Context;
use moq_net::{BroadcastProducer, Origin, OriginConsumer, OriginProducer, Path, Stats, Tier};
use tokio::task::AbortHandle;
use url::Url;

use crate::AuthToken;

/// Path prefix under which cluster nodes advertise their own URLs for gossip-style
/// peer discovery. Restricted to mTLS (`token.internal`) sessions by
/// [`Cluster::subscriber`] / [`Cluster::publisher`].
const MESH_PREFIX: &str = ".internal/origins";

/// Configuration for relay clustering.
///
/// Two modes that can be combined:
///
/// - **Static** ([`Self::connect`]): explicit list of peer URLs to dial. Each is kept
///   alive for the session lifetime; no discovery happens.
/// - **Gossip** ([`Self::node`] + at least one [`Self::connect`] entry): advertise
///   this relay's URL on the cluster origin so connected peers discover and dial it,
///   and watch for the advertisements of others so we dial them too.
///
/// Hop-based routing on broadcasts prevents announcement loops regardless of mode.
#[serde_with::serde_as]
#[derive(clap::Args, Clone, Debug, serde::Serialize, serde::Deserialize, Default)]
#[serde_with::skip_serializing_none]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
#[group(id = "cluster-config")]
pub struct ClusterConfig {
	/// Connect to one or more other cluster nodes. Accepts a comma-separated list on the CLI
	/// or repeat the flag; in config files use a TOML array.
	#[serde(alias = "connect")]
	#[arg(
		id = "cluster-connect",
		long = "cluster-connect",
		env = "MOQ_CLUSTER_CONNECT",
		value_delimiter = ','
	)]
	#[serde_as(as = "serde_with::OneOrMany<_>")]
	pub connect: Vec<String>,

	/// This relay's own externally-reachable URL. When set, the relay publishes its address
	/// on the cluster origin (under `.internal/origins/<url>`) so other mTLS-authenticated
	/// peers can discover and dial it. Pair with [`Self::connect`] to reach an initial peer
	/// who will gossip your address onward.
	#[arg(id = "cluster-node", long = "cluster-node", env = "MOQ_CLUSTER_NODE")]
	pub node: Option<String>,

	/// Use the token in this file when connecting to other nodes.
	#[arg(id = "cluster-token", long = "cluster-token", env = "MOQ_CLUSTER_TOKEN")]
	pub token: Option<PathBuf>,

	/// Removed; present only to emit a migration error. Use [`Self::connect`] instead.
	#[arg(id = "cluster-root", long = "cluster-root", env = "MOQ_CLUSTER_ROOT", hide = true)]
	pub root: Option<String>,
}

/// A relay cluster built around a single [`OriginProducer`].
///
/// Local sessions and remote cluster connections all publish into the same
/// origin. Loop prevention and shortest-path preference come from the
/// hop list carried on each broadcast (see [`moq_net::Broadcast::hops`]).
///
/// Construct with [`Cluster::new`], then attach a QUIC client and (optionally)
/// a [`Stats`] aggregator with the `with_*` builder methods. A cluster without
/// a client can serve local sessions but cannot dial remote peers.
#[derive(Clone)]
pub struct Cluster {
	config: ClusterConfig,
	client: Option<moq_native::Client>,

	/// All broadcasts, local and remote. Downstream sessions read from here
	/// (filtered by their auth token) and remote dials both read and write here.
	pub origin: OriginProducer,

	/// Stats aggregator. One instance per relay; sessions pick a tier via
	/// [`Stats::tier`] at acceptance time so external (non-mTLS) and internal
	/// (mTLS / cluster peer) traffic land in separate counter sets. Defaults
	/// to [`Stats::disabled`] (a no-op aggregator) until [`with_stats`](Self::with_stats)
	/// is called.
	pub stats: Stats,
}

impl Cluster {
	/// Creates a new cluster with a fresh origin and no peers, client, or stats.
	///
	/// Use [`with_client`](Self::with_client) to enable dialing remote peers
	/// (required when `config.connect` is non-empty), and
	/// [`with_stats`](Self::with_stats) to enable metrics publishing.
	pub fn new(config: ClusterConfig) -> Self {
		let origin = Origin::random().produce();
		tracing::info!(origin_id = %origin.id, "cluster initialized");
		Cluster {
			config,
			client: None,
			origin,
			stats: Stats::disabled(),
		}
	}

	/// Attach a QUIC client used to dial cluster peers.
	///
	/// Required when `config.connect` is non-empty; [`run`](Self::run) returns
	/// an error otherwise.
	pub fn with_client(mut self, client: moq_native::Client) -> Self {
		self.client = Some(client);
		self
	}

	/// Attach a [`Stats`] aggregator. Replaces the default no-op aggregator.
	///
	/// Build the value with [`StatsConfig::build`](crate::StatsConfig::build),
	/// passing [`Self::origin`] so the aggregator publishes through the same
	/// origin cluster peers read from.
	pub fn with_stats(mut self, stats: Stats) -> Self {
		self.stats = stats;
		self
	}

	/// Returns an [`OriginConsumer`] scoped to this session's subscribe permissions.
	///
	/// Non-internal tokens (i.e. JWT-authenticated end users) cannot see `.internal/*`
	/// paths regardless of their declared scope. Cluster mesh registrations and other
	/// infrastructure broadcasts live under that prefix.
	pub fn subscriber(&self, token: &AuthToken) -> Option<OriginConsumer> {
		let view = self.origin.with_root(&token.root)?.scope(&token.subscribe)?.consume();
		Some(if token.internal { view } else { view.block(".internal") })
	}

	/// Returns an [`OriginProducer`] scoped to this session's publish permissions.
	///
	/// Non-internal tokens cannot publish into `.internal/*` regardless of their
	/// declared scope.
	pub fn publisher(&self, token: &AuthToken) -> Option<OriginProducer> {
		let view = self.origin.with_root(&token.root)?.scope(&token.publish)?;
		Some(if token.internal { view } else { view.block(".internal") })
	}

	/// Runs the cluster event loop: dial static `--cluster-connect` peers, publish a
	/// self-registration broadcast for `--cluster-node` (if set), and watch for other
	/// peers' registrations to discover and dial them.
	///
	/// Completes once all dials have given up; a node with no peers and no self URL
	/// returns immediately. Errors:
	/// - if `cluster.root` / `--cluster-root` is set (removed flag);
	/// - if any cluster work is configured but no QUIC client has been attached via
	///   [`with_client`](Self::with_client).
	pub async fn run(self) -> anyhow::Result<()> {
		if let Some(root) = &self.config.root {
			anyhow::bail!(
				"`cluster.root` / `--cluster-root` was removed (value: {root:?}). \
				 Use `--cluster-connect <peer-url>` for static peer connections, or \
				 `--cluster-node <self-url>` to gossip this relay's address so other peers \
				 can discover and dial it. See https://doc.moq.dev/bin/relay/cluster."
			);
		}

		let has_work = !self.config.connect.is_empty() || self.config.node.is_some();
		if !has_work {
			tracing::info!("no cluster peers configured; running standalone");
			return Ok(());
		}

		anyhow::ensure!(
			self.client.is_some(),
			"cluster peers configured but no QUIC client attached (call Cluster::with_client)"
		);

		let token = match &self.config.token {
			Some(path) => std::fs::read_to_string(path)
				.context("failed to read cluster token")?
				.trim()
				.to_string(),
			None => String::new(),
		};

		// Hold the self-registration broadcast alive for the lifetime of `run`. Dropping
		// it would unannounce immediately and tell peers we've left.
		let _self_registration: Option<BroadcastProducer> = self.config.node.as_deref().map(|node| {
			let path = Path::new(MESH_PREFIX).join(node);
			let broadcast = self
				.origin
				.create_broadcast(&path)
				.expect(".internal/origins is within the relay origin's root");
			tracing::info!(%node, %path, "advertising cluster node");
			broadcast
		});

		// Track active dial tasks by URL so static and gossip-discovered peers don't
		// duplicate, and so the discovery side can abort a task when a peer unannounces.
		let active: Arc<Mutex<HashMap<String, AbortHandle>>> = Arc::new(Mutex::new(HashMap::new()));

		let mut tasks = tokio::task::JoinSet::new();

		// Seed static peers from --cluster-connect.
		for peer in &self.config.connect {
			Self::spawn_dial(&mut tasks, &active, self.clone(), peer.clone(), token.clone());
		}

		// Spawn the gossip discovery task if --cluster-node is set.
		if let Some(self_url) = self.config.node.clone() {
			let this = self.clone();
			let token = token.clone();
			let active = active.clone();
			tasks.spawn(async move {
				this.run_discovery(self_url, token, active).await;
			});
		}

		while tasks.join_next().await.is_some() {}
		Ok(())
	}

	/// Spawn a dial loop for `peer` and remember its abort handle. Skips if `peer`
	/// is already tracked (caller-side dedup against static peers and prior discoveries).
	fn spawn_dial(
		tasks: &mut tokio::task::JoinSet<()>,
		active: &Arc<Mutex<HashMap<String, AbortHandle>>>,
		this: Self,
		peer: String,
		token: String,
	) {
		{
			let active = active.lock().expect("dial map poisoned");
			if active.contains_key(&peer) {
				return;
			}
		}
		let peer_for_task = peer.clone();
		let handle = tasks.spawn(async move {
			if let Err(err) = this.run_remote(&peer_for_task, token).await {
				tracing::warn!(%err, peer = %peer_for_task, "cluster peer connection ended");
			}
		});
		active.lock().expect("dial map poisoned").insert(peer, handle);
	}

	/// Watch `.internal/origins/*` for peer registrations and dial each newly-announced
	/// URL that isn't already tracked. Unannounces abort the corresponding dial.
	async fn run_discovery(self, self_url: String, token: String, active: Arc<Mutex<HashMap<String, AbortHandle>>>) {
		let Some(mut consumer) = self.origin.consume().with_root(MESH_PREFIX) else {
			tracing::warn!("could not scope cluster origin to {MESH_PREFIX}; discovery disabled");
			return;
		};

		while let Some((relative, announced)) = consumer.announced().await {
			let peer = relative.as_str();
			if peer == self_url {
				continue;
			}

			match announced {
				Some(_) => {
					let peer = peer.to_owned();
					let already_active = {
						let active = active.lock().expect("dial map poisoned");
						active.contains_key(&peer)
					};
					if already_active {
						tracing::debug!(%peer, "discovered peer already tracked; skipping dial");
						continue;
					}
					tracing::info!(%peer, "discovered cluster peer; dialing");
					let this = self.clone();
					let token = token.clone();
					let peer_for_task = peer.clone();
					let handle = tokio::spawn(async move {
						if let Err(err) = this.run_remote(&peer_for_task, token).await {
							tracing::warn!(%err, peer = %peer_for_task, "cluster peer connection ended");
						}
					});
					active
						.lock()
						.expect("dial map poisoned")
						.insert(peer, handle.abort_handle());
				}
				None => {
					tracing::info!(%peer, "cluster peer unannounced; aborting dial");
					if let Some(handle) = active.lock().expect("dial map poisoned").remove(peer) {
						handle.abort();
					}
				}
			}
		}
	}

	#[tracing::instrument("remote", skip_all, err, fields(%remote))]
	async fn run_remote(self, remote: &str, token: String) -> anyhow::Result<()> {
		let mut url = Url::parse(&format!("https://{remote}/"))?;
		if !token.is_empty() {
			url.query_pairs_mut().append_pair("jwt", &token);
		}

		let base_backoff = tokio::time::Duration::from_secs(1);
		let max_backoff = tokio::time::Duration::from_secs(300);
		// Sessions shorter than this are treated as churn: we keep backing off
		// instead of resetting, otherwise a peer that rejects us instantly would
		// turn into a tight reconnect loop.
		let stable_threshold = tokio::time::Duration::from_secs(10);

		let mut backoff = base_backoff;

		loop {
			let started = tokio::time::Instant::now();
			let result = self.run_remote_once(&url).await;
			let elapsed = started.elapsed();

			match result {
				Ok(()) if elapsed >= stable_threshold => backoff = base_backoff,
				Ok(()) => {
					tracing::warn!(?elapsed, "cluster peer session closed cleanly but quickly; backing off");
					backoff = (backoff * 2).min(max_backoff);
				}
				Err(err) => {
					tracing::warn!(%err, "cluster peer error; will retry");
					backoff = (backoff * 2).min(max_backoff);
				}
			}

			tokio::time::sleep(backoff).await;
		}
	}

	async fn run_remote_once(&self, url: &Url) -> anyhow::Result<()> {
		let mut log_url = url.clone();
		log_url.set_query(None);
		tracing::info!(url = %log_url, "dialing cluster peer");

		// Checked at the start of `run`; per-peer tasks inherit that guarantee.
		let client = self
			.client
			.clone()
			.context("internal: cluster peer dial without an attached QUIC client")?;

		// Cluster-to-cluster traffic is internal by definition.
		let session = client
			.with_publish(self.origin.consume())
			.with_consume(self.origin.clone())
			.with_stats(self.stats.tier(Tier::Internal))
			.connect(url.clone())
			.await
			.context("failed to connect to cluster peer")?;

		session.closed().await.map_err(Into::into)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::Config;
	use moq_net::{Broadcast, PathOwned, PathPrefixes};

	fn full_scope_jwt() -> AuthToken {
		AuthToken {
			root: PathOwned::default(),
			subscribe: PathPrefixes::from(vec![PathOwned::from(String::new())]),
			publish: PathPrefixes::from(vec![PathOwned::from(String::new())]),
			internal: false,
		}
	}

	/// A JWT with the broadest possible scope is still kept out of `.internal/*`.
	#[tokio::test]
	async fn internal_paths_invisible_to_non_mtls_token() {
		let cluster = Cluster::new(ClusterConfig::default());
		let mesh = Broadcast::new().produce();
		let user = Broadcast::new().produce();

		cluster
			.origin
			.publish_broadcast(".internal/origins/peer.example.com:4443", mesh.consume());
		cluster.origin.publish_broadcast("demo/test", user.consume());

		let token = full_scope_jwt();
		let mut subscriber = cluster.subscriber(&token).expect("subscriber");

		// The user broadcast is visible; the mesh registration must not be.
		let (path, broadcast) = subscriber.try_announced().expect("user announce");
		assert_eq!(path.as_str(), "demo/test");
		assert!(broadcast.is_some());
		assert!(
			subscriber.try_announced().is_none(),
			".internal/* must not be visible to a broad-scope JWT"
		);

		// The publisher view rejects publishes to `.internal/*` even with broad scope.
		let publisher = cluster.publisher(&token).expect("publisher");
		let attempt = Broadcast::new().produce();
		assert!(!publisher.publish_broadcast(".internal/origins/attacker", attempt.consume()));
	}

	/// mTLS sessions see the mesh registrations they need to route between cluster peers.
	#[tokio::test]
	async fn internal_paths_visible_to_mtls_token() {
		let cluster = Cluster::new(ClusterConfig::default());
		let mesh = Broadcast::new().produce();
		cluster
			.origin
			.publish_broadcast(".internal/origins/peer.example.com:4443", mesh.consume());

		let mut subscriber = cluster.subscriber(&AuthToken::unrestricted()).expect("subscriber");
		let (path, broadcast) = subscriber.try_announced().expect("announce");
		assert_eq!(path.as_str(), ".internal/origins/peer.example.com:4443");
		assert!(broadcast.is_some());
	}

	/// Setting `cluster.root` (the removed flag) at startup must surface a migration
	/// message that names both the replacement flags.
	#[tokio::test]
	async fn cluster_root_errors_with_migration_message() {
		let config = ClusterConfig {
			root: Some("legacy-root.example.com:4443".to_string()),
			..Default::default()
		};
		let err = Cluster::new(config).run().await.expect_err("should error");
		let msg = format!("{err}");
		assert!(msg.contains("cluster.root"), "missing cluster.root in: {msg}");
		assert!(msg.contains("--cluster-connect"), "missing --cluster-connect in: {msg}");
		assert!(msg.contains("--cluster-node"), "missing --cluster-node in: {msg}");
	}

	/// `cluster.root` parsed from TOML triggers the same migration error.
	#[test]
	fn cluster_root_toml_parses_then_errors() {
		let toml = "[cluster]\nroot = \"legacy-root.example.com:4443\"\n";
		let dir = std::env::temp_dir().join("moq-relay-cluster-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("cluster-root-toml.toml");
		std::fs::write(&path, toml).unwrap();

		let args = vec![std::ffi::OsString::from("moq-relay"), std::ffi::OsString::from(&path)];
		let config = Config::parse_and_merge(args).expect("config load");
		assert_eq!(config.cluster.root.as_deref(), Some("legacy-root.example.com:4443"));

		let rt = tokio::runtime::Runtime::new().unwrap();
		let err = rt
			.block_on(Cluster::new(config.cluster).run())
			.expect_err("should error");
		assert!(format!("{err}").contains("cluster.root"));
	}

	/// `cluster.node` round-trips through TOML and CLI.
	#[test]
	fn cluster_node_round_trips() {
		let toml = "[cluster]\nnode = \"us-east.example.com:4443\"\nconnect = [\"root.example.com:4443\"]\n";
		let dir = std::env::temp_dir().join("moq-relay-cluster-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("cluster-node-toml.toml");
		std::fs::write(&path, toml).unwrap();

		let args = vec![std::ffi::OsString::from("moq-relay"), std::ffi::OsString::from(&path)];
		let config = Config::parse_and_merge(args).expect("config load");
		assert_eq!(config.cluster.node.as_deref(), Some("us-east.example.com:4443"));
		assert_eq!(config.cluster.connect, vec!["root.example.com:4443".to_string()]);
	}
}
