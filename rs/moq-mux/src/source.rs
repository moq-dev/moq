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
		Ok(self.request_catalog().await?)
	}

	/// Subscribe to this broadcast's catalog.
	///
	/// The stream rejects a catalog carrying a `broadcast` reference that walks above the root
	/// (see [`Error::EscapingBroadcast`](crate::Error::EscapingBroadcast)), so every snapshot a
	/// consumer sees is one whose references all name a broadcast it may reach.
	pub async fn catalog<E: crate::catalog::hang::CatalogExt>(
		&self,
		format: crate::catalog::CatalogFormat,
	) -> crate::Result<crate::catalog::Consumer<E>> {
		let broadcast = self.broadcast().await?;
		crate::catalog::Consumer::new(&broadcast, format).await
	}

	/// Begin resolving the catalog broadcast (the one at this source's path).
	pub(crate) fn request_catalog(&self) -> kio::Pending<moq_net::origin::Requesting> {
		self.origin.request_broadcast(&self.path)
	}

	/// The path of the broadcast a rendition's `broadcast` reference names.
	///
	/// A missing or empty `rel` names the catalog broadcast; anything else names the
	/// resolved broadcast, which may be the catalog's own path again (`../pub` from
	/// `a/pub`).
	fn target(&self, rel: Option<&moq_net::PathRelative<'_>>) -> crate::Result<moq_net::PathOwned> {
		let Some(rel) = rel.filter(|rel| !rel.is_empty()) else {
			return Ok(self.path.clone());
		};

		self.path.resolve(rel).ok_or_else(|| {
			tracing::error!(%rel, catalog = %self.path, "broadcast reference escapes the root");
			crate::Error::EscapingBroadcast(rel.to_string())
		})
	}

	/// Begin resolving the broadcast that serves a rendition, honoring an optional
	/// cross-broadcast reference.
	///
	/// The broadcast is fetched from the origin, which deduplicates repeat requests for the
	/// same live path (announced or dynamically served) so the catalog and every rendition
	/// share one upstream subscription.
	///
	/// Fails with [`Error::EscapingBroadcast`](crate::Error::EscapingBroadcast) if `rel` walks
	/// above the origin root, naming no broadcast.
	pub(crate) fn request(
		&self,
		rel: Option<&moq_net::PathRelative<'_>>,
	) -> crate::Result<kio::Pending<moq_net::origin::Requesting>> {
		Ok(self.origin.request_broadcast(&self.target(rel)?))
	}

	/// Resolve an optional cross-broadcast reference to its broadcast.
	///
	/// `rel` is a rendition's catalog `broadcast` field: `None` (or an empty reference)
	/// resolves the catalog broadcast itself; anything else fetches the referenced
	/// broadcast from the origin. Use it when you need the broadcast handle itself
	/// (e.g. to FETCH individual groups) rather than a subscription.
	///
	/// A reference that escapes above the origin root is
	/// [`Error::EscapingBroadcast`](crate::Error::EscapingBroadcast): the rendition names
	/// no broadcast, so there is nothing to resolve.
	pub async fn resolve(
		&self,
		rel: Option<&moq_net::PathRelative<'_>>,
	) -> crate::Result<moq_net::broadcast::Consumer> {
		Ok(self.request(rel)?.await?)
	}

	/// Resolve an optional cross-broadcast reference and subscribe to track `name`,
	/// awaiting SUBSCRIBE_OK.
	///
	/// `rel` is a rendition's catalog `broadcast` field: `None` (or an empty reference)
	/// subscribes on the catalog broadcast; anything else fetches the referenced broadcast
	/// from the origin first. A reference that escapes above the origin root is
	/// [`Error::EscapingBroadcast`](crate::Error::EscapingBroadcast).
	///
	/// This is the async counterpart to the poll-driven container exporters: consumers
	/// that wrap a raw [`moq_net::track::Subscriber`] themselves (e.g. the WebRTC egress)
	/// use it to honor cross-broadcast renditions without reimplementing the path math.
	pub async fn subscribe_track(
		&self,
		rel: Option<&moq_net::PathRelative<'_>>,
		name: &str,
	) -> crate::Result<moq_net::track::Subscriber> {
		let broadcast = self.request(rel)?.await?;
		Ok(broadcast.track(name)?.subscribe(None).await?)
	}
}

/// Test helper: serve `broadcast` on a throwaway origin's dynamic handler and return a
/// [`Source`] rooted at it, so exporter tests that build a local broadcast can still resolve
/// it by path. The origin is leaked so the broadcast stays reachable for the source's
/// lifetime (harmless in a test binary).
#[cfg(test)]
pub(crate) fn announced(broadcast: &moq_net::broadcast::Consumer) -> Source {
	let origin = moq_net::Origin::random().produce();
	let mut dynamic = origin.dynamic();
	let served = broadcast.clone();
	tokio::spawn(async move {
		while let Ok(request) = dynamic.requested_broadcast().await {
			request.accept(served.clone());
		}
	});
	let source = Source::new(origin.consume(), "test");
	Box::leak(Box::new(origin));
	source
}

