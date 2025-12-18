use std::{collections::HashMap, path::PathBuf, sync::Arc};

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
	/// Connect to this hostname in order to discover other nodes.
	#[serde(alias = "connect")]
	#[arg(
		id = "cluster-root",
		long = "cluster-root",
		env = "MOQ_CLUSTER_ROOT",
		alias = "cluster-connect"
	)]
	pub root: Option<String>,

	/// Use the token in this file when connecting to other nodes.
	#[arg(id = "cluster-token", long = "cluster-token", env = "MOQ_CLUSTER_TOKEN")]
	pub token: Option<PathBuf>,

	/// Our hostname which we advertise to other nodes.
	///
	// TODO Remove alias once we've migrated to the new name.
	#[serde(alias = "advertise")]
	#[arg(
		id = "cluster-node",
		long = "cluster-node",
		env = "MOQ_CLUSTER_NODE",
		alias = "cluster-advertise"
	)]
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

	// Advertises ourselves as an origin to other nodes.
	noop: moq_lite::Produce<BroadcastProducer, BroadcastConsumer>,

	// Broadcasts announced by local clients (users).
	pub primary: Arc<moq_lite::Produce<OriginProducer, OriginConsumer>>,

	// Broadcasts announced by remote servers (cluster).
	pub secondary: Arc<moq_lite::Produce<OriginProducer, OriginConsumer>>,

	// Broadcasts announced by local clients and remote servers.
	pub combined: Arc<moq_lite::Produce<OriginProducer, OriginConsumer>>,
}

impl Cluster {
	pub fn new(config: ClusterConfig, client: moq_native::Client) -> Self {
		Cluster {
			config,
			client,
			noop: Broadcast::produce(),
			primary: Arc::new(Origin::produce()),
			secondary: Arc::new(Origin::produce()),
			combined: Arc::new(Origin::produce()),
		}
	}

	// For a given auth token, return the origin that should be used for the session.
	pub fn subscriber(&self, token: &AuthToken) -> Option<OriginConsumer> {
		// These broadcasts will be served to the session (when it subscribes).
		// If this is a cluster node, then only publish our primary broadcasts.
		// Otherwise publish everything.
		let subscribe_origin = match token.cluster {
			true => &self.primary,
			false => &self.combined,
		};

		// Scope the origin to our root.
		let subscribe_origin = subscribe_origin.producer.with_root(&token.root)?;
		subscribe_origin.consume_only(&token.subscribe)
	}

	pub fn publisher(&self, token: &AuthToken) -> Option<OriginProducer> {
		// If this is a cluster node, then add its broadcasts to the secondary origin.
		// That way we won't publish them to other cluster nodes.
		let publish_origin = match token.cluster {
			true => &self.secondary,
			false => &self.primary,
		};

		let publish_origin = publish_origin.producer.with_root(&token.root)?;
		publish_origin.publish_only(&token.publish)
	}

	pub fn get(&self, broadcast: &str) -> Option<BroadcastConsumer> {
		self.primary
			.consumer
			.consume_broadcast(broadcast)
			.or_else(|| self.secondary.consumer.consume_broadcast(broadcast))
	}

	pub async fn run(self) -> anyhow::Result<()> {
		let root = match self.config.root.clone() {
			// If we're using a root node, then we have to connect to it.
			Some(connect) if Some(&connect) != self.config.node.as_ref() => connect,
			// Otherwise, we're the root node so we wait for other nodes to connect to us.
			_ => {
				tracing::info!("running as root, accepting leaf nodes");
				self.run_combined().await?;
				anyhow::bail!("combined connection closed");
			}
		};

		// Subscribe to available origins.
		// Use with_root to automatically strip the prefix from announced paths.
		let origins = self
			.secondary
			.producer
			.with_root(&self.config.prefix)
			.context("no authorized origins")?;

		// Announce ourselves as an origin to the root node.
		if let Some(myself) = self.config.node.as_ref() {
			tracing::info!(%myself, "announcing as leaf");
			origins.publish_broadcast(myself, self.noop.consumer.clone());
		}

		// If the token is provided, read it from the disk and use it in the query parameter.
		// TODO put this in an AUTH header once WebTransport supports it.
		let token = match &self.config.token {
			Some(path) => std::fs::read_to_string(path).context("failed to read token")?,
			None => "".to_string(),
		};

		let noop = self.noop.consumer.clone();

		// Despite returning a Result, we should NEVER return an Ok
		tokio::select! {
			res = self.clone().run_remote(&root, token.clone(), noop) => {
				res.context("failed to connect to root")?;
				anyhow::bail!("connection to root closed");
			}
			res = self.clone().run_remotes(origins.consume(), token) => {
				res.context("failed to connect to remotes")?;
				anyhow::bail!("connection to remotes closed");
			}
			res = self.run_combined() => {
				res.context("failed to run combined")?;
				anyhow::bail!("combined connection closed");
			}
		}
	}

