use std::task::{Poll, ready};

use moq_net::PathOwned;

use super::{Catalog, CatalogExt};
use crate::Result;

/// A catalog consumer, used to receive catalog updates and discover tracks.
///
/// This wraps a [`moq_json::snapshot::Consumer`], reconstructing the JSON catalog from the latest
/// group's snapshot (plus any future deltas) to discover available audio and video tracks.
///
/// Generic over the application extension `E` (defaulting to `()`); yields a
/// [`Catalog<E>`](super::Catalog).
///
/// A rendition's `broadcast` field is relative to the path of the broadcast serving the
/// catalog, taken from the track's [`broadcast::Info::path`](moq_net::broadcast::Info::path).
/// A catalog that misuses that reference is rejected outright (see [`Self::poll_next`]).
pub struct Consumer<E: CatalogExt = ()> {
	inner: moq_json::snapshot::Consumer<Catalog<E>>,
	/// Path of the broadcast serving this catalog, which every rendition's `broadcast`
	/// reference resolves against.
	base: PathOwned,
}

impl<E: CatalogExt> Consumer<E> {
	/// Create a new catalog consumer from a MoQ track subscriber (uncompressed `catalog.json`).
	pub fn new(track: moq_net::track::Subscriber) -> Self {
		Self {
			base: track.broadcast().path.clone(),
			inner: moq_json::snapshot::Consumer::new(track, moq_json::snapshot::ConsumerConfig::default()),
		}
	}

	/// Create a consumer for the DEFLATE-compressed catalog track (`catalog.json.z`).
	///
	/// The track must be the compressed one (see [`hang::Catalog::COMPRESSED_NAME`]).
	pub fn compressed(track: moq_net::track::Subscriber) -> Self {
		let mut config = moq_json::snapshot::ConsumerConfig::default();
		config.compression = true;
		Self {
			base: track.broadcast().path.clone(),
			inner: moq_json::snapshot::Consumer::new(track, config),
		}
	}

	/// Poll for the next catalog update.
	///
	/// Fails with [`Error::EscapingBroadcast`](crate::Error::EscapingBroadcast) if any rendition's
	/// `broadcast` reference walks above the root. Such a reference names no broadcast, and the
	/// root is the consumer's authorized subtree, so it is an attempt to reference content the
	/// consumer cannot name. The whole catalog is rejected rather than the one rendition: a
	/// publisher that emits one has a bug (or is hostile), and silently serving the rest hides
	/// that from the operator while the missing rendition surfaces much later as a track that
	/// never fills.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Catalog<E>>>> {
		let catalog = ready!(self.inner.poll_next(waiter))?;
		if let Some(catalog) = catalog.as_ref() {
			check_resolvable(&self.base, catalog)?;
		}
		Poll::Ready(Ok(catalog))
	}

	/// Get the next catalog update.
	///
	/// This method waits for the next catalog publication and returns the
	/// catalog data. If there are no more updates, `None` is returned.
	pub async fn next(&mut self) -> Result<Option<Catalog<E>>>
	where
		Catalog<E>: Unpin,
	{
		kio::wait(|waiter| self.poll_next(waiter)).await
	}
}

impl<E: CatalogExt> From<moq_net::track::Subscriber> for Consumer<E> {
	fn from(inner: moq_net::track::Subscriber) -> Self {
		Self::new(inner)
	}
}

/// Reject the catalog if any rendition's `broadcast` reference walks above the root.
///
/// Every section carrying renditions must be chained here; a section left out silently
/// exempts its renditions from the containment check.
fn check_resolvable<E: CatalogExt>(base: &PathOwned, catalog: &Catalog<E>) -> Result<()> {
	let video = catalog.video.renditions.iter();
	let audio = catalog.audio.renditions.iter();
	let text = catalog.text.renditions.iter();

	for (rendition, rel) in video
		.map(|(name, config)| (name, config.broadcast.as_ref()))
		.chain(audio.map(|(name, config)| (name, config.broadcast.as_ref())))
		.chain(text.map(|(name, config)| (name, config.broadcast.as_ref())))
	{
		let Some(rel) = rel.filter(|rel| !rel.is_empty()) else {
			continue;
		};

		if base.resolve(rel).is_none() {
			tracing::error!(rendition, %rel, catalog = %base, "rejecting catalog: broadcast reference escapes the root");
			return Err(crate::Error::EscapingBroadcast(rel.to_string()));
		}
	}

	Ok(())
}

#[cfg(test)]
mod test {
	use std::task::Poll;

	use super::*;

	/// Mint a standalone track for tests via a throwaway broadcast, since tracks are
	/// born from their broadcast (no public `track::Producer::new`).
	fn track_producer(
		name: impl Into<std::sync::Arc<str>>,
		info: impl Into<Option<moq_net::track::Info>>,
	) -> moq_net::track::Producer {
		moq_net::broadcast::Info::new()
			.produce()
			.create_track(name, info)
			.unwrap()
	}