#[cfg(test)]
mod tests {
	use super::*;
	use moq_net::{Origin, PathRelative};

	/// Let the origin's spawned attach task run: a created broadcast becomes
	/// routable asynchronously, shortly after `create_broadcast` returns.
	async fn settle() {
		for _ in 0..10 {
			tokio::task::yield_now().await;
		}
	}

	#[tokio::test]
	async fn no_override_targets_catalog_broadcast() {
		let origin = Origin::random().produce();
		let _producer = origin
			.create_broadcast("a/pub", moq_net::broadcast::Route::new().with_announce(true))
			.unwrap();
		settle().await;

		let source = Source::new(origin.consume(), "a/pub");

		// No reference and an empty reference both resolve to the catalog broadcast.
		source
			.request(None)
			.expect("no reference is always resolvable")
			.await
			.expect("catalog broadcast should resolve");
		let empty = PathRelative::empty();
		source
			.request(Some(&empty))
			.expect("empty reference is always resolvable")
			.await
			.expect("empty reference should resolve to the catalog broadcast");
	}

	#[tokio::test]
	async fn subscribe_track_resolves_catalog_broadcast() {
		let origin = Origin::random().produce();
		let mut producer = origin
			.create_broadcast("a/pub", moq_net::broadcast::Route::new().with_announce(true))
			.unwrap();
		// The track must exist for the subscription to resolve (SUBSCRIBE_OK).
		let _video = producer.create_track("video", None).unwrap();
		settle().await;

		let source = Source::new(origin.consume(), "a/pub");
		source
			.subscribe_track(None, "video")
			.await
			.expect("catalog track should resolve");
	}

	#[tokio::test]
	async fn self_reference_targets_catalog_broadcast() {
		let origin = Origin::random().produce();
		let mut producer = origin
			.create_broadcast("a/pub", moq_net::broadcast::Route::new().with_announce(true))
			.unwrap();
		let _video = producer.create_track("video", None).unwrap();
		settle().await;

		let source = Source::new(origin.consume(), "a/pub");

		// Walks back to the catalog's own path.
		let rel = PathRelative::new("../pub");
		source
			.subscribe_track(Some(&rel), "video")
			.await
			.expect("self-reference should resolve to the catalog broadcast");
	}

	#[tokio::test]
	async fn escaping_reference_is_rejected() {
		let origin = Origin::random().produce();

		let mut catalog = origin
			.create_broadcast("a/pub", moq_net::broadcast::Route::new().with_announce(true))
			.unwrap();
		let _catalog_video = catalog.create_track("video", None).unwrap();

		// The broadcast an escaping reference would land on if it clamped at the root.
		let mut clamped = origin
			.create_broadcast("elsewhere", moq_net::broadcast::Route::new().with_announce(true))
			.unwrap();
		let _clamped_video = clamped.create_track("video", None).unwrap();
		settle().await;

		let source = Source::new(origin.consume(), "a/pub");

		// `a/pub` has two segments, so a third `..` walks above the root. `../..` stops at
		// the root, which still names a broadcast, so it is not in this set.
		for reference in ["../../../elsewhere", "../../..", "../../../.."] {
			let rel = PathRelative::new(reference);
			assert!(source.request(Some(&rel)).is_err(), "{reference} should be rejected");

			// Neither `elsewhere` nor the catalog broadcast answers it: the rendition names
			// no broadcast at all.
			match source.resolve(Some(&rel)).await {
				Err(crate::Error::EscapingBroadcast(_)) => {}
				Err(err) => panic!("{reference} failed with the wrong error: {err:?}"),
				Ok(_) => panic!("{reference} should not resolve to any broadcast"),
			}
			match source.subscribe_track(Some(&rel), "video").await {
				Err(crate::Error::EscapingBroadcast(_)) => {}
				Err(err) => panic!("{reference} failed with the wrong error: {err:?}"),
				Ok(_) => panic!("{reference} should not resolve to any broadcast"),
			}
		}
	}

	#[tokio::test]
	async fn subscribe_track_resolves_referenced_broadcast() {
		let origin = Origin::random().produce();

		let _catalog = origin
			.create_broadcast("a/pub", moq_net::broadcast::Route::new().with_announce(true))
			.unwrap();

		let mut referenced = origin
			.create_broadcast("a/source", moq_net::broadcast::Route::new().with_announce(true))
			.unwrap();
		let _video = referenced.create_track("video", None).unwrap();
		settle().await;

		let source = Source::new(origin.consume(), "a/pub");

		// The reference resolves to `a/source`, whose "video" track answers the subscribe.
		let rel = PathRelative::new("../source");
		source
			.subscribe_track(Some(&rel), "video")
			.await
			.expect("referenced track should resolve");
	}
}
