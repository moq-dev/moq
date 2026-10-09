//! The consuming half: typed readers over one published stats broadcast.

use moq_net::stats::{Role, Tier};
use moq_net::{AsPath, broadcast};

use crate::{PresenceFrame, Result, TotalsFrame, TrafficFrame, presence_track, totals_track, traffic_track};

/// Configuration for a [`Consumer`]. Construct with [`Config::new`]
/// and chain the `with_*` setters.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Config {
	/// Read the compressed `.json.z` tracks instead of the plain `.json` ones.
	/// Same data for a fraction of the bytes, but requires a producer that
	/// publishes them. Defaults to `false`.
	pub compression: bool,
}

impl Config {
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
/// Subscribe to the tracks you care about with [`Self::totals`],
/// [`Self::traffic`], and [`Self::presence`]. A named tier's traffic track is
/// held open with an empty frame until the tier records, and a root's presence
/// track is produced only while subscribed.
pub struct Consumer {
	broadcast: broadcast::Consumer,
	config: Config,
}

impl Consumer {
	/// Wrap a stats broadcast. The broadcast is whatever the announce at a
	/// stats path resolved to; parse the path with [`crate::parse_node_path`].
	pub fn new(broadcast: broadcast::Consumer, config: Config) -> Self {
		Self { broadcast, config }
	}

	/// Subscribe to the totals track, awaiting the subscription handshake.
	pub async fn totals(&self) -> Result<Totals> {
		let name = totals_track(self.config.compression);
		Ok(Totals {
			inner: self.subscribe(&name).await?,
		})
	}

	/// Subscribe to the traffic track for `(tier, role)`, awaiting the
	/// subscription handshake.
	pub async fn traffic(&self, tier: &Tier, role: Role) -> Result<Traffic> {
		let name = traffic_track(tier, role, self.config.compression);
		Ok(Traffic {
			inner: self.subscribe(&name).await?,
		})
	}

	/// Subscribe to the presence track for auth `root`, awaiting the
	/// subscription handshake. Fails with
	/// [`TooManyRequests`](moq_net::Error::TooManyRequests) while the producer
	/// holds its cap of presence tracks; retry later.
	pub async fn presence(&self, root: impl AsPath) -> Result<Presence> {
		let name = presence_track(root, self.config.compression);
		Ok(Presence {
			inner: self.subscribe(&name).await?,
		})
	}

	async fn subscribe<T: serde::de::DeserializeOwned>(&self, name: &str) -> Result<moq_json::snapshot::Consumer<T>> {
		let track = self.broadcast.track(name)?.subscribe(None).await?;
		let mut config = moq_json::snapshot::consumer::Config::default();
		if self.config.compression {
			config.compression = moq_json::Compression::Deflate;
		}
		Ok(moq_json::snapshot::Consumer::new(track, config))
	}
}

/// A typed reader over one traffic track. Yields the latest [`TrafficFrame`];
/// intermediate frames a slow reader missed are collapsed, which is safe
/// because the counters are cumulative.
pub struct Traffic {
	inner: moq_json::snapshot::Consumer<TrafficFrame>,
}

impl Traffic {
	/// The next frame, or `None` once the track ends (the producer went away).
	pub async fn next(&mut self) -> Result<Option<TrafficFrame>> {
		Ok(self.inner.next().await?)
	}
}

/// A typed reader over the totals track; see [`Traffic`].
pub struct Totals {
	inner: moq_json::snapshot::Consumer<TotalsFrame>,
}

impl Totals {
	/// The next frame, or `None` once the track ends (the producer went away).
	pub async fn next(&mut self) -> Result<Option<TotalsFrame>> {
		Ok(self.inner.next().await?)
	}
}

/// A typed reader over one root's presence track; see [`Traffic`].
pub struct Presence {
	inner: moq_json::snapshot::Consumer<PresenceFrame>,
}

impl Presence {
	/// The next frame, or `None` once the track ends (the producer went away
	/// or its group unannounced).
	pub async fn next(&mut self) -> Result<Option<PresenceFrame>> {
		Ok(self.inner.next().await?)
	}
}

#[cfg(test)]
mod tests {
	/// Build an origin producer, spawning its driver on the ambient runtime.
	fn produce_origin() -> moq_net::origin::Producer {
		let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::default());
		if tokio::runtime::Handle::try_current().is_ok() {
			tokio::spawn(moq_net::time::run(driver));
		} else {
			// A sync test: nothing polls the driver, and dropping it would tear
			// the origin down, so leak it and rely on the synchronous half.
			std::mem::forget(driver);
		}
		producer
	}

