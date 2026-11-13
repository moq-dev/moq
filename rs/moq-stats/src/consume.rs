//! The consuming half: typed readers over one published stats broadcast.

use std::task::{Poll, ready};

use moq_net::{Path, broadcast};
use serde::de::DeserializeOwned;

use crate::{PrefixFrame, PresenceFrame, Result, TotalsFrame, prefix_track, presence_track, totals_track};

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
/// [`Self::presence`], and [`Self::prefix`]. A label's presence track and a
/// prefix's track are produced only while subscribed.
pub struct Consumer {
	broadcast: broadcast::Consumer,
}

impl Consumer {
	/// Wrap a stats broadcast. The broadcast is whatever the announce at a
	/// stats path resolved to; parse the path with [`crate::parse_node_path`].
	pub fn new(broadcast: broadcast::Consumer, _config: Config) -> Self {
		Self { broadcast }
	}

	/// Subscribe to the totals track. The first read finishes the subscription
	/// handshake, failing with its error.
	pub fn totals(&self) -> Result<TotalsConsumer> {
		Ok(TotalsConsumer {
			inner: self.subscribe(&totals_track())?,
		})
	}

	/// Subscribe to the presence track for `label`; see [`Self::totals`].
	pub fn presence(&self, label: impl Into<Path>) -> Result<PresenceConsumer> {
		Ok(PresenceConsumer {
			inner: self.subscribe(&presence_track(label))?,
		})
	}

	/// Subscribe to the prefix track for broadcast prefix `path`; see
	/// [`Self::totals`].
	pub fn prefix(&self, path: impl Into<Path>) -> Result<PrefixConsumer> {
		Ok(PrefixConsumer {
			inner: self.subscribe(&prefix_track(path))?,
		})
	}

	/// Subscribe to bucket `bucket`'s track; see [`Self::totals`]. The first read
	/// fails with [`NotFound`](moq_net::Error::NotFound) unless the bucket is
	/// listed.
	#[cfg(test)]
	pub(crate) async fn bucket(&self, bucket: impl Into<Path>) -> Result<BucketConsumer> {
		Ok(BucketConsumer {
			inner: self.subscribe(&crate::listed_bucket_track(&self.broadcast, &bucket.into()).await)?,
		})
	}

	fn subscribe<T: DeserializeOwned>(&self, name: &str) -> Result<Reader<T>> {
		// The reader subscribes itself, so a refused subscription fails its first read.
		let track = self.broadcast.track(name)?;
		Ok(Reader(Box::new(moq_json::snapshot::Consumer::new(
			track,
			Default::default(),
		))))
	}
}

/// A snapshot reader yielding only the latest value.
struct Reader<T>(Box<moq_json::snapshot::Consumer<T>>);

