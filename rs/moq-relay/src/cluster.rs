use std::{collections::HashMap, path::PathBuf};

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

	// Broadcasts announced by local clients (users).
	pub primary: OriginProducer,

	// Broadcasts announced by remote servers (cluster).
	pub secondary: OriginProducer,

	// Broadcasts announced by local clients and remote servers.
	pub combined: OriginProducer,
}

impl Cluster {
	pub fn new(config: ClusterConfig, client: moq_native::Client) -> Self {
		Cluster {
			config,
			client,
			primary: Origin::produce(),
			secondary: Origin::produce(),
			combined: Origin::produce(),
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
		let subscribe_origin = subscribe_origin.with_root(&token.root)?;
		subscribe_origin.consume_only(&token.subscribe)
	}

	// For a given auth token, return the origin that should be used for the session.
	pub fn publisher(&self, token: &AuthToken) -> Option<OriginProducer> {
		// If this is a cluster node, then add its broadcasts to the secondary origin.
		// That way we won't publish them to other cluster nodes.
		let publish_origin = match token.cluster {
			true => &self.secondary,
			false => &self.primary,
		};

		let publish_origin = publish_origin.with_root(&token.root)?;
		publish_origin.publish_only(&token.publish)
	}

	// Register a cluster node's presence.
	//
	// Returns a [ClusterRegistration] that should be kept alive for the duration of the session.
	pub fn register(&self, token: &AuthToken) -> Option<ClusterRegistration> {
		let node = token.register.clone()?;
		let broadcast = Broadcast::produce();

		let path = moq_lite::Path::new(&self.config.prefix).join(&node);
		self.primary.publish_broadcast(path, broadcast.consume());

		Some(ClusterRegistration::new(node, broadcast))
	}

	pub fn get(&self, broadcast: &str) -> Option<BroadcastConsumer> {
		self.primary
			.consume_broadcast(broadcast)
			.or_else(|| self.secondary.consume_broadcast(broadcast))
	}

	pub async fn run(self) -> anyhow::Result<()> {
		// If we're using a root node, then we have to connect to it.
		// Otherwise, we're the root node so we wait for other nodes to connect to us.
		let Some(root) = self
			.config
			.root
			.clone()
			.filter(|connect| Some(connect) != self.config.node.as_ref())
		else {
			tracing::info!("running as root, accepting leaf nodes");
			self.run_combined().await?;
			anyhow::bail!("combined connection closed");
		};

		// Subscribe to available origins from secondary (what we learn from other nodes).
		// Use with_root to automatically strip the prefix from announced paths.
		let origins = self
			.secondary
			.with_root(&self.config.prefix)
			.context("no authorized origins")?;

		// If the token is provided, read it from the disk and use it in the query parameter.
		// TODO put this in an AUTH header once WebTransport supports it.
		let token = match &self.config.token {
			Some(path) => std::fs::read_to_string(path)
				.context("failed to read token")?
				.trim()
				.to_string(),
			None => "".to_string(),
		};

		let local = self.config.node.clone().context("missing node")?;

		// Create a dummy broadcast that we don't close so run_remote doesn't close.
		let noop = Broadcast::produce();

		// Despite returning a Result, we should NEVER return an Ok
		tokio::select! {
			res = self.clone().run_remote(&root, Some(local.as_str()), token.clone(), noop.consume()) => {
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
		let mut primary = self.primary.consume();
		let mut secondary = self.secondary.consume();

		loop {
			let (name, broadcast) = tokio::select! {
				biased;
				Some(primary) = primary.announced() => primary,
				Some(secondary) = secondary.announced() => secondary,
				else => return Ok(()),
			};

			if let Some(broadcast) = broadcast {
				self.combined.publish_broadcast(&name, broadcast);
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
			if self.config.node.as_deref() == Some(node.as_str()) {
				// Skip ourselves.
				continue;
			}

			let Some(origin) = origin else {
				tracing::info!(%node, "origin cancelled");
				active.remove(node.as_str()).unwrap().abort();
				continue;
			};

			tracing::info!(%node, "discovered origin");

			let this = self.clone();
			let token = token.clone();
			let node2 = node.clone();

			let handle = tokio::spawn(
				async move {
					match this.run_remote(node2.as_str(), None, token, origin).await {
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

	#[tracing::instrument("remote", skip_all, err, fields(%remote))]
	async fn run_remote(
		mut self,
		remote: &str,
		register: Option<&str>,
		token: String,
		origin: BroadcastConsumer,
	) -> anyhow::Result<()> {
		let mut url = Url::parse(&format!("https://{remote}/"))?;
		{
			let mut q = url.query_pairs_mut();
			if !token.is_empty() {
				q.append_pair("jwt", &token);
			}
			if let Some(register) = register {
				q.append_pair("register", register);
			}
		}
		let mut backoff = 1;

		loop {
			let res = tokio::select! {
				biased;
				_ = origin.closed() => break,
				res = self.run_remote_once(&url) => res,
			};

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

		Ok(())
	}

	async fn run_remote_once(&mut self, url: &Url) -> anyhow::Result<()> {
		let mut log_url = url.clone();
		log_url.set_query(None);
		tracing::info!(url = %log_url, "connecting to remote");

		let session = self
			.client
			.clone()
			.with_publish(self.primary.consume())
			.with_consume(self.secondary.clone())
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
		self.broadcast.close();
	}
}
