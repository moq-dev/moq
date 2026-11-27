//! Reading typed catalog updates from a broadcast.

use std::task::{Poll, ready};

use moq_net::Path;
use serde::de::DeserializeOwned;

use super::Catalog;
use crate::Result;

/// Reads typed catalog roots from a catalog track, subscribing to it at catalog priority.
pub struct Consumer<E = ()> {
	// Decoded in two steps, so a catalog needing a newer reader fails with a typed error.
	inner: moq_json::snapshot::Consumer<serde_json::Value>,
	base: Path,
	// Whether the reader holds each update until its timestamp, so it reads every one in order.
	holds: bool,
	_ext: std::marker::PhantomData<E>,
}

impl<E: DeserializeOwned + Default> Consumer<E> {
	/// Read a catalog track, compressed or not, for a reader playing `delay` behind the live edge.
	///
	/// Subscribes to it at the catalog's priority, as [`Catalog::default_subscription`] does. A zero `delay` reads only
	/// the newest catalog, for a reader that applies each on arrival. A reader that holds each update
	/// until its media reaches the update's timestamp passes its playout delay instead, and reads
	/// every update in order that it may still need: an update stays deliverable until it falls
	/// `delay` behind the live edge. See [`moq_json::snapshot::Consumer::new`].
	pub fn new(track: moq_net::track::Consumer, delay: std::time::Duration) -> Self {
		let mut config = moq_json::snapshot::consumer::Config::default();
		config.priority = crate::catalog::PRIORITY.catalog;
		config.delay = delay;
		Self {
			base: track.broadcast().path.clone(),
			inner: moq_json::snapshot::Consumer::new(track, config),
			holds: !delay.is_zero(),
			_ext: std::marker::PhantomData,
		}
	}

	/// Get the next catalog update and its timestamp, or `None` when the track ends.
	///
	/// The update takes effect at `at` on the broadcast's media timeline, so a caller holds one it
	/// reads before its media reaches `at`, and applies one read late, or the first it reads, at
	/// once. On a scale-less catalog track `at` is a group key with no time, so the update applies on
	/// arrival.
	///
	/// Fails with [`Error::EscapingBroadcast`](crate::Error::EscapingBroadcast) if any rendition's
	/// `broadcast` reference, include, or echo prefix walks above the root. Such a reference names no broadcast, and the
	/// root is the consumer's authorized subtree, so it is an attempt to reference content the
	/// consumer cannot name. The whole catalog is rejected rather than the one rendition: a
	/// publisher that emits one has a bug (or is hostile), and silently serving the rest hides
	/// that from the operator while the missing rendition surfaces much later as a track that
	/// never fills.
	#[cfg_attr(feature = "async", kio::wrapper)]
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<moq_json::Timed<Catalog<E>>>>> {
		let next = match self.holds {
			true => ready!(self.inner.poll_next(waiter)),
			false => ready!(self.inner.poll_latest(waiter)),
		};
		let next = next.map_err(|err| match err {
			// A transport failure stays typed so callers can tell a lost source from a bad catalog.
			moq_json::Error::Net(err) => crate::Error::Moq(err),
			err => crate::Error::Json(err.to_string()),
		})?;
		let Some(moq_json::Timed { value, at }) = next else {
			return Poll::Ready(Ok(None));
		};
		let catalog = Catalog::from_value(value)?;
		catalog.check_renditions()?;
		check_resolvable(&self.base, &catalog)?;
		Poll::Ready(Ok(Some(moq_json::Timed::new(catalog, at))))
	}
}

/// Reject the catalog if a track reference, include, or echo prefix walks above the root.
///
/// Every section carrying a relative broadcast path must be included here; a section left out
/// silently exempts its paths from the containment check.
fn check_resolvable<E>(base: &Path, catalog: &Catalog<E>) -> Result<()> {
	let video = catalog.video.renditions.iter();
	let audio = catalog.audio.renditions.iter();
	let text = catalog.text.renditions.iter();
	let json = catalog.json.tracks.iter();
	let binary = catalog.binary.tracks.iter();

	for rel in video
		.map(|(_, config)| config.broadcast.as_ref())
		.chain(audio.map(|(_, config)| config.broadcast.as_ref()))
		.chain(text.map(|(_, config)| config.broadcast.as_ref()))
		.chain(json.map(|(_, config)| config.broadcast.as_ref()))
		.chain(binary.map(|(_, config)| config.broadcast.as_ref()))
		.flatten()
		.chain(catalog.include.values().map(|entry| &entry.broadcast))
	{
		if !rel.is_empty() && base.try_resolve(rel).is_none() {
			return Err(crate::Error::EscapingBroadcast(rel.to_string()));
		}
	}

	if let Some(echo) = &catalog.echo
		&& base.try_resolve(&echo.path).is_none()
	{
		return Err(crate::Error::EscapingBroadcast(echo.path.to_string()));
	}
	Ok(())
}

#[cfg(test)]
mod test {
	use super::*;
	use std::str::FromStr;

	#[test]
	fn echo_prefix_cannot_escape_the_catalog_root() {
		let base = moq_net::Path::from_static("room/live");
		let mut catalog = Catalog::<()>::from_str(include_str!("../../fixtures/catalog-stats-echo.json")).unwrap();
		assert!(check_resolvable(&base, &catalog).is_ok());
		let echo = catalog.echo.as_mut().unwrap();
		echo.path = moq_net::path::Relative::new("../viewers").unwrap();
		assert!(check_resolvable(&base, &catalog).is_ok());
		catalog.echo.as_mut().unwrap().path = moq_net::path::Relative::new("../../viewers").unwrap();
		assert!(matches!(
			check_resolvable(&base, &catalog),
			Err(crate::Error::EscapingBroadcast(_))
		));
	}

