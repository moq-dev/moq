use std::path::PathBuf;

use anyhow::Context;
use moq_lite::{BroadcastConsumer, Origin, OriginProducer};
use tracing::Instrument;
use url::Url;

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

	pub fn get(&self, broadcast: &str) -> Option<BroadcastConsumer> {
		self.origin.consume_broadcast(broadcast)
	}

	pub async fn run(self) -> anyhow::Result<()> {
		if self.config.connect.is_empty() {
			return Ok(());
		}

		// If the token is provided, read it from the disk and use it in the query parameter.
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

		while let Some(res) = tasks.join_next().await {
			res??;
		}

		Ok(())
	}

	#[tracing::instrument("remote", skip_all, err, fields(%remote))]
	async fn run_remote(self, remote: &str, token: String) -> anyhow::Result<()> {
		let mut url = Url::parse(&format!("https://{remote}/"))?;
		if !token.is_empty() {
			url.query_pairs_mut().append_pair("jwt", &token);
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
				anyhow::bail!("remote connection keep failing, giving up");
			}

			tokio::time::sleep(timeout).await;
		}
	}

	async fn run_remote_once(&self, url: &Url) -> anyhow::Result<()> {
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
