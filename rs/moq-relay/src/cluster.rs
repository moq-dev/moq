use std::path::PathBuf;

use anyhow::Context;
use moq_net::{Origin, OriginConsumer, OriginProducer, Stats, Tier};
use url::Url;

use crate::AuthToken;

/// Configuration for relay clustering.
///
/// Each node runs a full mesh: every configured `--cluster-connect` peer is
/// dialed and kept open for the session's lifetime. Hop-based routing on
/// broadcasts prevents announcement loops.
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

	/// Use the token in this file when connecting to other nodes.
	#[arg(id = "cluster-token", long = "cluster-token", env = "MOQ_CLUSTER_TOKEN")]
	pub token: Option<PathBuf>,
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

	/// Runs the cluster event loop, dialing the configured peers and keeping
	/// each connection alive indefinitely with exponential backoff on failure.
	///
	/// Completes once all dials have given up; a node with no peers (`connect`
	/// empty) has no outbound work and returns immediately. Errors when peers
	/// are configured but no client has been attached via
	/// [`with_client`](Self::with_client).
	pub async fn run(self) -> anyhow::Result<()> {
		if self.config.connect.is_empty() {
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

		let mut tasks = tokio::task::JoinSet::new();
		for peer in &self.config.connect {
			let this = self.clone();
			let token = token.clone();
			let peer = peer.clone();
			tasks.spawn(async move {
				if let Err(err) = this.run_remote(&peer, token).await {
					tracing::warn!(%err, %peer, "cluster peer connection ended");
				}
			});
		}

		while tasks.join_next().await.is_some() {}
		Ok(())
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