	/// The next route and whether it is active.
	async fn next_update(announced: &mut moq_net::announce::Consumer) -> Option<(moq_net::announce::Announce, bool)> {
		match announced.next().await? {
			moq_net::announce::Event::Start(route) | moq_net::announce::Event::Update(route) => Some((route, true)),
			moq_net::announce::Event::End(route) => Some((route, false)),
		}
	}

	use std::time::Duration;

	use moq_net::{Consume, PathOwned, Timestamp, announce, broadcast, origin, track};

	use crate::{Producer, Tier, produce};

	use super::*;

	fn test_producer() -> (Producer, origin::Producer) {
		let origin = produce_origin();
		let producer = Producer::new(
			produce::Config::new()
				.with_origin(origin.clone())
				.with_node(PathOwned::from("sjc")),
		);
		(producer, origin)
	}

	/// A tagged egress feed into a producer's registry, holding the handles needed
	/// to write more traffic incrementally. Presence is recorded under `root`.
	struct Feed {
		track: track::Producer,
		sub: track::Subscriber,
		_announced: announce::Consumer,
		_source: broadcast::Producer,
		_ctx: moq_net::stats::Session,
	}

	impl Feed {
		/// Write one frame of `bytes` bytes into the broadcast and read it out on the
		/// egress side, so the publisher `bytes`/`frames`/`groups` counters advance.
		async fn write(&mut self, bytes: usize) {
			let mut group = self.track.append_group().unwrap();
			group.write_frame(Timestamp::ZERO, vec![0u8; bytes]).unwrap();
			group.finish().unwrap();
			let mut group = self.sub.recv_group().await.unwrap().unwrap();
			while group.read_frame().await.unwrap().is_some() {}
		}
	}

	async fn feed(producer: &Producer, tier: Tier, root: &str, path: &str) -> Feed {
		let ctx = producer.registry().tier(tier).session(root);
		let feed_origin = produce_origin();
		let egress = feed_origin.consume().with_stats(ctx.clone());

		let mut announced = egress.announced();
		let source = feed_origin.create_broadcast(path).unwrap();
		source.announce(origin::Route::default()).unwrap();
		let track = source.clone().create_track("video", None).unwrap();

		let (_, active) = next_update(&mut announced).await.expect("announce");
		assert!(active);
		let consumer = egress.request_broadcast(path, None).await.expect("resolve");
		let sub = consumer.track("video").unwrap().subscribe(None).await.unwrap();

		Feed {
			track,
			sub,
			_announced: announced,
			_source: source,
			_ctx: ctx,
		}
	}

	async fn announced(origin: &origin::Producer) -> moq_net::broadcast::Consumer {
		let mut consumer = origin.consume().with_hidden(true).announced();
		tokio::time::advance(Duration::from_millis(1)).await;
		let (update, active) = next_update(&mut consumer).await.expect("expected announce");
		assert!(active);
		origin
			.consume()
			.request_broadcast(moq_net::Path::new(update.prefix.as_str()), None)
			.await
			.expect("resolve")
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
		let mut fed = feed(&producer, tier.clone(), "acme", "foo/bar").await;
		fed.write(42).await;

		drive_tick().await;

		let broadcast = announced(&origin).await;
		let plain = Consumer::new(broadcast.consume(), Config::new());
		let compressed = Consumer::new(broadcast.consume(), Config::new().with_compression(true));

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
		fed.write(8).await;
		drive_tick().await;
		let plain_frame = plain_traffic.next().await.expect("read").expect("frame");
		let z_frame = z_traffic.next().await.expect("read").expect("frame");
		assert_eq!(plain_frame.get("foo/bar").expect("entry").bytes, 50);
		assert_eq!(plain_frame, z_frame, "delta reconstructs the same frame");

		let mut totals = compressed.totals().await.expect("subscribe totals");
		let frame = totals.next().await.expect("read").expect("frame");
		let totals = frame.get("").expect("default tier");
		assert_eq!(totals.publisher.bytes, 50);
		assert_eq!(totals.sessions.active(), 1);

		// A presence track is produced once requested, so drive a drain.
		let mut presence = std::pin::pin!(compressed.presence("acme"));
		assert!(futures::poll!(presence.as_mut()).is_pending());
		drive_tick().await;
		let mut presence = presence.await.expect("subscribe presence");
		let frame = presence.next().await.expect("read").expect("frame");
		assert_eq!(frame.get("").expect("default tier").active(), 1);
	}
}
