use std::task::{Poll, ready};

use super::{Catalog, CatalogExt};
use crate::Result;

/// A catalog consumer, used to receive catalog updates and discover tracks.
///
/// This wraps a [`moq_json::Consumer`], reconstructing the JSON catalog from the latest
/// group's snapshot (plus any future deltas) to discover available audio and video tracks.
///
/// Generic over the application extension `E` (defaulting to `()`); yields a
/// [`Catalog<E>`](super::Catalog).
pub struct Consumer<E: CatalogExt = ()> {
	inner: moq_json::Consumer<Catalog<E>>,
}

impl<E: CatalogExt> Consumer<E> {
	/// Create a new catalog consumer from a MoQ track subscriber (uncompressed `catalog.json`).
	pub fn new(track: moq_net::TrackSubscriber) -> Self {
		Self {
			inner: moq_json::Consumer::new(track, moq_json::ConsumerConfig::default()),
		}
	}

	/// Create a consumer for the DEFLATE-compressed catalog track (`catalog.json.z`).
	///
	/// The track must be the compressed one (see [`hang::Catalog::COMPRESSED_NAME`]).
	pub fn compressed(track: moq_net::TrackSubscriber) -> Self {
		let mut config = moq_json::ConsumerConfig::default();
		config.compression = true;
		Self {
			inner: moq_json::Consumer::new(track, config),
		}
	}

	/// Poll for the next catalog update.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Catalog<E>>>> {
		let result = ready!(self.inner.poll_next(waiter));
		Poll::Ready(result.map_err(Into::into))
	}

	/// Get the next catalog update.
	///
	/// This method waits for the next catalog publication and returns the
	/// catalog data. If there are no more updates, `None` is returned.
	pub async fn next(&mut self) -> Result<Option<Catalog<E>>>
	where
		Catalog<E>: Unpin,
	{
		Ok(self.inner.next().await?)
	}
}

impl<E: CatalogExt> From<moq_net::TrackSubscriber> for Consumer<E> {
	fn from(inner: moq_net::TrackSubscriber) -> Self {
		Self::new(inner)
	}
}

#[cfg(test)]
mod test {
	use std::task::Poll;

	use super::*;

	/// Mint a standalone track for tests via a throwaway broadcast, since tracks are
	/// born from their broadcast (no public `TrackProducer::new`).
	fn track_producer(
		name: impl Into<std::sync::Arc<str>>,
		info: impl Into<Option<moq_net::TrackInfo>>,
	) -> moq_net::TrackProducer {
		moq_net::BroadcastInfo::new()
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

	#[test]
	fn waits_for_pending_catalog_group_payload() {
		let mut track = track_producer(hang::Catalog::DEFAULT_NAME, hang::Catalog::default_track_info());
		let mut consumer = Consumer::new(track.subscribe(None));
		let mut group = track.append_group().expect("catalog group should append");

		let waiter = kio::Waiter::noop();
		assert!(matches!(consumer.poll_next(&waiter), Poll::Pending));

		let (catalog, payload) = catalog_payload("pending");
		group.write_frame_now(payload).expect("catalog frame should write");
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
		group.write_frame_now(payload).expect("catalog frame should write");
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
			.write_frame_now(old_payload)
			.expect("old catalog frame should write");
		old_group.finish().expect("old catalog group should finish");

		let mut latest_group = track.append_group().expect("latest catalog group should append");
		latest_group
			.write_frame_now(latest_payload)
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
			.write_frame_now(old_payload)
			.expect("old catalog frame should write");
		old_group.finish().expect("old catalog group should finish");

		let mut latest_group = track.append_group().expect("latest catalog group should append");

		assert!(matches!(consumer.poll_next(&waiter), Poll::Pending));

		latest_group
			.write_frame_now(latest_payload)
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
			.write_frame_now(latest_payload)
			.expect("latest catalog frame should write");
		latest_group.finish().expect("latest catalog group should finish");
		track.finish().expect("catalog track should finish");

		assert_eq!(expect_catalog(consumer.poll_next(&waiter)), latest);

		old_group
			.write_frame_now(old_payload)
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
