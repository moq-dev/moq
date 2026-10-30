//! The consuming half: typed readers over one published stats broadcast.

use moq_net::{Path, broadcast};

use crate::{
	GroupFrame, PrefixFrame, PresenceFrame, Result, TotalsFrame, group_track, prefix_track, presence_track,
	totals_track,
};

/// Configuration for a [`Consumer`]. Empty for now; construct with
/// [`Config::new`]. The struct is `#[non_exhaustive]`, so new options stay
/// additive.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Config {}

impl Config {
	/// A config with default settings.
	pub fn new() -> Self {
		Self::default()
	}
}

/// Reads one published stats broadcast (a `<prefix>/node/<node>` announce),
/// yielding typed frames per track.
///
/// Subscribe to the tracks you care about with [`Self::totals`],
/// [`Self::group`], [`Self::presence`], and [`Self::prefix`]. A label's
/// presence track and a prefix's track are produced only while subscribed.
/// At depth 0 the one group is the empty one; otherwise discover live groups
/// from the broadcast's [track listing](moq_net::broadcast::Consumer::tracks)
/// with [`crate::parse_group_track`].
pub struct Consumer {
	broadcast: broadcast::Consumer,
}

impl Consumer {
	/// Wrap a stats broadcast. The broadcast is whatever the announce at a
	/// stats path resolved to; parse the path with [`crate::parse_node_path`].
	pub fn new(broadcast: broadcast::Consumer, _config: Config) -> Self {
		Self { broadcast }
	}

	/// Subscribe to the totals track, awaiting the subscription handshake.
	pub async fn totals(&self) -> Result<TotalsConsumer> {
		let name = totals_track();
		Ok(TotalsConsumer {
			inner: self.subscribe(&name).await?,
		})
	}

	/// Subscribe to the presence track for `label`, awaiting the
	/// subscription handshake.
	pub async fn presence(&self, label: impl Into<Path>) -> Result<PresenceConsumer> {
		let name = presence_track(label);
		Ok(PresenceConsumer {
			inner: self.subscribe(&name).await?,
		})
	}

	/// Subscribe to the prefix track for broadcast prefix `path`, awaiting the
	/// subscription handshake.
	pub async fn prefix(&self, path: impl Into<Path>) -> Result<PrefixConsumer> {
		let name = prefix_track(path);
		Ok(PrefixConsumer {
			inner: self.subscribe(&name).await?,
		})
	}

	/// Subscribe to group `group`'s track, awaiting the subscription handshake.
	/// Fails with [`NotFound`](moq_net::Error::NotFound) unless the group is
	/// listed.
	pub async fn group(&self, group: impl Into<Path>) -> Result<GroupConsumer> {
		let track = self.broadcast.track(&group_track(group))?.subscribe(None).await?;
		Ok(GroupConsumer {
			inner: moq_json::snapshot::Consumer::new(track, Default::default()),
		})
	}

	async fn subscribe<T: serde::de::DeserializeOwned>(&self, name: &str) -> Result<moq_json::snapshot::Consumer<T>> {
		let track = self.broadcast.track(name)?.subscribe(None).await?;
		Ok(moq_json::snapshot::Consumer::new(track, Default::default()))
	}
}

/// A typed reader over the totals track. Yields the latest [`TotalsFrame`];
/// intermediate frames a slow reader missed are collapsed, which is safe
/// because the counters are cumulative.
pub struct TotalsConsumer {
	inner: moq_json::snapshot::Consumer<TotalsFrame>,
}

impl TotalsConsumer {
	/// The next frame, or `None` once the track ends (the producer went away).
	pub async fn next(&mut self) -> Result<Option<TotalsFrame>> {
		Ok(self.inner.latest().await?.map(|value| value.value))
	}
}

/// A typed reader over one label's presence track; see [`TotalsConsumer`].
pub struct PresenceConsumer {
	inner: moq_json::snapshot::Consumer<PresenceFrame>,
}

impl PresenceConsumer {
	/// The next frame, or `None` once the track ends (the producer went away).
	pub async fn next(&mut self) -> Result<Option<PresenceFrame>> {
		Ok(self.inner.latest().await?.map(|value| value.value))
	}
}

/// A typed reader over one broadcast prefix's track; see [`TotalsConsumer`].
pub struct PrefixConsumer {
	inner: moq_json::snapshot::Consumer<PrefixFrame>,
}

impl PrefixConsumer {
	/// The next frame, or `None` once the track ends (the producer went away).
	/// Fails with
	/// [`FrameTooLarge`](moq_net::Error::FrameTooLarge) once the frame outgrows
	/// the cache.
	pub async fn next(&mut self) -> Result<Option<PrefixFrame>> {
		Ok(self.inner.latest().await?.map(|value| value.value))
	}
}

/// A typed reader over one group's track; see [`TotalsConsumer`].
pub struct GroupConsumer {
	inner: moq_json::snapshot::Consumer<GroupFrame>,
}

