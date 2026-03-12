use std::path::PathBuf;

use anyhow::Context;
use moq_lite::{Broadcast, BroadcastConsumer, BroadcastProducer, Origin, OriginConsumer, OriginProducer};
use tracing::Instrument;
use url::Url;

use crate::AuthToken;

#[serde_with::serde_as]
#[derive(clap::Args, Clone, Debug, serde::Serialize, serde::Deserialize, Default)]
#[serde_with::skip_serializing_none]
#[serde(default, deny_unknown_fields)]
pub struct ClusterConfig {
	/// Connect to these hostnames to form the cluster.
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

	/// Our hostname which we advertise to other nodes.
	#[arg(id = "cluster-node", long = "cluster-node", env = "MOQ_CLUSTER_NODE")]
	pub node: Option<String>,

	/// The prefix to use for cluster announcements.
	/// Defaults to "internal/origins".
	///
	/// WARNING: This should not be accessible by users unless authentication is disabled (YOLO).
	#[arg(
		id = "cluster-prefix",
		long = "cluster-prefix",
		default_value = "internal/origins",
		env = "MOQ_CLUSTER_PREFIX"
	)]
	pub prefix: String,
}

#[derive(Clone)]
pub struct Cluster {
	config: ClusterConfig,
	client: moq_native::Client,

	// All broadcasts, both local and remote.
	// Hops-based routing ensures the shortest path is preferred.
	pub origin: OriginProducer,
}

impl Cluster {
	pub fn new(config: ClusterConfig, client: moq_native::Client) -> Self {
		Cluster {
			config,
			client,
			origin: Origin::produce(),
		}
	}

	// For a given auth token, return the origin that should be used for the session.
	pub fn subscriber(&self, token: &AuthToken) -> Option<OriginConsumer> {
		let subscribe_origin = self.origin.with_root(&token.root)?;
		subscribe_origin.consume_only(&token.subscribe)
	}

	// For a given auth token, return the origin that should be used for the session.
	pub fn publisher(&self, token: &AuthToken) -> Option<OriginProducer> {
		let publish_origin = self.origin.with_root(&token.root)?;
		publish_origin.publish_only(&token.publish)
	}

	// Register a cluster node's presence.
	//
	// Returns a [ClusterRegistration] that should be kept alive for the duration of the session.
	pub fn register(&self, token: &AuthToken) -> Option<ClusterRegistration> {
		let node = token.register.clone()?;
		let broadcast = Broadcast::new().produce();

		let path = moq_lite::Path::new(&self.config.prefix).join(&node);
		self.origin.publish_broadcast(path, broadcast.consume());

		Some(ClusterRegistration::new(node, broadcast))
	}

	pub fn get(&self, broadcast: &str) -> Option<BroadcastConsumer> {
		self.origin.consume_broadcast(broadcast)
	}

	pub async fn run(self) -> anyhow::Result<()> {
		if self.config.connect.is_empty() {
			// No peers configured, just accept incoming connections.
			tracing::info!("no cluster peers configured, accepting incoming connections only");
			std::future::pending::<()>().await;
			anyhow::bail!("unexpected return");
		}

		// If the token is provided, read it from the disk and use it in the query parameter.
		// TODO put this in an AUTH header once WebTransport supports it.
		let token = match &self.config.token {
			Some(path) => std::fs::read_to_string(path)
				.context("failed to read token")?
				.trim()
				.to_string(),
			None => "".to_string(),
		};

		let mut tasks = tokio::task::JoinSet::new();
		for remote in self.config.connect.clone() {
			let this = self.clone();
			let token = token.clone();
			tasks.spawn(async move { this.run_remote(&remote, token).await }.in_current_span());
		}

		// If any connection fails permanently, propagate the error.
		while let Some(res) = tasks.join_next().await {
			res??;
		}

		Ok(())
	}

	#[tracing::instrument("remote", skip_all, err, fields(%remote))]
	async fn run_remote(mut self, remote: &str, token: String) -> anyhow::Result<()> {
		let mut url = Url::parse(&format!("https://{remote}/"))?;
		{
			let mut q = url.query_pairs_mut();
			if !token.is_empty() {
				q.append_pair("jwt", &token);
			}
			if let Some(register) = &self.config.node {
				q.append_pair("register", register);
			}
		}

		let mut backoff = 1;

		loop {
			let res = self.run_remote_once(&url).await;

			match res {
				Ok(()) => backoff = 1,
				Err(err) => {
					backoff *= 2;
					tracing::error!(%err, "remote error");
				}
			}

			let timeout = tokio::time::Duration::from_secs(backoff);
			if timeout > tokio::time::Duration::from_secs(300) {
				// 5 minutes of backoff is enough, just give up.
				anyhow::bail!("remote connection keep failing, giving up");
			}

			tokio::time::sleep(timeout).await;
		}
	}

	async fn run_remote_once(&mut self, url: &Url) -> anyhow::Result<()> {
		let mut log_url = url.clone();
		log_url.set_query(None);
		tracing::info!(url = %log_url, "connecting to remote");

		let session = self
			.client
			.clone()
			.with_publish(self.origin.consume())
			.with_consume(self.origin.clone())
			.connect(url.clone())
			.await
			.context("failed to connect to remote")?;

		session.closed().await.map_err(Into::into)
	}
}

pub struct ClusterRegistration {
	// The name of the node.
	node: String,

	// The announcement, send to other nodes.
	broadcast: BroadcastProducer,
}

impl ClusterRegistration {
	pub fn new(node: String, broadcast: BroadcastProducer) -> Self {
		tracing::info!(%node, "registered cluster client");
		ClusterRegistration { node, broadcast }
	}
}
impl Drop for ClusterRegistration {
	fn drop(&mut self) {
		tracing::info!(%self.node, "unregistered cluster client");
		let _ = self.broadcast.abort(moq_lite::Error::Cancel);
	}
}
