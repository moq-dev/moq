//! Export input: an origin plus the path of the broadcast whose catalog drives the export.
//!
//! A hang catalog rendition may reference a track published in *another*
//! broadcast via its `broadcast` field (a path relative to the catalog's
//! broadcast, e.g. `./source`). Resolving that reference needs the catalog
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

	/// Resolve a rendition's optional broadcast reference to an origin path.
	///
	/// A missing or empty reference returns the catalog broadcast path. A valid reference
	/// may return the empty root path, which still names a broadcast. `None` means the
	/// reference walked above the root and names nothing.
	pub fn resolve_reference(&self, rel: Option<&moq_net::PathRelative<'_>>) -> Option<moq_net::PathOwned> {
		match rel.filter(|rel| !rel.is_empty()) {
			Some(rel) => self.path.try_resolve(rel),
			None => Some(self.path.clone()),
		}
	}

	/// The path of the broadcast a rendition's `broadcast` reference names.
	///
	/// The erroring counterpart to [`Self::resolve_reference`], for the consumer side, where
	/// an escaping reference is a fault to report rather than a rendition to drop.
	fn target(&self, rel: Option<&moq_net::PathRelative<'_>>) -> crate::Result<moq_net::PathOwned> {
		self.resolve_reference(rel).ok_or_else(|| {
			let rel = rel.map_or("", |rel| rel.as_str());
			tracing::error!(rel, catalog = %self.path, "broadcast reference escapes the root");
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

	/// The skipping counterpart to [`Self::request`], returning `None` when `rel` walks above
	/// the origin root.
	///
	/// The exporters use it to drop a single bad rendition rather than fail the whole export.
	/// [`Self::retain_valid`] already removes those renditions, so this is the second half of
	/// the same policy rather than a different one.
	pub(crate) fn try_request(
		&self,
		rel: Option<&moq_net::PathRelative<'_>>,
	) -> Option<kio::Pending<moq_net::origin::Requesting>> {
		Some(self.origin.request_broadcast(&self.resolve_reference(rel)?))
	}

	/// Remove renditions whose broadcast reference escapes above the origin root.
	///
	/// Every section carrying renditions must be listed here; one left out silently exempts
	/// its renditions from the containment check, exactly as on the consumer side.
	pub(crate) fn retain_valid<E: crate::catalog::hang::CatalogExt>(
		&self,
		catalog: &mut crate::catalog::hang::Catalog<E>,
	) {
		self.retain_valid_references("video", &mut catalog.video.renditions);
		self.retain_valid_references("audio", &mut catalog.audio.renditions);
		self.retain_valid_references("text", &mut catalog.text.renditions);
	}

	/// Remove media renditions whose broadcast reference escapes above the origin root.
	pub(crate) fn retain_valid_media(&self, catalog: &mut hang::Catalog) {
		self.retain_valid_references("video", &mut catalog.video.renditions);
		self.retain_valid_references("audio", &mut catalog.audio.renditions);
		self.retain_valid_references("text", &mut catalog.text.renditions);
	}

	fn retain_valid_references<C: BroadcastConfig>(
		&self,
		kind: &'static str,
		renditions: &mut std::collections::BTreeMap<String, C>,
	) {
		renditions.retain(|name, config| {
			let valid = self.resolve_reference(config.broadcast()).is_some();
			if !valid {
				tracing::warn!(
					rendition = name,
					kind,
					"ignoring rendition whose broadcast escapes above the root"
				);
			}
			valid
		});
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

trait BroadcastConfig {
	fn broadcast(&self) -> Option<&moq_net::PathRelativeOwned>;
}

impl BroadcastConfig for hang::catalog::VideoConfig {
	fn broadcast(&self) -> Option<&moq_net::PathRelativeOwned> {
		self.broadcast.as_ref()
	}
}

impl BroadcastConfig for hang::catalog::AudioConfig {
	fn broadcast(&self) -> Option<&moq_net::PathRelativeOwned> {
		self.broadcast.as_ref()
	}
}

impl BroadcastConfig for hang::catalog::TextConfig {
	fn broadcast(&self) -> Option<&moq_net::PathRelativeOwned> {
		self.broadcast.as_ref()
	}
}

/// Test helper: build an origin producer, spawning its driver on the ambient runtime.
#[cfg(test)]
pub(crate) fn produce_origin() -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::Origin::random().into());
	if tokio::runtime::Handle::try_current().is_ok() {
		tokio::spawn(driver);
	} else {
		// A sync test: nothing polls the driver, and dropping it would tear
		// the origin down, so leak it and rely on the synchronous half.
		std::mem::forget(driver);
	}
	producer
}

/// Test helper: serve `broadcast` on a throwaway origin's dynamic handler and return a
/// [`Source`] rooted at it, so exporter tests that build a local broadcast can still resolve
/// it by path. The origin is leaked so the broadcast stays reachable for the source's
/// lifetime (harmless in a test binary).
#[cfg(test)]
pub(crate) fn announced(broadcast: &moq_net::broadcast::Consumer) -> Source {
	let origin = produce_origin();
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
	use hang::catalog::{H264, VideoConfig};
	use moq_net::PathRelative;

	/// Let the origin's spawned attach task run: a created broadcast becomes
	/// routable asynchronously, shortly after `create_broadcast` returns.
	async fn settle() {
		for _ in 0..10 {
			tokio::task::yield_now().await;
		}
	}

	#[tokio::test]
	async fn no_override_targets_catalog_broadcast() {
		let origin = produce_origin();
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
		let origin = produce_origin();
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
		let origin = produce_origin();
		let mut producer = origin
			.create_broadcast("a/pub", moq_net::broadcast::Route::new().with_announce(true))
			.unwrap();
		let _video = producer.create_track("video", None).unwrap();
		settle().await;

		let source = Source::new(origin.consume(), "a/pub");

		// Names the catalog within its own parent.
		let rel = PathRelative::new("./pub");
		source
			.subscribe_track(Some(&rel), "video")
			.await
			.expect("self-reference should resolve to the catalog broadcast");
	}

	#[tokio::test]
	async fn escaping_reference_is_rejected() {
		let origin = produce_origin();

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

		// A reference resolves against the catalog's parent (`a`), so a second `..` walks
		// above the root. A lone `..` stops at the root, which still names a broadcast, so
		// it is not in this set.
		for reference in ["../../elsewhere", "../..", "../../.."] {
			let rel = PathRelative::new(reference);
			assert!(
				source.resolve_reference(Some(&rel)).is_none(),
				"{reference} should escape"
			);
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

	#[test]
	fn escaping_rendition_is_removed_while_valid_sibling_remains() {
		let origin = produce_origin();
		let source = Source::new(origin.consume(), "a/pub");
		let mut escaped = VideoConfig::new(H264 {
			profile: 0x42,
			constraints: 0,
			level: 0x1e,
			inline: false,
		});
		escaped.broadcast = Some(PathRelative::new("../../source").to_owned());
		let mut sibling = escaped.clone();
		sibling.broadcast = Some(PathRelative::new("./source").to_owned());

		let mut catalog = hang::Catalog::default();
		catalog.video.renditions.insert("escaped".to_string(), escaped);
		catalog.video.renditions.insert("sibling".to_string(), sibling);
		source.retain_valid_media(&mut catalog);

		assert!(!catalog.video.renditions.contains_key("escaped"));
		assert!(catalog.video.renditions.contains_key("sibling"));
	}

	/// The filter covers every section carrying renditions, not just the media ones. Text
	/// is the section that only exists on one side of the containment check by default, so
	/// it is the one that silently goes unchecked when the two sides drift.
	#[test]
	fn escaping_text_rendition_is_removed() {
		let origin = produce_origin();
		let source = Source::new(origin.consume(), "a/pub");

		let mut escaped = hang::catalog::TextConfig::new(hang::catalog::TextFormat::Vtt);
		escaped.broadcast = Some(PathRelative::new("../../source").to_owned());
		let mut sibling = escaped.clone();
		sibling.broadcast = Some(PathRelative::new("./source").to_owned());

		let mut catalog = hang::Catalog::default();
		catalog.text.renditions.insert("escaped".to_string(), escaped);
		catalog.text.renditions.insert("sibling".to_string(), sibling);
		source.retain_valid_media(&mut catalog);

		assert!(!catalog.text.renditions.contains_key("escaped"));
		assert!(catalog.text.renditions.contains_key("sibling"));
	}

	#[tokio::test]
	async fn subscribe_track_resolves_referenced_broadcast() {
		let origin = produce_origin();

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
		let rel = PathRelative::new("./source");
		source
			.subscribe_track(Some(&rel), "video")
			.await
			.expect("referenced track should resolve");
	}

	#[tokio::test]
	async fn dot_resolves_output_parent() {
		let origin = produce_origin();

		let _catalog = origin
			.create_broadcast(
				"a/source/transcode",
				moq_net::broadcast::Route::new().with_announce(true),
			)
			.unwrap();

		let mut referenced = origin
			.create_broadcast("a/source", moq_net::broadcast::Route::new().with_announce(true))
			.unwrap();
		let _video = referenced.create_track("video", None).unwrap();
		settle().await;

		let source = Source::new(origin.consume(), "a/source/transcode");
		let rel = PathRelative::new(".");
		source
			.subscribe_track(Some(&rel), "video")
			.await
			.expect("dot should resolve to the catalog broadcast's parent");
	}

	#[tokio::test]
	async fn dot_resolves_one_segment_catalog_to_root() {
		let origin = produce_origin();

		let _catalog = origin
			.create_broadcast("top", moq_net::broadcast::Route::new().with_announce(true))
			.unwrap();

		let mut root = origin
			.create_broadcast("", moq_net::broadcast::Route::new().with_announce(true))
			.unwrap();
		let _video = root.create_track("video", None).unwrap();
		settle().await;

		let source = Source::new(origin.consume(), "top");
		let rel = PathRelative::new(".");
		source
			.subscribe_track(Some(&rel), "video")
			.await
			.expect("dot should resolve to the empty root broadcast");
	}
}
