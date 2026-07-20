//! The consuming half: typed readers over one published stats broadcast.

use moq_net::broadcast;
use moq_net::stats::{Role, Tier};

use crate::{Result, SessionsFrame, TrafficFrame, sessions_track, traffic_track};

/// Configuration for a [`Consumer`]. Construct with [`ConsumerConfig::new`]
/// and chain the `with_*` setters.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ConsumerConfig {
	/// Read the compressed `.json.z` tracks instead of the plain `.json` ones.
	/// Same data for a fraction of the bytes, but requires a producer that
	/// publishes them. Defaults to `false`.
	pub compression: bool,
}

impl ConsumerConfig {
	/// A config with default settings: the plain `.json` tracks.
	pub fn new() -> Self {
		Self::default()
	}

	/// Read the compressed `.json.z` tracks instead of the plain `.json` ones.
	pub fn with_compression(mut self, compression: bool) -> Self {
		self.compression = compression;
		self
	}
}

/// Reads one published stats broadcast (a `<prefix>/node/<node>` announce),
/// yielding typed frames per track.
///
/// Subscribe to the traffic and session tracks you care about with
/// [`Self::traffic`] / [`Self::sessions`]; a track that the producer never
/// created (e.g. a named tier that saw no traffic) fails to subscribe or ends
/// immediately, so callers typically subscribe the tiers they know exist.
pub struct Consumer {
	broadcast: broadcast::Consumer,
	config: ConsumerConfig,
}

impl Consumer {
	/// Wrap a stats broadcast. The broadcast is whatever the announce at a
	/// stats path resolved to; parse the path with [`crate::parse_node_path`].
	pub fn new(broadcast: broadcast::Consumer, config: ConsumerConfig) -> Self {
		Self { broadcast, config }
	}

	/// Subscribe to the traffic track for `(tier, role)`, awaiting the
	/// subscription handshake.
	pub async fn traffic(&self, tier: &Tier, role: Role) -> Result<TrafficConsumer> {
		let name = traffic_track(tier, role, self.config.compression);
		Ok(TrafficConsumer {
			inner: self.subscribe(&name).await?,
		})
	}

	/// Subscribe to the sessions track for `tier`, awaiting the subscription
	/// handshake.
	pub async fn sessions(&self, tier: &Tier) -> Result<SessionsConsumer> {
		let name = sessions_track(tier, self.config.compression);
		Ok(SessionsConsumer {
			inner: self.subscribe(&name).await?,
		})
	}

	async fn subscribe<T: serde::de::DeserializeOwned>(&self, name: &str) -> Result<moq_json::snapshot::Consumer<T>> {
		let track = self.broadcast.track(name)?.subscribe(None).await?;
		let config = moq_json::snapshot::ConsumerConfig::default().with_compression(self.config.compression);
		Ok(moq_json::snapshot::Consumer::new(track, config))
	}
}

/// A typed reader over one traffic track. Yields the latest [`TrafficFrame`];
/// intermediate frames a slow reader missed are collapsed, which is safe
/// because the counters are cumulative.
pub struct TrafficConsumer {
	inner: moq_json::snapshot::Consumer<TrafficFrame>,
}

impl TrafficConsumer {
	/// The next frame, or `None` once the track ends (the producer went away).
	pub async fn next(&mut self) -> Result<Option<TrafficFrame>> {
		Ok(self.inner.next().await?)
	}
}

/// A typed reader over one sessions track; see [`TrafficConsumer`].
pub struct SessionsConsumer {
	inner: moq_json::snapshot::Consumer<SessionsFrame>,
}

impl SessionsConsumer {
	/// The next frame, or `None` once the track ends (the producer went away).
	pub async fn next(&mut self) -> Result<Option<SessionsFrame>> {
		Ok(self.inner.next().await?)
	}
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use moq_net::{Consume, Origin, PathOwned, announce, origin};

	use crate::{Producer, ProducerConfig, Tier};

	use super::*;

	fn test_producer() -> (Producer, origin::Producer) {
		let origin = Origin::random().produce();
		let producer = Producer::new(
			ProducerConfig::new()
				.with_origin(origin.clone())
				.with_node(PathOwned::from("sjc")),
		);
		(producer, origin)
	}

	async fn announced(origin: &origin::Producer) -> moq_net::broadcast::Consumer {
		let mut consumer = origin.consume().announced();
		tokio::time::advance(Duration::from_millis(1)).await;
		let announce::Update { broadcast, .. } = consumer.next().await.expect("expected announce");
		broadcast.expect("active")
	}

	async fn drive_tick() {
		tokio::time::advance(Duration::from_millis(1100)).await;
		for _ in 0..4 {
			tokio::task::yield_now().await;
		}
	}

	#[tokio::test(start_paused = true)]
	async fn plain_and_compressed_round_trip() {
		// The same drain must decode identically off the plain track and the
		// compressed sibling, including across an update (the compressed
		// track's delta path).
		let (producer, origin) = test_producer();
		let tier = Tier::default();
		let stats = producer.registry().tier(tier.clone());
		let bs = stats.broadcast("foo/bar");
		let track = bs.publisher().track("video");
		track.bytes(42);
		let _session = stats.session("acme");

		drive_tick().await;

		let broadcast = announced(&origin).await;
		let plain = Consumer::new(broadcast.consume(), ConsumerConfig::new());
		let compressed = Consumer::new(broadcast.consume(), ConsumerConfig::new().with_compression(true));

		let mut plain_traffic = plain.traffic(&tier, Role::Publisher).await.expect("subscribe plain");
		let mut z_traffic = compressed
			.traffic(&tier, Role::Publisher)
			.await
			.expect("subscribe compressed");

		let plain_frame = plain_traffic.next().await.expect("read").expect("frame");
		let z_frame = z_traffic.next().await.expect("read").expect("frame");
		assert_eq!(plain_frame, z_frame, "both flavors carry the same data");
		assert_eq!(plain_frame.get("foo/bar").expect("entry").bytes, 42);

		// A later drain updates both flavors; the compressed one rides a delta.
		track.bytes(8);
		drive_tick().await;
		let plain_frame = plain_traffic.next().await.expect("read").expect("frame");
		let z_frame = z_traffic.next().await.expect("read").expect("frame");
		assert_eq!(plain_frame.get("foo/bar").expect("entry").bytes, 50);
		assert_eq!(plain_frame, z_frame, "delta reconstructs the same frame");

		let mut sessions = compressed.sessions(&tier).await.expect("subscribe sessions");
		let frame = sessions.next().await.expect("read").expect("frame");
		assert_eq!(frame.get("acme").expect("root").active(), 1);
	}
}