	// Build a base catalog distinguished by an audio rendition named `name`, plus its JSON payload.
	fn catalog_payload(name: &str) -> (Catalog, String) {
		let mut catalog = Catalog::default();
		catalog.audio.renditions.insert(
			name.to_string(),
			hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2),
		);
		let payload = serde_json::to_string(&catalog).expect("catalog should serialize");
		(catalog, payload)
	}

	fn expect_catalog(result: Poll<Result<Option<Catalog>>>) -> Catalog {
		match result {
			Poll::Ready(Ok(Some(decoded))) => decoded,
			other => panic!("expected catalog payload, got {other:?}"),
		}
	}

	fn opus() -> hang::catalog::AudioConfig {
		hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2)
	}

	fn referencing(rel: &str) -> hang::catalog::AudioConfig {
		let mut config = opus();
		config.broadcast = Some(moq_net::PathRelative::new(rel).into_owned());
		config
	}

	/// Publish `catalog` as one snapshot on a broadcast at `a/pub` (two segments, so a
	/// third `..` walks above the root) and return what the stream yields. The broadcast is
	/// minted through an origin because that is what stamps
	/// [`broadcast::Info::path`](moq_net::broadcast::Info::path), the base the consumer
	/// resolves against.
	fn publish_catalog(published: Catalog<()>) -> Result<Option<Catalog>> {
		let origin = moq_net::Origin::random().produce();
		let mut broadcast = origin
			.create_broadcast("a/pub", moq_net::broadcast::Route::new())
			.expect("broadcast should create");
		let mut track = broadcast
			.create_track(hang::Catalog::DEFAULT_NAME, hang::Catalog::default_track_info())
			.expect("catalog track should create");
		let mut consumer = Consumer::new(track.subscribe(None));
		let mut group = track.append_group().expect("catalog group should append");

		let payload = serde_json::to_string(&published).expect("catalog should serialize");
		group
			.write_frame(moq_net::Timestamp::ZERO, payload)
			.expect("catalog frame should write");
		group.finish().expect("catalog group should finish");

		match consumer.poll_next(&kio::Waiter::noop()) {
			Poll::Ready(result) => result,
			Poll::Pending => panic!("a finished catalog group should be ready"),
		}
	}

	/// [`publish_catalog`] over audio renditions alone.
	fn publish(renditions: Vec<(&str, hang::catalog::AudioConfig)>) -> Result<Option<Catalog>> {
		let mut published = Catalog::<()>::default();
		for (name, config) in renditions {
			published.audio.renditions.insert(name.to_string(), config);
		}
		publish_catalog(published)
	}

	/// A reference that stops at or below the root names a broadcast, so the catalog stands.
	#[tokio::test]
	async fn accepts_references_within_the_root() {
		let catalog = publish(vec![
			("here", opus()),
			("sibling", referencing("../source")),
			("root", referencing("../..")),
		])
		.expect("catalog should be accepted")
		.expect("catalog should decode");

		let mut names: Vec<&str> = catalog.audio.renditions.keys().map(String::as_str).collect();
		names.sort_unstable();
		assert_eq!(names, ["here", "root", "sibling"]);
	}

	/// A rendition whose `broadcast` reference walks above the root names no broadcast, and
	/// the root is the consumer's authorized subtree. The whole catalog is rejected rather
	/// than the one rendition, so a publisher bug is loud instead of surfacing later as a
	/// track that never fills.
	#[tokio::test]
	async fn rejects_a_catalog_whose_broadcast_escapes_the_root() {
		for reference in ["../../../elsewhere", "../../..", "../../../.."] {
			// The escaping rendition is published alongside perfectly good ones, which do
			// not rescue the catalog.
			let result = publish(vec![
				("here", opus()),
				("sibling", referencing("../source")),
				("escaping", referencing(reference)),
			]);

			match result {
				Err(crate::Error::EscapingBroadcast(reported)) => assert_eq!(reported, reference),
				Err(err) => panic!("{reference} failed with the wrong error: {err:?}"),
				Ok(_) => panic!("{reference} should reject the catalog"),
			}
		}
	}

	/// The containment check covers every section carrying renditions, not just audio and
	/// video: an escaping text (caption) reference rejects the catalog the same way.
	#[tokio::test]
	async fn rejects_a_catalog_whose_text_broadcast_escapes_the_root() {
		let mut published = Catalog::<()>::default();
		published.audio.renditions.insert("here".to_string(), opus());

		let mut text = hang::catalog::TextConfig::new(hang::catalog::TextFormat::Vtt);
		text.broadcast = Some(moq_net::PathRelative::new("../../../elsewhere").into_owned());
		published.text.renditions.insert("captions".to_string(), text);

		match publish_catalog(published) {
			Err(crate::Error::EscapingBroadcast(reported)) => assert_eq!(reported, "../../../elsewhere"),
			Err(err) => panic!("wrong error: {err:?}"),
			Ok(_) => panic!("an escaping text reference should reject the catalog"),
		}
	}

	#[test]
	fn waits_for_pending_catalog_group_payload() {
		let mut track = track_producer(hang::Catalog::DEFAULT_NAME, hang::Catalog::default_track_info());
		let mut consumer = Consumer::new(track.subscribe(None));
		let mut group = track.append_group().expect("catalog group should append");

		let waiter = kio::Waiter::noop();
		assert!(matches!(consumer.poll_next(&waiter), Poll::Pending));

		let (catalog, payload) = catalog_payload("pending");
		group
			.write_frame(moq_net::Timestamp::ZERO, payload)
			.expect("catalog frame should write");
		group.finish().expect("catalog group should finish");

		assert_eq!(expect_catalog(consumer.poll_next(&waiter)), catalog);
	}

	#[test]
	fn waits_for_pending_catalog_group_payload_after_track_finish() {
		let mut track = track_producer(hang::Catalog::DEFAULT_NAME, hang::Catalog::default_track_info());
		let mut consumer = Consumer::new(track.subscribe(None));
		let mut group = track.append_group().expect("catalog group should append");

		track.finish().expect("catalog track should finish");

		let waiter = kio::Waiter::noop();
		assert!(matches!(consumer.poll_next(&waiter), Poll::Pending));

		let (catalog, payload) = catalog_payload("finished");
		group
			.write_frame(moq_net::Timestamp::ZERO, payload)
			.expect("catalog frame should write");
		group.finish().expect("catalog group should finish");

		assert_eq!(expect_catalog(consumer.poll_next(&waiter)), catalog);
	}

	#[test]
	fn returns_latest_complete_catalog_group() {
		let mut track = track_producer(hang::Catalog::DEFAULT_NAME, hang::Catalog::default_track_info());
		let mut consumer = Consumer::new(track.subscribe(None));
		let waiter = kio::Waiter::noop();

		let (_old, old_payload) = catalog_payload("old");
		let (latest, latest_payload) = catalog_payload("latest");

		let mut old_group = track.append_group().expect("old catalog group should append");
		old_group
			.write_frame(moq_net::Timestamp::ZERO, old_payload)
			.expect("old catalog frame should write");
		old_group.finish().expect("old catalog group should finish");

		let mut latest_group = track.append_group().expect("latest catalog group should append");
		latest_group
			.write_frame(moq_net::Timestamp::ZERO, latest_payload)
			.expect("latest catalog frame should write");
		latest_group.finish().expect("latest catalog group should finish");
		track.finish().expect("catalog track should finish");

		assert_eq!(expect_catalog(consumer.poll_next(&waiter)), latest);
		assert!(matches!(consumer.poll_next(&waiter), Poll::Ready(Ok(None))));
	}

	#[test]
	fn waits_for_newer_pending_group_instead_of_returning_older_ready_group() {
		let mut track = track_producer(hang::Catalog::DEFAULT_NAME, hang::Catalog::default_track_info());
		let mut consumer = Consumer::new(track.subscribe(None));
		let waiter = kio::Waiter::noop();

		let (_old, old_payload) = catalog_payload("old");
		let (latest, latest_payload) = catalog_payload("latest");

		let mut old_group = track.append_group().expect("old catalog group should append");
		old_group
			.write_frame(moq_net::Timestamp::ZERO, old_payload)
			.expect("old catalog frame should write");
		old_group.finish().expect("old catalog group should finish");

		let mut latest_group = track.append_group().expect("latest catalog group should append");

		assert!(matches!(consumer.poll_next(&waiter), Poll::Pending));

		latest_group
			.write_frame(moq_net::Timestamp::ZERO, latest_payload)
			.expect("latest catalog frame should write");
		latest_group.finish().expect("latest catalog group should finish");

		assert_eq!(expect_catalog(consumer.poll_next(&waiter)), latest);
	}

	#[test]
	fn retained_pending_group_is_superseded_by_newer_group() {
		let mut track = track_producer(hang::Catalog::DEFAULT_NAME, hang::Catalog::default_track_info());
		let mut consumer = Consumer::new(track.subscribe(None));
		let waiter = kio::Waiter::noop();

		let (_old, old_payload) = catalog_payload("old");
		let (latest, latest_payload) = catalog_payload("latest");

		let mut old_group = track.append_group().expect("old catalog group should append");

		assert!(matches!(consumer.poll_next(&waiter), Poll::Pending));

		let mut latest_group = track.append_group().expect("latest catalog group should append");
		latest_group
			.write_frame(moq_net::Timestamp::ZERO, latest_payload)
			.expect("latest catalog frame should write");
		latest_group.finish().expect("latest catalog group should finish");
		track.finish().expect("catalog track should finish");

		assert_eq!(expect_catalog(consumer.poll_next(&waiter)), latest);

		old_group
			.write_frame(moq_net::Timestamp::ZERO, old_payload)
			.expect("old catalog frame should write");
		old_group.finish().expect("old catalog group should finish");

		assert!(matches!(consumer.poll_next(&waiter), Poll::Ready(Ok(None))));
	}

	#[test]
	fn returns_none_when_empty_track_finishes() {
		let mut track = track_producer(hang::Catalog::DEFAULT_NAME, hang::Catalog::default_track_info());
		let mut consumer: Consumer = Consumer::new(track.subscribe(None));
		let waiter = kio::Waiter::noop();

		track.finish().expect("catalog track should finish");

		assert!(matches!(consumer.poll_next(&waiter), Poll::Ready(Ok(None))));
	}
}
