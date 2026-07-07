//! Export input: the catalog broadcast plus optional origin context.
//!
//! A hang catalog rendition may reference a track published in *another*
//! broadcast via its `broadcast` field (a path relative to the catalog's
//! broadcast, e.g. `../source`). Resolving that reference requires more than a
//! [`moq_net::broadcast::Consumer`]: it needs the catalog broadcast's own path and
//! an [`moq_net::origin::Consumer`] to fetch the referenced broadcast from.
//! [`Source`] bundles the three so exporters can subscribe to any rendition.

use moq_net::AsPath;

/// The subscription side of an export: the broadcast whose catalog drives it,
/// plus optional origin context for resolving cross-broadcast rendition references.
///
/// Build one from a bare [`moq_net::broadcast::Consumer`] (via `From` or [`Source::new`])
/// when every track lives in the catalog's own broadcast. Add origin context with
/// [`Source::with_origin`] to also serve catalogs whose renditions reference sibling
/// broadcasts; without it, such a rendition fails with [`Error::MissingOrigin`](crate::Error::MissingOrigin).
#[derive(Clone)]
pub struct Source {
	broadcast: moq_net::broadcast::Consumer,
	origin: Option<(moq_net::origin::Consumer, moq_net::PathOwned)>,
}

impl Source {
	/// A source without origin context: every track must live in the catalog's broadcast.
	pub fn new(broadcast: moq_net::broadcast::Consumer) -> Self {
		Self {
			broadcast,
			origin: None,
		}
	}

	/// Attach the origin the catalog broadcast came from and the path it lives at,
	/// enabling renditions that reference another broadcast (e.g. `../source`).
	///
	/// The relative reference is resolved against `path` and fetched via
	/// [`moq_net::origin::Consumer::request_broadcast`], so the referenced broadcast must
	/// be reachable through `origin` (announced, or served by a dynamic handler).
	pub fn with_origin(mut self, origin: moq_net::origin::Consumer, path: impl AsPath) -> Self {
		self.origin = Some((origin, path.as_path().to_owned()));
		self
	}

	/// The broadcast whose catalog drives the export.
	pub fn broadcast(&self) -> &moq_net::broadcast::Consumer {
		&self.broadcast
	}

	/// Start subscribing to `name`, honoring an optional cross-broadcast reference.
	///
	/// A missing/empty `rel`, or one that resolves back to the catalog's own path (or
	/// to the origin root), subscribes on the catalog broadcast directly. Anything else
	/// requests the resolved broadcast from the origin first.
	pub(crate) fn subscribe(&self, rel: Option<&moq_net::PathRelative<'_>>, name: &str) -> crate::Result<Subscribe> {
		if let Some(rel) = rel.filter(|rel| !rel.is_empty()) {
			let Some((origin, base)) = &self.origin else {
				return Err(crate::Error::MissingOrigin(rel.to_owned()));
			};

			let resolved = base.resolve(rel);

			// A reference that walks back to the catalog's own broadcast is served by
			// the catalog broadcast itself, avoiding a redundant subscription. Excess
			// `..` resolving to the (empty) origin root is not a broadcast; treat it
			// the same way rather than requesting an unrouteable path.
			if !resolved.is_empty() && resolved != *base {
				return Ok(Subscribe::Broadcast(
					origin.request_broadcast(&resolved),
					name.to_string(),
				));
			}
		}

		Ok(Subscribe::Track(self.broadcast.track(name)?.subscribe(None)))
	}

	/// Resolve an optional cross-broadcast reference and subscribe to track `name`,
	/// awaiting SUBSCRIBE_OK.
	///
	/// `rel` is a rendition's catalog `broadcast` field: `None` (or an empty / self
	/// reference) subscribes on the catalog broadcast; anything else fetches the
	/// referenced broadcast from the attached origin first. Without origin context a
	/// non-trivial reference fails with [`Error::MissingOrigin`](crate::Error::MissingOrigin).
	///
	/// This is the async counterpart to the poll-driven container exporters: consumers
	/// that wrap a raw [`moq_net::track::Subscriber`] themselves (e.g. the WebRTC egress)
	/// use it to honor cross-broadcast renditions without reimplementing the path math.
	pub async fn subscribe_track(
		&self,
		rel: Option<&moq_net::PathRelative<'_>>,
		name: &str,
	) -> crate::Result<moq_net::track::Subscriber> {
		match self.subscribe(rel, name)? {
			Subscribe::Track(pending) => Ok(pending.await?),
			Subscribe::Broadcast(pending, name) => {
				let broadcast = pending.await?;
				Ok(broadcast.track(&name)?.subscribe(None).await?)
			}
		}
	}
}