	// Shovel broadcasts from the primary and secondary origins into the combined origin.
	async fn run_combined(self) -> anyhow::Result<()> {
		let mut primary = self.primary.consumer.consume();
		let mut secondary = self.secondary.consumer.consume();

		loop {
			let (name, broadcast) = tokio::select! {
				biased;
				Some(primary) = primary.announced() => primary,
				Some(secondary) = secondary.announced() => secondary,
				else => return Ok(()),
			};

			if let Some(broadcast) = broadcast {
				self.combined.producer.publish_broadcast(&name, broadcast);
			}
		}
	}

	async fn run_remotes(self, mut origins: OriginConsumer, token: String) -> anyhow::Result<()> {
		// Cancel tasks when the origin is closed.
		let mut active: HashMap<String, tokio::task::AbortHandle> = HashMap::new();

		// Discover other origins.
		// NOTE: The root node will connect to all other nodes as a client, ignoring the existing (server) connection.
		// This ensures that nodes are advertising a valid hostname before any tracks get announced.
		while let Some((node, origin)) = origins.announced().await {
			if Some(node.as_str()) == self.config.node.as_deref() {
				// Skip ourselves.
				continue;
			}

			let origin = match origin {
				Some(origin) => origin,
				None => {
					tracing::info!(%node, "origin cancelled");
					active.remove(node.as_str()).unwrap().abort();
					continue;
				}
			};

			tracing::info!(%node, "discovered origin");

			let this = self.clone();
			let token = token.clone();
			let node2 = node.clone();

			let handle = tokio::spawn(
				async move {
					match this.run_remote(node2.as_str(), token, origin).await {
						Ok(()) => tracing::info!(%node2, "origin closed"),
						Err(err) => tracing::warn!(%err, %node2, "origin error"),
					}
				}
				.in_current_span(),
			);

			active.insert(node.to_string(), handle.abort_handle());
		}

		Ok(())
	}

	#[tracing::instrument("remote", skip_all, err, fields(%node))]
	async fn run_remote(mut self, node: &str, token: String, origin: BroadcastConsumer) -> anyhow::Result<()> {
		let url = Url::parse(&format!("https://{node}/?jwt={token}"))?;
		let mut backoff = 1;

		loop {
			let res = tokio::select! {
				biased;
				_ = origin.closed() => break,
				res = self.run_remote_once(&url) => res,
			};

			if let Err(err) = res {
				backoff *= 2;
				tracing::error!(%err, "remote error");
			}

			let timeout = tokio::time::Duration::from_secs(backoff);
			if timeout > tokio::time::Duration::from_secs(300) {
				// 5 minutes of backoff is enough, just give up.
				// TODO Reset the backoff if the connect is successful for some period of time.
				anyhow::bail!("remote connection keep failing, giving up");
			}

			tokio::time::sleep(timeout).await;
		}

		Ok(())
	}

	async fn run_remote_once(&mut self, url: &Url) -> anyhow::Result<()> {
		tracing::info!(%url, "connecting to remote");

		// Connect to the remote node.
		let conn = self
			.client
			.connect(url.clone())
			.await
			.context("failed to connect to remote")?;

		let publish = Some(self.primary.consumer.consume());
		let subscribe = Some(self.secondary.producer.clone());

		let session = moq_lite::Session::connect(conn, publish, subscribe)
			.await
			.context("failed to establish session")?;

		session.closed().await.map_err(Into::into)
	}
}