impl GroupConsumer {
	/// The next frame, or `None` once the track ends (the producer went away
	/// or the group lingered out of the listing). Fails with
	/// [`FrameTooLarge`](moq_net::Error::FrameTooLarge) once the frame outgrows
	/// the cache.
	pub async fn next(&mut self) -> Result<Option<GroupFrame>> {
		Ok(self.inner.latest().await?.map(|value| value.value))
	}
}

#[cfg(test)]
mod tests {
	/// A millisecond track: the frames written here carry timestamps.
	fn timed() -> moq_net::track::Info {
		moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
	}

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
			moq_net::announce::Event::Start(route)
			| moq_net::announce::Event::Update(route)
			| moq_net::announce::Event::Restart(route)
			| moq_net::announce::Event::Takeover { announce: route, .. } => Some((route, true)),
			moq_net::announce::Event::End(route) => Some((route, false)),
		}
	}

	use std::time::Duration;

	use moq_net::{Consume, Path, Timestamp, announce, origin, track};

	use crate::{Producer, Tier, produce};

	use super::*;

	fn test_producer() -> (Producer, origin::Producer) {
		let origin = produce_origin();
		let producer = Producer::new(
			produce::Config::new()
				.with_origin(origin.clone())
				.with_node(Path::from_static("sjc")),
		);
		(producer, origin)
	}

	/// A tagged egress feed into a producer's registry, holding the handles needed
	/// to write more traffic incrementally. Presence is recorded under `label`.
	struct Feed {
		track: track::Producer,
		sub: track::Subscriber,
		_announced: announce::Consumer,
		_source: origin::Publication,
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

	async fn feed(producer: &Producer, tier: Tier, label: &str, path: &str) -> Feed {
		let ctx = producer
			.registry()
			.tier(tier)
			.session(moq_net::Path::new(label).unwrap());
		let feed_origin = produce_origin();
		let egress = feed_origin.consume().with_stats(ctx.clone());

		let mut announced = egress.announced();
		let source = feed_origin.create_broadcast(path).unwrap();
		source.announce(origin::Route::default()).unwrap();
		let track = source.clone().create_track("video", timed()).unwrap();

		let (_, active) = next_update(&mut announced).await.expect("announce");
		assert!(active);
		let consumer = egress.request(path, None).await.expect("resolve");
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
		origin.consume().request(&update.prefix, None).await.expect("resolve")
	}

	async fn drive_tick() {
		tokio::time::advance(Duration::from_millis(1100)).await;
		for _ in 0..4 {
			tokio::task::yield_now().await;
		}
	}

	#[tokio::test(start_paused = true)]
	async fn every_track_round_trips() {
		// A drain must decode identically off each compressed track, including
		// across an update (the delta path).
		let (producer, origin) = test_producer();
		let tier = Tier::default();
		let mut fed = feed(&producer, tier.clone(), "acme", "foo/bar").await;
		fed.write(42).await;

		drive_tick().await;

		let broadcast = announced(&origin).await;
		let consumer = Consumer::new(broadcast.consume(), Config::new());

		let mut totals = consumer.totals().await.expect("subscribe totals");
		let frame = totals.next().await.expect("read").expect("frame");
		assert_eq!(frame[""].publisher.bytes, 42);

		// A later drain rides a delta.
		fed.write(8).await;
		drive_tick().await;
		let frame = totals.next().await.expect("read").expect("frame");
		assert_eq!(frame[""].publisher.bytes, 50);
		assert_eq!(frame[""].sessions.active(), 1);

		// At depth 0 the one group is the empty one.
		let mut group = consumer
			.group(moq_net::Path::from_static(""))
			.await
			.expect("subscribe group");
		let frame = group.next().await.expect("read").expect("frame");
		let tier = &frame.tiers[""];
		assert_eq!(tier.publisher.bytes, 50);
		assert_eq!(tier.paths["foo/bar"].publisher.bytes, 50);
		assert_eq!(tier.labels["acme"].active(), 1);

		// A presence track is produced once requested, so drive a drain.
		let mut presence = std::pin::pin!(consumer.presence(moq_net::Path::from_static("acme")));
		assert!(futures::poll!(presence.as_mut()).is_pending());
		drive_tick().await;
		let mut presence = presence.await.expect("subscribe presence");
		let frame = presence.next().await.expect("read").expect("frame");
		assert_eq!(frame.get("").expect("default tier").active(), 1);

		// So is a prefix track.
		let mut prefix = std::pin::pin!(consumer.prefix(moq_net::Path::from_static("foo")));
		assert!(futures::poll!(prefix.as_mut()).is_pending());
		drive_tick().await;
		let mut prefix = prefix.await.expect("subscribe prefix");
		let frame = prefix.next().await.expect("read").expect("frame");
		let tier = frame.get("").expect("default tier");
		assert_eq!(tier.publisher.bytes, 50);
		assert_eq!(tier.children["bar"].publisher.bytes, 50);
	}
}