impl From<moq_net::broadcast::Consumer> for Source {
	fn from(broadcast: moq_net::broadcast::Consumer) -> Self {
		Self::new(broadcast)
	}
}

/// A pending rendition subscription, either direct or via a referenced broadcast.
pub(crate) enum Subscribe {
	/// Subscribing on the catalog broadcast.
	Track(kio::Pending<moq_net::track::Subscribe>),
	/// Waiting for the referenced broadcast; the track (by name) is subscribed once it resolves.
	Broadcast(kio::Pending<moq_net::origin::Requested>, String),
}

#[cfg(test)]
mod tests {
	use super::*;
	use moq_net::{Origin, PathRelative};

	fn broadcast() -> moq_net::broadcast::Producer {
		moq_net::broadcast::Info::new().produce()
	}

	#[test]
	fn no_override_subscribes_catalog_broadcast() {
		let producer = broadcast();
		// Keep a dynamic handle alive so track requests pend instead of NotFound.
		let _dynamic = producer.dynamic();
		let source = Source::new(producer.consume());

		assert!(matches!(source.subscribe(None, "video").unwrap(), Subscribe::Track(_)));
		// An empty rel is the same as no rel.
		let empty = PathRelative::empty();
		assert!(matches!(
			source.subscribe(Some(&empty), "video").unwrap(),
			Subscribe::Track(_)
		));
	}

	#[test]
	fn override_without_origin_fails() {
		let producer = broadcast();
		let source = Source::new(producer.consume());

		let rel = PathRelative::new("../other");
		assert!(matches!(
			source.subscribe(Some(&rel), "video"),
			Err(crate::Error::MissingOrigin(_))
		));
	}

	#[test]
	fn self_reference_subscribes_catalog_broadcast() {
		let origin = Origin::random().produce();
		let producer = broadcast();
		let _dynamic = producer.dynamic();
		let _publish = origin.publish_broadcast("a/pub", &producer).unwrap();

		let source = Source::new(producer.consume()).with_origin(origin.consume(), "a/pub");

		// Walks back to the catalog's own path.
		let rel = PathRelative::new("../pub");
		assert!(matches!(
			source.subscribe(Some(&rel), "video").unwrap(),
			Subscribe::Track(_)
		));

		// Excess `..` resolves to the (empty) origin root, which is not a broadcast.
		let rel = PathRelative::new("../../..");
		assert!(matches!(
			source.subscribe(Some(&rel), "video").unwrap(),
			Subscribe::Track(_)
		));
	}

	#[tokio::test]
	async fn override_resolves_referenced_broadcast() {
		let origin = Origin::random().produce();

		let catalog = broadcast();
		let _catalog_publish = origin.publish_broadcast("a/pub", &catalog).unwrap();

		let referenced = broadcast();
		let _referenced_publish = origin.publish_broadcast("a/source", &referenced).unwrap();

		let source = Source::new(catalog.consume()).with_origin(origin.consume(), "a/pub");

		let rel = PathRelative::new("../source");
		let Subscribe::Broadcast(pending, name) = source.subscribe(Some(&rel), "video").unwrap() else {
			panic!("expected a cross-broadcast subscription");
		};
		assert_eq!(name, "video");
		pending.await.expect("referenced broadcast should resolve");
	}

	#[tokio::test]
	async fn subscribe_track_resolves_catalog_broadcast() {
		let mut producer = broadcast();
		// The track must exist for the subscription to resolve (SUBSCRIBE_OK).
		let _video = producer.create_track("video", None).unwrap();
		let source = Source::new(producer.consume());

		source
			.subscribe_track(None, "video")
			.await
			.expect("catalog track should resolve");
	}

	#[tokio::test]
	async fn subscribe_track_without_origin_fails() {
		let producer = broadcast();
		let source = Source::new(producer.consume());

		let rel = PathRelative::new("../other");
		assert!(matches!(
			source.subscribe_track(Some(&rel), "video").await,
			Err(crate::Error::MissingOrigin(_))
		));
	}

	#[tokio::test]
	async fn subscribe_track_resolves_referenced_broadcast() {
		let origin = Origin::random().produce();

		let catalog = broadcast();
		let _catalog_publish = origin.publish_broadcast("a/pub", &catalog).unwrap();

		let mut referenced = broadcast();
		let _video = referenced.create_track("video", None).unwrap();
		let _referenced_publish = origin.publish_broadcast("a/source", &referenced).unwrap();

		let source = Source::new(catalog.consume()).with_origin(origin.consume(), "a/pub");

		// The reference resolves to `a/source`, whose "video" track answers the subscribe.
		let rel = PathRelative::new("../source");
		source
			.subscribe_track(Some(&rel), "video")
			.await
			.expect("referenced track should resolve");
	}
}
