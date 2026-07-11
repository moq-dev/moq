//! Export input: an origin plus the path of the broadcast whose catalog drives the export.
//!
//! A hang catalog rendition may reference a track published in *another*
//! broadcast via its `broadcast` field (a path relative to the catalog's
//! broadcast, e.g. `../source`). Resolving that reference needs the catalog
//! broadcast's own path and an [`moq_net::origin::Consumer`] to fetch the
//! referenced broadcast from. [`Source`] bundles the two, and resolves both the
//! catalog broadcast and any referenced broadcast through the same origin so
//! [`request_broadcast`](moq_net::origin::Consumer::request_broadcast) deduplicates
//! shared subscriptions.

use moq_net::AsPath;

/// The subscription side of an export: an origin and the path of the broadcast
/// whose catalog drives it.
///
/// The catalog broadcast and every rendition (including ones whose catalog
/// `broadcast` field references a sibling broadcast) resolve against `origin`,
/// so a source can always follow a cross-broadcast reference. Build one with
/// [`Source::new`].
#[derive(Clone)]
pub struct Source {
	origin: moq_net::origin::Consumer,
	path: moq_net::PathOwned,
}

impl Source {
	/// A source rooted at `origin`, driven by the catalog of the broadcast at `path`.
	///
	/// `path` names the broadcast whose catalog is exported; a rendition's relative
	/// `broadcast` reference is resolved against it. Both the catalog broadcast and any
	/// referenced broadcast are fetched via
	/// [`origin.request_broadcast`](moq_net::origin::Consumer::request_broadcast), so they
	/// must be reachable through `origin` (announced, or served by a dynamic handler).
	pub fn new(origin: moq_net::origin::Consumer, path: impl AsPath) -> Self {
		Self {
			origin,
			path: path.as_path().to_owned(),
		}
	}

	/// Resolve and subscribe to the catalog broadcast (the one at this source's path).
	pub async fn broadcast(&self) -> crate::Result<moq_net::broadcast::Consumer> {
		Ok(self.origin.request_broadcast(&self.path).await?)
	}

	/// Begin resolving the broadcast that serves rendition track `name`, honoring an
	/// optional cross-broadcast reference.
	///
	/// A missing/empty `rel`, or one that resolves back to the catalog's own path (or
	/// walks past the origin root), targets the catalog broadcast; anything else targets
	/// the resolved sibling broadcast. Either way the broadcast is fetched from the origin,
	/// which deduplicates repeat requests for the same live path (announced or dynamically
	/// served) so the catalog and every rendition share one upstream subscription.
	pub(crate) fn request(&self, rel: Option<&moq_net::PathRelative<'_>>) -> kio::Pending<moq_net::origin::Requested> {
		let target = match rel.filter(|rel| !rel.is_empty()) {
			// Excess `..` clamps to the (empty) origin root, which is not a broadcast; treat
			// it as a self-reference and use the catalog broadcast instead.
			Some(rel) => match self.path.resolve(rel) {
				resolved if resolved.is_empty() => self.path.clone(),
				resolved => resolved,
			},
			None => self.path.clone(),
		};

		self.origin.request_broadcast(&target)
	}

	/// Resolve an optional cross-broadcast reference to its broadcast.
	///
	/// `rel` is a rendition's catalog `broadcast` field: `None` (or an empty / self
	/// reference) resolves the catalog broadcast itself; anything else fetches the
	/// referenced sibling broadcast from the origin. Use it when you need the broadcast
	/// handle itself (e.g. to FETCH individual groups) rather than a subscription.
	pub async fn resolve(
		&self,
		rel: Option<&moq_net::PathRelative<'_>>,
	) -> crate::Result<moq_net::broadcast::Consumer> {
		Ok(self.request(rel).await?)
	}

