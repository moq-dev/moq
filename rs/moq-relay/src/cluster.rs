use moq_lite::{BroadcastConsumer, Origin, OriginProducer};

#[derive(clap::Args, Clone, Debug, serde::Serialize, serde::Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct ClusterConfig {}

#[derive(Clone)]
pub struct Cluster {
	// All broadcasts, both local and remote.
	// Hops-based routing ensures the shortest path is preferred.
	pub origin: OriginProducer,
}

impl Cluster {
	pub fn new(_config: ClusterConfig) -> Self {
		Cluster {
			origin: Origin::produce(),
		}
	}

	pub fn get(&self, broadcast: &str) -> Option<BroadcastConsumer> {
		self.origin.consume_broadcast(broadcast)
	}

	pub async fn run(self) -> anyhow::Result<()> {
		// The cluster currently only accepts incoming connections.
		// There's no active work to do, so return immediately.
		Ok(())
	}
}