	#[test]
	fn includes_cannot_escape_the_root() {
		let base = moq_net::Path::from_static("room/main.hang");
		for (reference, accepted) in [("./alice.hang", true), ("../alice.hang", true), ("../../escape", false)] {
			let json = serde_json::json!({ "include": { "alice": { "broadcast": reference } } }).to_string();
			let catalog = Catalog::<()>::from_str(&json).unwrap();
			match check_resolvable(&base, &catalog) {
				Ok(()) => assert!(accepted, "{reference} must be refused"),
				Err(crate::Error::EscapingBroadcast(_)) => assert!(!accepted, "{reference} must be accepted"),
				Err(err) => panic!("unexpected error: {err}"),
			}
		}
	}

	#[tokio::test]
	async fn subscribe_validates_echo_prefix_before_yielding() {
		for (reference, accepted) in [("viewers", true), ("../../viewers", false)] {
			let mut info = moq_net::broadcast::Info::new();
			info.path = moq_net::Path::from_static("room/live");
			let broadcast = info.produce();
			let track = broadcast
				.create_track(Catalog::DEFAULT_NAME, Catalog::default_track_info())
				.unwrap();
			let mut consumer = Catalog::<()>::subscribe(&broadcast.consume()).await.unwrap();
			let published = serde_json::json!({ "echo": { "path": reference } }).to_string();
			let mut group = track.append_group(moq_net::Timestamp::from_millis(0).unwrap()).unwrap();
			group.write_frame(moq_net::Timestamp::ZERO, published).unwrap();
			group.finish().unwrap();
			let result = consumer.next().await;
			if accepted {
				assert!(result.unwrap().unwrap().value.echo.is_some());
			} else {
				assert!(matches!(result, Err(crate::Error::EscapingBroadcast(_))));
			}
		}
	}

	#[tokio::test]
	async fn subscribe_reads_each_root_with_its_timestamp() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track(Catalog::DEFAULT_NAME, Catalog::default_track_info())
			.unwrap();
		let mut consumer = Catalog::<()>::subscribe(&broadcast.consume()).await.unwrap();
		for (channels, ms) in [(1, 1_500), (2, 3_000)] {
			let mut published = Catalog::default();
			published.audio.renditions.insert(
				"audio".into(),
				crate::catalog::AudioConfig::new(crate::catalog::AudioCodec::Opus, 48_000, channels),
			);
			let at = moq_net::Timestamp::from_millis(ms).unwrap();
			let mut group = track.append_group(at).unwrap();
			group.write_frame(at, published.to_json().unwrap()).unwrap();
			group.finish().unwrap();
			assert_eq!(
				consumer.next().await.unwrap(),
				Some(moq_json::Timed::new(published, at)),
				"the update applies at its frame's timestamp"
			);
		}
	}

	/// Updates that arrive together reach a holding reader in order while it may still play them;
	/// an arrival reader takes only the newest.
	#[tokio::test]
	async fn a_holding_reader_reads_every_update_within_its_delay() {
		// A reader `delay` behind the live edge (3s) plays from `3000 - delay`, so it needs the update
		// in effect there and every later one.
		for (delay, expected) in [
			(0, vec![3_000]),
			(999, vec![2_000, 3_000]),
			(1_001, vec![1_000, 2_000, 3_000]),
		] {
			let broadcast = moq_net::broadcast::Info::new().produce();
			let track = broadcast
				.create_track(Catalog::DEFAULT_NAME, Catalog::default_track_info())
				.unwrap();
			let mut consumer = Consumer::<()>::new(
				broadcast.consume().track(Catalog::DEFAULT_NAME).unwrap(),
				std::time::Duration::from_millis(delay),
			);
			for ms in [1_000, 2_000, 3_000] {
				let mut catalog = Catalog::<()>::default();
				catalog.sync.interval = Some(std::time::Duration::from_millis(ms));
				let mut group = track
					.append_group(moq_net::Timestamp::from_millis(ms).unwrap())
					.unwrap();
				group
					.write_frame(moq_net::Timestamp::from_millis(ms).unwrap(), catalog.to_json().unwrap())
					.unwrap();
				group.finish().unwrap();
			}
			drop(track.finish().unwrap());

			let mut read = Vec::new();
			while let Some(update) = consumer.next().await.unwrap() {
				read.push(update.at.as_millis().unwrap());
			}
			assert_eq!(read, expected, "a {delay}ms delay");
		}
	}

	#[tokio::test]
	async fn a_lost_track_is_a_transport_error() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track(Catalog::DEFAULT_NAME, Catalog::default_track_info())
			.unwrap();
		let mut consumer = Catalog::<()>::subscribe(&broadcast.consume()).await.unwrap();
		track.abort(moq_net::Error::Cancel).unwrap();
		assert!(matches!(
			consumer.next().await,
			Err(crate::Error::Moq(moq_net::Error::Cancel))
		));
	}

	#[tokio::test]
	async fn a_newer_version_is_a_typed_refusal() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track(Catalog::DEFAULT_NAME, Catalog::default_track_info())
			.unwrap();
		let mut consumer = Catalog::<()>::subscribe(&broadcast.consume()).await.unwrap();
		let mut group = track.append_group(moq_net::Timestamp::from_millis(0).unwrap()).unwrap();
		group
			.write_frame(moq_net::Timestamp::ZERO, r#"{"version":5,"video":[]}"#)
			.unwrap();
		group.finish().unwrap();
		assert!(matches!(
			consumer.next().await,
			Err(crate::Error::UnsupportedVersion(5))
		));
	}
}