impl<T: DeserializeOwned> Reader<T> {
	fn poll_latest(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<T>>> {
		Poll::Ready(Ok(ready!(self.0.poll_latest(waiter))?.map(|value| value.value)))
	}
}

/// A typed reader over the totals track. Yields the latest [`TotalsFrame`];
/// intermediate frames a slow reader missed are collapsed, which is safe
/// because the counters are cumulative.
pub struct TotalsConsumer {
	inner: Reader<TotalsFrame>,
}

impl TotalsConsumer {
	/// The next frame, or `None` once the track ends (the producer went away).
	#[cfg_attr(feature = "async", kio::wrapper)]
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<TotalsFrame>>> {
		self.inner.poll_latest(waiter)
	}
}

/// A typed reader over one label's presence track; see [`TotalsConsumer`].
pub struct PresenceConsumer {
	inner: Reader<PresenceFrame>,
}

impl PresenceConsumer {
	/// The next frame, or `None` once the track ends (the producer went away).
	#[cfg_attr(feature = "async", kio::wrapper)]
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<PresenceFrame>>> {
		self.inner.poll_latest(waiter)
	}
}

/// A typed reader over one broadcast prefix's track; see [`TotalsConsumer`].
pub struct PrefixConsumer {
	inner: Reader<PrefixFrame>,
}

impl PrefixConsumer {
	/// The next frame, or `None` once the track ends (the producer went away).
	/// Fails with
	/// [`FrameTooLarge`](moq_net::Error::FrameTooLarge) once the frame outgrows
	/// the cache.
	#[cfg_attr(feature = "async", kio::wrapper)]
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<PrefixFrame>>> {
		self.inner.poll_latest(waiter)
	}
}

/// A typed reader over one bucket's track; see [`TotalsConsumer`].
#[cfg(test)]
pub(crate) struct BucketConsumer {
	inner: Reader<crate::BucketFrame>,
}

#[cfg(test)]
impl BucketConsumer {
	/// The next frame, or `None` once the track ends (the producer went away
	/// or the bucket lingered out of the listing). Fails with
	/// [`FrameTooLarge`](moq_net::Error::FrameTooLarge) once the frame outgrows
	/// the cache.
	#[cfg_attr(feature = "async", kio::wrapper)]
	pub(crate) fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<crate::BucketFrame>>> {
		self.inner.poll_latest(waiter)
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
	async fn next_update(announced: &mut moq_net::origin::Routes) -> Option<(moq_net::announce::Announce, bool)> {
		match announced.next().await? {
			moq_net::origin::RouteEvent {
				kind:
					moq_net::origin::RouteKind::Start
					| moq_net::origin::RouteKind::Update
					| moq_net::origin::RouteKind::Restart,
				announce: route,
			} => Some((route, true)),
			moq_net::origin::RouteEvent {
				kind: moq_net::origin::RouteKind::End | moq_net::origin::RouteKind::Offline,
				announce: route,
			} => Some((route, false)),
		}
	}

	use std::time::Duration;

	use moq_net::{Consume, Path, Timestamp, origin, track};

	use moq_net::stats::Tier;

	use crate::{Producer, produce};

	use super::*;

	fn test_producer() -> (Producer, origin::Producer) {
		let origin = produce_origin();
		let producer = crate::produce::spawned(Producer::new(
			produce::Config::new()
				.with_origin(origin.clone())
				.with_node(Path::from_static("sjc")),
		));
		(producer, origin)
	}

	/// A tagged egress feed into a producer's registry, holding the handles needed
	/// to write more traffic incrementally. Presence is recorded under `label`.
	struct Feed {
		track: track::Producer,
		sub: track::Subscriber,
		_announced: origin::Routes,
		_source: origin::Route,
		_ctx: moq_net::stats::Session,
	}

	impl Feed {
		/// Write one frame of `bytes` bytes into the broadcast and read it out on the
		/// egress side, so the publisher `bytes`/`frames`/`groups` counters advance.
		async fn write(&mut self, bytes: usize) {
			let start = self.track.largest().map_or(0, |largest| largest.group + 1);
			let mut group = self.track.append_group(start).unwrap();
			group
				.write_frame(Timestamp::from_millis(start).unwrap(), vec![0u8; bytes])
				.unwrap();
			group.close().unwrap();
			let mut group = self.sub.recv_group().await.unwrap().unwrap();
			while group.read_frame().await.unwrap().is_some() {}
		}
	}

	async fn feed(producer: &Producer, tier: Tier, label: &str, path: &str) -> Feed {
		let ctx = producer
			.registry()
			.tier(tier)
			.session(moq_net::stats::SessionInfo::new(moq_net::Path::new(label).unwrap()));
		let feed_origin = produce_origin();
		let egress = feed_origin.consume().with_stats(ctx.clone());

		let mut announced = egress.routes();
		let source = feed_origin
			.route(path, moq_net::origin::RouteOptions::default())
			.unwrap();
		let track = source.clone().broadcast().create_track("video", timed()).unwrap();

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
		let mut consumer = origin.consume().with_hidden(true).routes();
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

		let mut totals = consumer.totals().expect("subscribe totals");
		let frame = totals.next().await.expect("read").expect("frame");
		assert_eq!(frame[""].publisher.bytes, 42);

		// A later drain rides a delta.
		fed.write(8).await;
		drive_tick().await;
		let frame = totals.next().await.expect("read").expect("frame");
		assert_eq!(frame[""].publisher.bytes, 50);
		assert_eq!(frame[""].sessions.active(), 1);

		// At depth 0 the one bucket is the empty one.
		let mut bucket = consumer
			.bucket(moq_net::Path::from_static(""))
			.await
			.expect("subscribe bucket");
		let frame = bucket.next().await.expect("read").expect("frame");
		let tier = &frame.tiers[""];
		assert_eq!(tier.publisher.bytes, 50);
		assert_eq!(tier.paths["foo/bar"].publisher.bytes, 50);
		assert_eq!(tier.labels["acme"].active(), 1);

		// A presence track is produced once requested, so drive a drain.
		let mut presence = consumer
			.presence(moq_net::Path::from_static("acme"))
			.expect("subscribe presence");
		let mut next = std::pin::pin!(presence.next());
		assert!(futures::poll!(next.as_mut()).is_pending());
		drive_tick().await;
		let frame = next.await.expect("read").expect("frame");
		assert_eq!(frame.get("").expect("default tier").active(), 1);

		// So is a prefix track.
		let mut prefix = consumer
			.prefix(moq_net::Path::from_static("foo"))
			.expect("subscribe prefix");
		let mut next = std::pin::pin!(prefix.next());
		assert!(futures::poll!(next.as_mut()).is_pending());
		drive_tick().await;
		let frame = next.await.expect("read").expect("frame");
		let tier = frame.get("").expect("default tier");
		assert_eq!(tier.publisher.bytes, 50);
		assert_eq!(tier.children["bar"].publisher.bytes, 50);
	}
}