	/// Resolve an optional cross-broadcast reference and subscribe to track `name`,
	/// awaiting SUBSCRIBE_OK.
	///
	/// `rel` is a rendition's catalog `broadcast` field: `None` (or an empty / self
	/// reference) subscribes on the catalog broadcast; anything else fetches the
	/// referenced broadcast from the origin first.
	///
	/// This is the async counterpart to the poll-driven container exporters: consumers
	/// that wrap a raw [`moq_net::track::Subscriber`] themselves (e.g. the WebRTC egress)
	/// use it to honor cross-broadcast renditions without reimplementing the path math.
	pub async fn subscribe_track(
		&self,
		rel: Option<&moq_net::PathRelative<'_>>,
		name: &str,
	) -> crate::Result<moq_net::track::Subscriber> {
		let broadcast = self.request(rel).await?;
		Ok(broadcast.track(name)?.subscribe(None).await?)
	}
}

/// Test helper: announce `broadcast` on a throwaway origin and return a [`Source`] rooted at
/// it, so exporter tests that build a local broadcast can still resolve it by path. The origin
/// and its announcement are leaked so the broadcast stays reachable for the source's lifetime
/// (harmless in a test binary).
#[cfg(test)]
pub(crate) fn announced(broadcast: &moq_net::broadcast::Consumer) -> Source {
	let origin = moq_net::Origin::random().produce();
	let publish = origin
		.publish_broadcast("test", broadcast)
		.expect("publish test broadcast");
	let source = Source::new(origin.consume(), "test");
	Box::leak(Box::new((origin, publish)));
	source
}

#[cfg(test)]
mod tests {
	use super::*;
	use moq_net::{Origin, PathRelative};

	#[tokio::test]
	async fn no_override_targets_catalog_broadcast() {
		let origin = Origin::random().produce();
		let producer = moq_net::broadcast::Info::new().produce();
		let _publish = origin.publish_broadcast("a/pub", &producer).unwrap();

		let source = Source::new(origin.consume(), "a/pub");

		// No reference and an empty reference both resolve to the catalog broadcast.
		source.request(None).await.expect("catalog broadcast should resolve");
		let empty = PathRelative::empty();
		source
			.request(Some(&empty))
			.await
			.expect("empty reference should resolve to the catalog broadcast");
	}

	#[tokio::test]
	async fn subscribe_track_resolves_catalog_broadcast() {
		let origin = Origin::random().produce();
		let mut producer = moq_net::broadcast::Info::new().produce();
		// The track must exist for the subscription to resolve (SUBSCRIBE_OK).
		let _video = producer.create_track("video", None).unwrap();
		let _publish = origin.publish_broadcast("a/pub", &producer).unwrap();

		let source = Source::new(origin.consume(), "a/pub");
		source
			.subscribe_track(None, "video")
			.await
			.expect("catalog track should resolve");
	}

	#[tokio::test]
	async fn self_reference_targets_catalog_broadcast() {
		let origin = Origin::random().produce();
		let mut producer = moq_net::broadcast::Info::new().produce();
		let _video = producer.create_track("video", None).unwrap();
		let _publish = origin.publish_broadcast("a/pub", &producer).unwrap();

		let source = Source::new(origin.consume(), "a/pub");

		// Walks back to the catalog's own path.
		let rel = PathRelative::new("../pub");
		source
			.subscribe_track(Some(&rel), "video")
			.await
			.expect("self-reference should resolve to the catalog broadcast");

		// Excess `..` walks past the (empty) origin root, treated as a self-reference.
		let rel = PathRelative::new("../../..");
		source
			.subscribe_track(Some(&rel), "video")
			.await
			.expect("excess `..` should resolve to the catalog broadcast");
	}

	#[tokio::test]
	async fn subscribe_track_resolves_referenced_broadcast() {
		let origin = Origin::random().produce();

		let catalog = moq_net::broadcast::Info::new().produce();
		let _catalog_publish = origin.publish_broadcast("a/pub", &catalog).unwrap();

		let mut referenced = moq_net::broadcast::Info::new().produce();
		let _video = referenced.create_track("video", None).unwrap();
		let _referenced_publish = origin.publish_broadcast("a/source", &referenced).unwrap();

		let source = Source::new(origin.consume(), "a/pub");

		// The reference resolves to `a/source`, whose "video" track answers the subscribe.
		let rel = PathRelative::new("../source");
		source
			.subscribe_track(Some(&rel), "video")
			.await
			.expect("referenced track should resolve");
	}
}
