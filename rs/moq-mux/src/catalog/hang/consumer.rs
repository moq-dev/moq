use std::task::Poll;

use super::{Catalog, CatalogExt};
use crate::Result;

/// A catalog consumer, used to receive catalog updates and discover tracks.
///
/// This wraps [`hang::catalog::Consumer`], which reconstructs the JSON catalog from the latest
/// group's snapshot and checks its renditions, reporting failures as this crate's
/// [`Error`](crate::Error).
///
/// Generic over the application extension `E` (defaulting to `()`); yields a
/// [`Catalog<E>`](super::Catalog).
///
/// A rendition's `broadcast` field is relative to the path of the broadcast serving the
/// catalog, taken from the track's [`broadcast::Info::path`](moq_net::broadcast::Info::path).
/// A catalog that misuses that reference is rejected outright (see [`Self::poll_next`]).
pub struct Consumer<E: CatalogExt = ()> {
	inner: hang::catalog::Consumer<E>,
}

impl<E: CatalogExt> Consumer<E> {
	/// Create a new catalog consumer from a `catalog.json` track subscriber, compressed or not.
	pub fn new(track: moq_net::track::Subscriber) -> Self {
		Self {
			inner: hang::catalog::Consumer::new(track),
		}
	}

	/// Poll for the next catalog update.
	///
	/// Fails with [`Error::EscapingBroadcast`](crate::Error::EscapingBroadcast) if any rendition's
	/// `broadcast` reference or echo prefix walks above the root, rejecting the whole catalog
	/// (see [`hang::catalog::Consumer::poll_next`]). A transport failure is
	/// [`Error::Moq`](crate::Error::Moq).
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Catalog<E>>>> {
		self.inner.poll_next(waiter).map_err(|err| match err {
			// Lifted to this crate's variants, which `Source` reports for the same faults.
			hang::Error::Moq(err) => crate::Error::Moq(err),
			hang::Error::EscapingBroadcast(rel) => crate::Error::EscapingBroadcast(rel),
			err => crate::Error::Hang(err),
		})
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
		config.broadcast = Some(moq_net::path::Relative::new(rel).unwrap());
		config
	}

