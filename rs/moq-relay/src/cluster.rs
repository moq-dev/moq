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

/// One entry in the active-dial map. The provenance flag keeps
/// gossip unannounces from tearing down a peer that was also seeded statically:
/// static peers must keep their reconnect loop running even when the
/// remote temporarily disappears from `.internal/origins/*`.
struct DialEntry {
	handle: AbortHandle,
	is_static: bool,
}

/// Configuration for relay clustering.
///
/// [`Self::connect`] lists peers to dial. [`Self::mesh`] is optional: when set, this
/// relay advertises its own URL so other peers discover and dial it. Set both to
/// join an existing cluster; set mesh alone to act as a passive rendezvous.
///
/// Hop-based routing on broadcasts prevents announcement loops regardless of topology.
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

	/// This relay's own externally-reachable URL. When set, the relay publishes its
	/// address on the cluster origin so other peers can discover and dial it. Pair
	/// with [`Self::connect`] to reach an initial peer who will gossip your address
	/// onward, or set alone for passive rendezvous.
	#[arg(id = "cluster-mesh", long = "cluster-mesh", env = "MOQ_CLUSTER_MESH")]
	pub mesh: Option<String>,

	/// Use the token in this file when connecting to other nodes.
	#[arg(id = "cluster-token", long = "cluster-token", env = "MOQ_CLUSTER_TOKEN")]
	pub token: Option<PathBuf>,

	/// Removed; present only to emit a migration error. Use [`Self::mesh`] instead.
	#[arg(id = "cluster-node", long = "cluster-node", env = "MOQ_CLUSTER_NODE", hide = true)]
	pub node: Option<String>,

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
	pub fn subscriber(&self, token: &AuthToken) -> Option<OriginConsumer> {
		Some(self.origin.with_root(&token.root)?.scope(&token.subscribe)?.consume())
	}

	/// Returns an [`OriginProducer`] scoped to this session's publish permissions.
	pub fn publisher(&self, token: &AuthToken) -> Option<OriginProducer> {
		self.origin.with_root(&token.root)?.scope(&token.publish)
	}

	/// Runs the cluster event loop.
	///
	/// Modes are derived from config: standalone (no work) returns immediately;
	/// passive rendezvous (`mesh` only) parks after publishing self-registration
	/// and does not require a QUIC client; active (`connect` non-empty) dials
	/// peers and, if `mesh` is also set, runs gossip discovery.
	///
	/// Bails when removed flags `cluster.root` / `cluster.node` are set, or when
	/// `connect` is non-empty but no client was attached via
	/// [`with_client`](Self::with_client).
	pub async fn run(self) -> anyhow::Result<()> {
		if let Some(root) = &self.config.root {
			anyhow::bail!(
				"`cluster.root` / `--cluster-root` was removed (value: {root:?}). \
				 Use `--cluster-connect <peer-url>` to dial cluster peers, and \
				 optionally `--cluster-mesh <self-url>` to gossip this relay's address \
				 so other peers can discover and dial it. \
				 See https://doc.moq.dev/bin/relay/cluster."
			);
		}
		if let Some(node) = &self.config.node {
			anyhow::bail!(
				"`cluster.node` / `--cluster-node` was renamed (value: {node:?}). \
				 Use `--cluster-connect <peer-url>` to dial cluster peers, and \
				 optionally `--cluster-mesh <self-url>` to gossip this relay's address \
				 so other peers can discover and dial it. \
				 See https://doc.moq.dev/bin/relay/cluster."
			);
		}

		let has_outbound = !self.config.connect.is_empty();
		let has_work = has_outbound || self.config.mesh.is_some();
		if !has_work {
			tracing::info!("no cluster peers configured; running standalone");
			return Ok(());
		}

		if has_outbound {
			anyhow::ensure!(
				self.client.is_some(),
				"cluster peers configured but no QUIC client attached (call Cluster::with_client)"
			);
		}

		let token = match &self.config.token {
			Some(path) => std::fs::read_to_string(path)
				.context("failed to read cluster token")?
				.trim()
				.to_string(),
			None => String::new(),
		};

		let active: Arc<Mutex<HashMap<String, DialEntry>>> = Arc::new(Mutex::new(HashMap::new()));
		let mut tasks = tokio::task::JoinSet::new();

		for peer in &self.config.connect {
			Self::spawn_dial(&mut tasks, &active, self.clone(), peer.clone(), token.clone(), true);
		}

		// Held in scope so the registration stays announced until `run` exits.
		// Discovery is paired with it: a mesh-only relay (passive rendezvous) has
		// nothing to discover, so we only run it when we also have an outbound peer.
		let _self_registration: Option<BroadcastProducer> = if let Some(mesh) = self.config.mesh.as_deref() {
			let path = Path::new(MESH_PREFIX).join(mesh);
			let broadcast = self
				.origin
				.create_broadcast(&path)
				.expect(".internal/origins is within the relay origin's root");
			tracing::info!(url = %mesh, %path, "advertising cluster mesh URL");

			if has_outbound {
				let this = self.clone();
				let token = token.clone();
				let active = active.clone();
				let self_url = mesh.to_owned();
				tasks.spawn(async move {
					this.run_discovery(self_url, token, active).await;
				});
			}

			Some(broadcast)
		} else {
			None
		};

		if tasks.is_empty() {
			// Passive rendezvous: park to keep `_self_registration` alive. The
			// process still exits via the other arms of `tokio::select!` in main.
			std::future::pending::<()>().await
		}

		while tasks.join_next().await.is_some() {}
		Ok(())
	}

	/// Spawn a dial loop for `peer` and remember its abort handle. Skips if `peer`
	/// is already tracked (caller-side dedup against static peers and prior discoveries).
	fn spawn_dial(
		tasks: &mut tokio::task::JoinSet<()>,
		active: &Arc<Mutex<HashMap<String, DialEntry>>>,
		this: Self,
		peer: String,
		token: String,
		is_static: bool,
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
		active
			.lock()
			.expect("dial map poisoned")
			.insert(peer, DialEntry { handle, is_static });
	}

	/// Watch `.internal/origins/*` for peer registrations and dial each newly-announced
	/// URL that isn't already tracked. An unannounce aborts the corresponding dial only
	/// for gossip-discovered peers; static `--cluster-connect` peers keep reconnecting.
	async fn run_discovery(self, self_url: String, token: String, active: Arc<Mutex<HashMap<String, DialEntry>>>) {
		let Some(mut consumer) = self.origin.consume().with_root(MESH_PREFIX) else {
			tracing::warn!("could not scope cluster origin to {MESH_PREFIX}; discovery disabled");
			return;
		};

		while let Some((relative, announced)) = consumer.announced().await {
			let peer = relative.as_str();
			if peer == self_url {
				continue;
			}

			if announced.is_some() {
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
				active.lock().expect("dial map poisoned").insert(
					peer,
					DialEntry {
						handle: handle.abort_handle(),
						is_static: false,
					},
				);
			} else {
				Self::handle_gossip_unannounce(&active, peer);
			}
		}
	}

	/// Handle a gossip unannounce for `peer`: abort the dial only if the entry was
	/// added by discovery. Static peers (seeded from `--cluster-connect`) keep their
	/// reconnect loop running, since gossip churn is just remote restarts.
	fn handle_gossip_unannounce(active: &Arc<Mutex<HashMap<String, DialEntry>>>, peer: &str) {
		let mut active = active.lock().expect("dial map poisoned");
		match active.get(peer) {
			Some(entry) if entry.is_static => {
				tracing::debug!(
					%peer,
					"gossip unannounce for static peer; reconnect loop kept alive"
				);
			}
			Some(_) => {
				tracing::info!(%peer, "cluster peer unannounced; aborting dial");
				if let Some(entry) = active.remove(peer) {
					entry.handle.abort();
				}
			}
			None => {}
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

	/// Regression test for the static-peer survival fix: gossip-driven unannounces must
	/// not abort the reconnect loop of a peer that was statically configured via
	/// `--cluster-connect`. Before the fix the active map didn't track provenance, so an
	/// unannounce of a peer that appeared in both `connect` and gossip removed it,
	/// permanently breaking that static peer's reconnect.
	#[tokio::test]
	async fn gossip_unannounce_preserves_static_peer() {
		let active: Arc<Mutex<HashMap<String, DialEntry>>> = Arc::new(Mutex::new(HashMap::new()));
		// Stand in for a real dial task; never polled but provides an AbortHandle.
		let placeholder = tokio::spawn(std::future::pending::<()>());
		let static_handle = placeholder.abort_handle();
		active.lock().unwrap().insert(
			"static-peer.example.com:4443".to_string(),
			DialEntry {
				handle: static_handle,
				is_static: true,
			},
		);

		Cluster::handle_gossip_unannounce(&active, "static-peer.example.com:4443");
		assert!(
			active.lock().unwrap().contains_key("static-peer.example.com:4443"),
			"static peer entry must survive a gossip unannounce"
		);

		// Now insert a discovered peer and confirm unannounce DOES drop it.
		let discovered = tokio::spawn(std::future::pending::<()>());
		active.lock().unwrap().insert(
			"discovered.example.com:4443".to_string(),
			DialEntry {
				handle: discovered.abort_handle(),
				is_static: false,
			},
		);
		Cluster::handle_gossip_unannounce(&active, "discovered.example.com:4443");
		assert!(
			!active.lock().unwrap().contains_key("discovered.example.com:4443"),
			"discovered peer entry should be removed on gossip unannounce"
		);
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
		assert!(msg.contains("--cluster-mesh"), "missing --cluster-mesh in: {msg}");
	}

	/// Setting `cluster.node` (the renamed flag) at startup must surface a migration
	/// message that names both replacement flags.
	#[tokio::test]
	async fn cluster_node_errors_with_migration_message() {
		let config = ClusterConfig {
			node: Some("legacy-node.example.com:4443".to_string()),
			..Default::default()
		};
		let err = Cluster::new(config).run().await.expect_err("should error");
		let msg = format!("{err}");
		assert!(msg.contains("cluster.node"), "missing cluster.node in: {msg}");
		assert!(msg.contains("--cluster-connect"), "missing --cluster-connect in: {msg}");
		assert!(msg.contains("--cluster-mesh"), "missing --cluster-mesh in: {msg}");
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

	/// `cluster.node` parsed from TOML triggers the same migration error.
	#[test]
	fn cluster_node_toml_parses_then_errors() {
		let toml = "[cluster]\nnode = \"legacy-node.example.com:4443\"\n";
		let dir = std::env::temp_dir().join("moq-relay-cluster-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("cluster-node-toml.toml");
		std::fs::write(&path, toml).unwrap();

		let args = vec![std::ffi::OsString::from("moq-relay"), std::ffi::OsString::from(&path)];
		let config = Config::parse_and_merge(args).expect("config load");
		assert_eq!(config.cluster.node.as_deref(), Some("legacy-node.example.com:4443"));

		let rt = tokio::runtime::Runtime::new().unwrap();
		let err = rt
			.block_on(Cluster::new(config.cluster).run())
			.expect_err("should error");
		assert!(format!("{err}").contains("cluster.node"));
	}

	/// A relay configured with only `cluster.mesh` (passive rendezvous) must run
	/// without a QUIC client, publish its self-registration on the cluster origin,
	/// and keep that registration alive (i.e. not exit and drop the broadcast).
	#[tokio::test(start_paused = true)]
	async fn passive_rendezvous_runs_without_client_and_advertises_self() {
		let cluster = Cluster::new(ClusterConfig {
			mesh: Some("rendezvous.example.com:4443".to_string()),
			..Default::default()
		});

		// Snapshot a consumer on the cluster origin before run() takes ownership of
		// `cluster` so we can later check that the registration was published.
		let mut watcher = cluster.origin.consume();

		let cluster_run = cluster.clone();
		let mut handle = tokio::spawn(async move { cluster_run.run().await });

		// Give the runtime a moment to execute the synchronous setup work.
		tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

		// The self-registration broadcast must be visible on the origin.
		let (path, broadcast) = watcher.try_announced().expect("self-registration must be published");
		assert_eq!(path.as_str(), ".internal/origins/rendezvous.example.com:4443");
		assert!(broadcast.is_some());

		// run() must NOT have returned: dropping the broadcast (via run returning)
		// would unannounce the registration immediately. Use a short timeout to
		// confirm we're still parked.
		let still_running = tokio::time::timeout(tokio::time::Duration::from_millis(50), &mut handle)
			.await
			.is_err();
		assert!(still_running, "passive rendezvous run() should park, not return");

		handle.abort();
	}

	/// `cluster.mesh` round-trips through TOML and CLI.
	#[test]
	fn cluster_mesh_round_trips() {
		let toml = "[cluster]\nmesh = \"us-east.example.com:4443\"\nconnect = [\"root.example.com:4443\"]\n";
		let dir = std::env::temp_dir().join("moq-relay-cluster-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("cluster-mesh-toml.toml");
		std::fs::write(&path, toml).unwrap();

		let args = vec![std::ffi::OsString::from("moq-relay"), std::ffi::OsString::from(&path)];
		let config = Config::parse_and_merge(args).expect("config load");
		assert_eq!(config.cluster.mesh.as_deref(), Some("us-east.example.com:4443"));
		assert_eq!(config.cluster.connect, vec!["root.example.com:4443".to_string()]);
	}
}
