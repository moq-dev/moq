//! The consuming half: typed readers over one published stats broadcast.

use std::collections::BTreeMap;
use std::task::Poll;

use moq_net::stats::{Role, Tier};
use moq_net::{broadcast, kio, track};

use crate::{Format, Result, SessionsFrame, TrafficFrame, fb, sessions_track, traffic_track};

/// Configuration for a [`Consumer`]. Construct with [`Config::new`]
/// and chain the `with_*` setters.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Config {
	/// Which flavor of each track to read. Every flavor decodes to the same
	/// frames; the compressed ones cost a fraction of the bytes. Defaults to
	/// [`Format::Json`].
	pub format: Format,
}

impl Config {
	/// A config with default settings: the plain `.json` tracks.
	pub fn new() -> Self {
		Self::default()
	}

	/// Read the given flavor of each track.
	pub fn with_format(mut self, format: Format) -> Self {
		self.format = format;
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
	config: Config,
}

impl Consumer {
	/// Wrap a stats broadcast. The broadcast is whatever the announce at a
	/// stats path resolved to; parse the path with [`crate::parse_node_path`].
	pub fn new(broadcast: broadcast::Consumer, config: Config) -> Self {
		Self { broadcast, config }
	}

	/// Subscribe to the traffic track for `(tier, role)`, awaiting the
	/// subscription handshake.
	pub async fn traffic(&self, tier: &Tier, role: Role) -> Result<Traffic> {
		let name = traffic_track(tier, role, self.config.format);
		Ok(Traffic {
			inner: self.subscribe(&name).await?,
		})
	}

	/// Subscribe to the sessions track for `tier`, awaiting the subscription
	/// handshake.
	pub async fn sessions(&self, tier: &Tier) -> Result<Sessions> {
		let name = sessions_track(tier, self.config.format);
		Ok(Sessions {
			inner: self.subscribe(&name).await?,
		})
	}

	async fn subscribe<V: Value>(&self, name: &str) -> Result<Reader<V>> {
		let track = self.broadcast.track(name)?.subscribe(None).await?;
		Ok(Reader::new(track, self.config.format))
	}
}

/// A typed reader over one traffic track. Yields the latest [`TrafficFrame`];
/// intermediate frames a slow reader missed are collapsed, which is safe
/// because the counters are cumulative.
pub struct Traffic {
	inner: Reader<moq_net::stats::Traffic>,
}

impl Traffic {
	/// The next frame, or `None` once the track ends (the producer went away).
	pub async fn next(&mut self) -> Result<Option<TrafficFrame>> {
		kio::wait(|waiter| self.inner.poll_next(waiter)).await
	}
}

/// A typed reader over one sessions track; see [`Traffic`].
pub struct Sessions {
	inner: Reader<moq_net::stats::Presence>,
}

impl Sessions {
	/// The next frame, or `None` once the track ends (the producer went away).
	pub async fn next(&mut self) -> Result<Option<SessionsFrame>> {
		kio::wait(|waiter| self.inner.poll_next(waiter)).await
	}
}

/// A frame value every flavor can carry: [`Traffic`](moq_net::stats::Traffic)
/// or [`Presence`](moq_net::stats::Presence).
pub(crate) trait Value: serde::de::DeserializeOwned + fb::Entry {}

impl<V: serde::de::DeserializeOwned + fb::Entry> Value for V {}

/// Reads one stats track in whichever flavor it was subscribed as, yielding
/// the newest frame.
pub(crate) enum Reader<V: Value> {
	Json(moq_json::snapshot::Consumer<BTreeMap<String, V>>),
	FlatBuffers(fb::Reader<V>),
}

impl<V: Value> Reader<V> {
	pub fn new(track: track::Subscriber, format: Format) -> Self {
		let mut config = moq_json::snapshot::consumer::Config::default();
		match format {
			Format::Json => {}
			Format::CompressedJson => config.compression = moq_json::Compression::Deflate,
			Format::FlatBuffers => return Self::FlatBuffers(fb::Reader::new(track)),
		}
		Self::Json(moq_json::snapshot::Consumer::new(track, config))
	}

	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<BTreeMap<String, V>>>> {
		match self {
			Self::Json(reader) => reader.poll_next(waiter).map_err(Into::into),
			Self::FlatBuffers(reader) => reader.poll_next(waiter),
		}
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

		let update = announced.next().await.expect("announce");
		assert!(update.kind.is_active());
		let consumer = egress.request_broadcast(path).await.expect("resolve");
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
		let mut consumer = origin.consume().announced();
		tokio::time::advance(Duration::from_millis(1)).await;
		let update = consumer.next().await.expect("expected announce");
		assert!(update.kind.is_active());
		origin
			.consume()
			.request_broadcast(moq_net::Path::new(update.prefix.as_str()))
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
	async fn every_format_round_trips() {
		// The same drain must decode identically off every flavor, including
		// across an update (the compressed tracks' shared-window path). The
		// `.fb.z` track does not exist until requested, so its subscribe
		// resolves on the next drain.
		let (producer, origin) = test_producer();
		let tier = Tier::default();
		let mut fed = feed(&producer, tier.clone(), "acme", "foo/bar").await;
		fed.write(42).await;

		drive_tick().await;

		let broadcast = announced(&origin).await;
		let plain = Consumer::new(broadcast.consume(), Config::new());
		let compressed = Consumer::new(broadcast.consume(), Config::new().with_format(Format::CompressedJson));
		let binary = Consumer::new(broadcast.consume(), Config::new().with_format(Format::FlatBuffers));

		let mut plain_traffic = plain.traffic(&tier, Role::Publisher).await.expect("subscribe plain");
		let mut z_traffic = compressed
			.traffic(&tier, Role::Publisher)
			.await
			.expect("subscribe compressed");
		let (fb_traffic, fb_sessions, _) = tokio::join!(
			binary.traffic(&tier, Role::Publisher),
			binary.sessions(&tier),
			drive_tick()
		);
		let mut fb_traffic = fb_traffic.expect("subscribe flatbuffers");
		let mut fb_sessions = fb_sessions.expect("subscribe flatbuffers sessions");

		let plain_frame = plain_traffic.next().await.expect("read").expect("frame");
		let z_frame = z_traffic.next().await.expect("read").expect("frame");
		let fb_frame = fb_traffic.next().await.expect("read").expect("frame");
		assert_eq!(plain_frame, z_frame, "both JSON flavors carry the same data");
		assert_eq!(plain_frame, fb_frame, "FlatBuffers carries the same data");
		assert_eq!(plain_frame.get("foo/bar").expect("entry").bytes, 42);

		// A later drain updates every flavor inside the open windows.
		fed.write(8).await;
		drive_tick().await;
		let plain_frame = plain_traffic.next().await.expect("read").expect("frame");
		let z_frame = z_traffic.next().await.expect("read").expect("frame");
		let fb_frame = fb_traffic.next().await.expect("read").expect("frame");
		assert_eq!(plain_frame.get("foo/bar").expect("entry").bytes, 50);
		assert_eq!(plain_frame, z_frame, "delta reconstructs the same frame");
		assert_eq!(plain_frame, fb_frame, "FlatBuffers carries the update");

		let mut sessions = compressed.sessions(&tier).await.expect("subscribe sessions");
		let frame = sessions.next().await.expect("read").expect("frame");
		assert_eq!(frame.get("acme").expect("root").active(), 1);
		let fb_frame = fb_sessions.next().await.expect("read").expect("frame");
		assert_eq!(frame, fb_frame, "sessions agree across flavors");
	}
}