	/// Publish `catalog` as one snapshot on a broadcast at `a/pub` (two segments, so a
	/// third `..` walks above the root) and return what the stream yields. The broadcast is
	/// minted through an origin because that is what stamps
	/// [`broadcast::Info::path`](moq_net::broadcast::Info::path), the base the consumer
	/// resolves against.
	fn publish_catalog(published: Catalog<()>) -> Result<Option<Catalog>> {
		let origin = crate::source::produce_origin();
		let broadcast = origin.create_broadcast("a/pub").expect("broadcast should create");
		let track = broadcast
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

	/// [`publish_catalog`] against the answer a dynamic handler prepares and publishes at
	/// `a/pub`, read back through the requesting origin cursor. That is the shape an app
	/// takes when it answers an origin request.
	async fn publish_catalog_served(published: Catalog<()>) -> Result<Option<Catalog>> {
		let origin = crate::source::produce_origin();
		let dynamic = origin.requests("").unwrap();
		let requesting = origin.consume().request("a/pub", None);
		// The handler hands its route back: dropping it would retract the route and end
		// the broadcast it just served.
		let handler = tokio::spawn(async move {
			let prepared = dynamic
				.requested_broadcast()
				.await
				.expect("handler should be asked for the path")
				.prepare(moq_net::Epoch::mint())
				.expect("the answer should prepare");
			let track = prepared
				.broadcast()
				.create_track(hang::Catalog::DEFAULT_NAME, hang::Catalog::default_track_info())
				.expect("catalog track should create");
			let broadcast = prepared.publish().expect("the answer should publish");
			(dynamic, broadcast, track)
		});

		let resolved = requesting.await;
		let (_dynamic, _broadcast, track) = handler.await.expect("handler task");
		let subscriber = resolved
			.expect("the handler served it")
			.track(hang::Catalog::DEFAULT_NAME)
			.expect("catalog track should resolve")
			.subscribe(None)
			.await
			.expect("catalog track should subscribe");
		let mut consumer = Consumer::new(subscriber);

		let mut group = track.append_group().expect("catalog group should append");
		let payload = serde_json::to_string(&published).expect("catalog should serialize");
		group
			.write_frame(moq_net::Timestamp::ZERO, payload)
			.expect("catalog frame should write");
		group.finish().expect("catalog group should finish");

		consumer.next().await
	}

	/// [`publish_catalog_served`] over one audio rendition carrying `reference`.
	async fn publish_served(reference: &str) -> Result<Option<Catalog>> {
		let mut published = Catalog::<()>::default();
		published
			.audio
			.renditions
			.insert("sibling".to_string(), referencing(reference));
		publish_catalog_served(published).await
	}

	fn soliciting(reference: &str) -> Catalog {
		let (mut catalog, _) = catalog_payload("here");
		catalog.echo = Some(hang::catalog::Echo::new(
			moq_net::path::Relative::new(reference).unwrap(),
		));
		catalog
	}

	#[tokio::test]
	async fn echo_prefix_is_checked_before_the_native_reader_yields() {
		for reference in ["viewers", "../viewers", ""] {
			assert!(publish_catalog(soliciting(reference)).unwrap().unwrap().echo.is_some());
		}
		for reference in ["../../viewers", "../../../elsewhere"] {
			let result = publish_catalog(soliciting(reference));
			assert!(
				matches!(result, Err(crate::Error::EscapingBroadcast(_))),
				"{reference}: {result:?}"
			);
		}
	}

	#[tokio::test]
	async fn echo_prefix_is_checked_against_the_dynamic_served_path() {
		assert!(
			publish_catalog_served(soliciting("../viewers"))
				.await
				.unwrap()
				.unwrap()
				.echo
				.is_some()
		);
		let result = publish_catalog_served(soliciting("../../../elsewhere")).await;
		assert!(matches!(result, Err(crate::Error::EscapingBroadcast(_))), "{result:?}");
	}

	/// A reference that stops at or below the root names a broadcast, so the catalog stands.
	/// `a/pub` resolves against `a`, so `..` is what lands on the root itself.
	#[tokio::test]
	async fn accepts_references_within_the_root() {
		let catalog = publish(vec![
			("here", opus()),
			("sibling", referencing("./source")),
			("root", referencing("..")),
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

	/// A broadcast is named by where its reader found it, not by where its producer created
	/// it: a standalone broadcast served on demand at `a/pub` resolves `../source` against
	/// `a/pub`. Naming it by the producer's own (empty) path would make its own root the
	/// bound, rejecting a reference the requester can perfectly well reach.
	#[tokio::test]
	async fn accepts_references_within_the_served_path() {
		let catalog = publish_served("../source")
			.await
			.expect("catalog should be accepted")
			.expect("catalog should decode");

		let rendition = catalog
			.audio
			.renditions
			.get("sibling")
			.expect("rendition should survive");
		assert_eq!(rendition.broadcast.as_ref().unwrap().as_str(), "../source");
	}

	/// The bound moves with the served path rather than disappearing: a reference that
	/// walks above `a/pub` still names nothing and still rejects the catalog.
	#[tokio::test]
	async fn rejects_references_escaping_the_served_path() {
		match publish_served("../../../elsewhere").await {
			Err(crate::Error::EscapingBroadcast(reported)) => assert_eq!(reported, "../../../elsewhere"),
			Err(err) => panic!("wrong error: {err:?}"),
			Ok(_) => panic!("an escaping reference should reject the catalog"),
		}
	}

	/// The containment check covers every section carrying renditions, not just audio and
	/// video: an escaping text (caption) reference rejects the catalog the same way.
	#[tokio::test]
	async fn rejects_a_catalog_whose_text_broadcast_escapes_the_root() {
		let mut published = Catalog::<()>::default();
		published.audio.renditions.insert("here".to_string(), opus());

		let mut text = hang::catalog::TextConfig::new(hang::catalog::TextFormat::Vtt);
		text.broadcast = Some(moq_net::path::Relative::new("../../../elsewhere").unwrap());
		published.text.renditions.insert("captions".to_string(), text);

		match publish_catalog(published) {
			Err(crate::Error::EscapingBroadcast(reported)) => assert_eq!(reported, "../../../elsewhere"),
			Err(err) => panic!("wrong error: {err:?}"),
			Ok(_) => panic!("an escaping text reference should reject the catalog"),
		}
	}

	/// Data tracks carry a `broadcast` reference like any rendition, so the containment check
	/// covers them too. Without this the catalog is accepted and the escape surfaces much later,
	/// when the entry is subscribed.
	#[tokio::test]
	async fn rejects_a_catalog_whose_data_broadcast_escapes_the_root() {
		for section in ["json", "binary"] {
			let mut published = Catalog::<()>::default();
			published.audio.renditions.insert("here".to_string(), opus());

			let escaping = moq_net::path::Relative::new("../../../elsewhere");
			match section {
				"json" => {
					let mut config = hang::catalog::JsonConfig::new(hang::catalog::Mode::Stream);
					config.broadcast = Some(escaping.unwrap());
					published.json.tracks.insert("chat".to_string(), config);
				}
				_ => {
					let mut config = hang::catalog::BinaryConfig::new(hang::catalog::Mode::Stream);
					config.broadcast = Some(escaping.unwrap());
					published.binary.tracks.insert("blobs".to_string(), config);
				}
			}

			match publish_catalog(published) {
				Err(crate::Error::EscapingBroadcast(reported)) => assert_eq!(reported, "../../../elsewhere"),
				Err(err) => panic!("wrong error for {section}: {err:?}"),
				Ok(_) => panic!("an escaping {section} reference should reject the catalog"),
			}
		}
	}

	#[test]
	fn waits_for_pending_catalog_group_payload() {
		let track = track_producer(hang::Catalog::DEFAULT_NAME, hang::Catalog::default_track_info());
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
	fn a_newer_version_is_a_typed_refusal() {
		let track = track_producer(hang::Catalog::DEFAULT_NAME, hang::Catalog::default_track_info());
		let mut consumer: Consumer = Consumer::new(track.subscribe(None));
		let mut group = track.append_group().expect("catalog group should append");
		group
			.write_frame(moq_net::Timestamp::ZERO, r#"{"version":5}"#)
			.expect("catalog frame should write");
		group.finish().expect("catalog group should finish");

		assert!(matches!(
			consumer.poll_next(&kio::Waiter::noop()),
			Poll::Ready(Err(crate::Error::Hang(hang::Error::UnsupportedVersion(5))))
		));
	}

	#[test]
	fn waits_for_pending_catalog_group_payload_after_track_finish() {
		let track = track_producer(hang::Catalog::DEFAULT_NAME, hang::Catalog::default_track_info());
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
		let track = track_producer(hang::Catalog::DEFAULT_NAME, hang::Catalog::default_track_info());
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
		let track = track_producer(hang::Catalog::DEFAULT_NAME, hang::Catalog::default_track_info());
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
		let track = track_producer(hang::Catalog::DEFAULT_NAME, hang::Catalog::default_track_info());
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

	/// A lost catalog track is a transport error, so a caller can hold for the source to return
	/// instead of failing as if the catalog were malformed.
	#[test]
	fn a_lost_track_is_a_transport_error() {
		let track = track_producer(hang::Catalog::DEFAULT_NAME, hang::Catalog::default_track_info());
		let mut consumer: Consumer = Consumer::new(track.subscribe(None));
		track.abort(moq_net::Error::Cancel).expect("catalog track should abort");

		assert!(matches!(
			consumer.poll_next(&kio::Waiter::noop()),
			Poll::Ready(Err(crate::Error::Moq(moq_net::Error::Cancel)))
		));
	}

	#[test]
	fn returns_none_when_empty_track_finishes() {
		let track = track_producer(hang::Catalog::DEFAULT_NAME, hang::Catalog::default_track_info());
		let mut consumer: Consumer = Consumer::new(track.subscribe(None));
		let waiter = kio::Waiter::noop();

		track.finish().expect("catalog track should finish");

		assert!(matches!(consumer.poll_next(&waiter), Poll::Ready(Ok(None))));
	}
}
