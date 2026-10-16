//! Export input: an origin plus the catalog broadcast that drives the export.
//!
//! A hang catalog rendition may reference a track published in *another*
//! broadcast via its `broadcast` field (a path relative to the catalog's
//! broadcast, e.g. `./source`). Resolving that reference needs the catalog
//! broadcast's own path and an [`moq_net::origin::Consumer`] to fetch the
//! referenced broadcast from. [`Source`] bundles the two, and resolves every
//! referenced broadcast through the same origin so
//! [`request_broadcast`](moq_net::origin::Consumer::request_broadcast) deduplicates
//! shared subscriptions.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::task::{Poll, ready};

/// The broadcast held for each path, shared by a source's clones and its requests.
type Pins = Arc<Mutex<HashMap<moq_net::PathOwned, moq_net::broadcast::Consumer>>>;

/// The subscription side of an export: an origin and the catalog broadcast that drives it.
///
/// Every rendition (including ones whose catalog `broadcast` field references a sibling
/// broadcast) resolves against `origin`, so a source can always follow a cross-broadcast
/// reference. Build one with [`Source::new`].
///
/// A source serves its own path from the broadcast it was built from, and keeps the first
/// broadcast it resolves for each sibling. Every later request for a path is served from that
/// handle ([`broadcast`](Self::broadcast), [`catalog`](Self::catalog),
/// [`resolve`](Self::resolve), [`bind`](Self::bind), [`subscribe_track`](Self::subscribe_track),
/// and an export's track requests). A route replaced after that is never resolved, epoch or
/// not, so two publishers never splice into one export; a request on a held broadcast that has
/// ended fails instead. Clones share the held broadcasts.
#[derive(Clone)]
pub struct Source {
	origin: moq_net::origin::Consumer,
	path: moq_net::PathOwned,
	pins: Pins,
	/// What each rendition an export reads through this source has received. See
	/// [`stats`](Self::stats).
	received: Arc<crate::stats::Registry<crate::stats::Received>>,
}

impl Source {
	/// A source driven by the catalog of `broadcast`, resolving its references through `origin`.
	///
	/// `broadcast` is the catalog broadcast as resolved through `origin`: a rendition's relative
	/// `broadcast` reference resolves against its [path](moq_net::broadcast::Info::path), and the
	/// referenced broadcast is fetched via
	/// [`origin.request_broadcast`](moq_net::origin::Consumer::request_broadcast), so it must be
	/// reachable through `origin` (by exact path, or served by a dynamic handler). A standalone
	/// broadcast has an empty path, so it is its own root and any `..` reference escapes.
	pub fn new(origin: moq_net::origin::Consumer, broadcast: moq_net::broadcast::Consumer) -> Self {
		let path = broadcast.info().path.clone();
		let pins = HashMap::from([(path.clone(), broadcast)]);
		Self {
			origin,
			path,
			pins: Arc::new(Mutex::new(pins)),
			received: Default::default(),
		}
	}

	/// This source holding nothing but `broadcast` at its own path: an export switching to
	/// another instance starts over from the broadcast it switched to. Its counters carry on.
	pub(crate) fn switched(&self, broadcast: &moq_net::broadcast::Consumer) -> Self {
		let pins = HashMap::from([(self.path.clone(), broadcast.clone())]);
		Self {
			origin: self.origin.clone(),
			path: self.path.clone(),
			pins: Arc::new(Mutex::new(pins)),
			received: self.received.clone(),
		}
	}

	/// The counters an export reading rendition `name` bumps.
	pub(crate) fn received(&self, name: &str) -> Arc<crate::stats::Received> {
		self.received.track(name)
	}

	/// What each rendition exported through this source (and its clones) has received, keyed by
	/// rendition ID in the catalog that listed it, including renditions referenced from another
	/// broadcast.
	///
	/// Counters are cumulative for the life of the source, across resubscriptions and a
	/// [`Follower`](crate::container::ts::Follower) switching instances, so a rendition no export
	/// reads any more keeps its final counts. Fold this into an
	/// [`echo::Snapshot`](hang::echo::Snapshot) on the feedback interval.
	pub fn stats(&self) -> std::collections::BTreeMap<String, hang::echo::Rendition> {
		self.received.received()
	}

	/// Serve this source's own path from `broadcast` from now on, as when the same instance
	/// returns on a new route and the old broadcast's routes are gone.
	pub(crate) fn pin(&self, broadcast: &moq_net::broadcast::Consumer) {
		let mut pins = self.pins.lock().expect("pins poisoned");
		pins.insert(self.path.clone(), broadcast.clone());
	}

	pub(crate) fn origin(&self) -> &moq_net::origin::Consumer {
		&self.origin
	}

	pub(crate) fn path(&self) -> &moq_net::PathOwned {
		&self.path
	}

	/// The catalog broadcast, the one at this source's path.
	pub fn broadcast(&self) -> moq_net::broadcast::Consumer {
		let pins = self.pins.lock().expect("pins poisoned");
		pins.get(&self.path).expect("the own path is always held").clone()
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
		crate::catalog::Consumer::new(&self.broadcast(), format).await
	}

	/// The catalog broadcast, as a binding.
	pub(crate) fn request_catalog(&self) -> Binding {
		Binding::new(self.broadcast())
	}

	/// The broadcast `path` is pinned to, or a lookup that pins its answer.
	fn request_path(&self, path: &moq_net::PathOwned) -> Binding {
		if let Some(pinned) = self.pins.lock().expect("pins poisoned").get(path) {
			return Binding::new(pinned.clone());
		}
		Binding(Bound::Requested {
			request: self.origin.request_broadcast(path, None).into_inner(),
			pins: self.pins.clone(),
			path: path.clone(),
		})
	}

	/// Resolve a rendition's optional broadcast reference to an origin path.
	///
	/// A missing or empty reference returns the catalog broadcast path. A valid reference
	/// may return the empty root path, which still names a broadcast. `None` means the
	/// reference walked above the root and names nothing.
	pub fn resolve_reference(&self, rel: Option<&moq_net::path::Relative<'_>>) -> Option<moq_net::PathOwned> {
		match rel.filter(|rel| !rel.is_empty()) {
			Some(rel) => self.path.try_resolve(rel),
			None => Some(self.path.clone()),
		}
	}

	/// The path of the broadcast a rendition's `broadcast` reference names.
	///
	/// The erroring counterpart to [`Self::resolve_reference`], for the consumer side, where
	/// an escaping reference is a fault to report rather than a rendition to drop.
	fn target(&self, rel: Option<&moq_net::path::Relative<'_>>) -> crate::Result<moq_net::PathOwned> {
		self.resolve_reference(rel).ok_or_else(|| {
			let rel = rel.map_or("", |rel| rel.as_str());
			tracing::error!(rel, catalog = %self.path, "broadcast reference escapes the root");
			crate::Error::EscapingBroadcast(rel.to_string())
		})
	}

	/// The skipping counterpart to [`Self::bind`], returning `None` when `rel` walks above
	/// the origin root.
	///
	/// The exporters use it to drop a single bad rendition rather than fail the whole export.
	/// [`Self::retain_selectable`] already removes those renditions, so this is the second half of
	/// the same policy rather than a different one.
	pub(crate) fn try_request(&self, rel: Option<&moq_net::path::Relative<'_>>) -> Option<Binding> {
		Some(self.request_path(&self.resolve_reference(rel)?))
	}

	/// Remove the renditions an export must not select: a disabled one (catalog
	/// `enabled: false`), or one whose broadcast reference escapes above the origin root.
	///
	/// Every export runs this on each catalog snapshot, so a disabled rendition reads as
	/// absent: one that is disabled later leaves the snapshot, and one that re-enables
	/// returns to it. Every section carrying renditions must be listed here; one left out
	/// silently exempts its renditions from the containment check, exactly as on the
	/// consumer side.
	pub(crate) fn retain_selectable<E: crate::catalog::hang::CatalogExt>(
		&self,
		catalog: &mut crate::catalog::hang::Catalog<E>,
	) {
		catalog.video.renditions.retain(|_, config| config.enabled);
		catalog.audio.renditions.retain(|_, config| config.enabled);
		self.retain_valid_references("video", &mut catalog.video.renditions);
		self.retain_valid_references("audio", &mut catalog.audio.renditions);
		self.retain_valid_references("text", &mut catalog.text.renditions);
		self.retain_valid_references("json", &mut catalog.json.tracks);
		self.retain_valid_references("binary", &mut catalog.binary.tracks);
	}

	fn retain_valid_references<C: hang::catalog::Reference>(
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
		rel: Option<&moq_net::path::Relative<'_>>,
	) -> crate::Result<moq_net::broadcast::Consumer> {
		self.bind(rel)?.broadcast().await
	}

	/// Start one broadcast request now and retain its result for repeated reads.
	///
	/// Unlike [`resolve`](Self::resolve), this issues the request without awaiting or polling.
	/// A remote or dynamic handler may answer later. The binding never retries a failed request
	/// or resolves the path again after a publisher is replaced.
	///
	/// A path this source already resolved binds to that broadcast; otherwise the origin is
	/// asked, deduplicating repeat requests for one path so the catalog and every rendition share
	/// one upstream subscription, and its answer is pinned. Use [`Binding::new`] when the
	/// intended broadcast is already in hand.
	///
	/// Rejects an escaping reference exactly as [`resolve`](Self::resolve) does.
	pub fn bind(&self, rel: Option<&moq_net::path::Relative<'_>>) -> crate::Result<Binding> {
		Ok(self.request_path(&self.target(rel)?))
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
		rel: Option<&moq_net::path::Relative<'_>>,
		name: &str,
	) -> crate::Result<moq_net::track::Subscriber> {
		let broadcast = self.bind(rel)?.broadcast().await?;
		Ok(broadcast.track(name)?.subscribe(None).await?)
	}
}

/// A held broadcast or the retained result of one eagerly issued broadcast request.
///
/// Reading the binding never looks up its path again, even after a failure or publisher
/// replacement. Pending requests follow the origin's routing semantics; issuing a request
/// does not establish an epoch relationship with a separate catalog broadcast.
///
/// Build one with [`Source::bind`], or with [`Binding::new`] for a broadcast already in hand.
pub struct Binding(Bound);

enum Bound {
	/// A broadcast the caller already holds.
	Ready(moq_net::broadcast::Consumer),
	/// A request issued when the binding was made.
	Requested {
		request: moq_net::origin::Requesting,
		/// Where the answer is pinned, the first for its path winning.
		pins: Pins,
		path: moq_net::PathOwned,
	},
}

impl Binding {
	/// Bind to a broadcast already in hand, for a reference that named it.
	pub fn new(broadcast: moq_net::broadcast::Consumer) -> Self {
		Self(Bound::Ready(broadcast))
	}

	/// Poll for the bound broadcast without blocking.
	///
	/// Concurrent polls and cancelled waits retain the same request and result.
	pub fn poll_broadcast(&self, waiter: &kio::Waiter) -> Poll<crate::Result<moq_net::broadcast::Consumer>> {
		match &self.0 {
			Bound::Ready(broadcast) => Poll::Ready(Ok(broadcast.clone())),
			Bound::Requested { request, pins, path } => {
				let broadcast = ready!(request.poll_ok(waiter))?;
				// A request that raced another for the path takes the one pinned first.
				let mut pins = pins.lock().expect("pins poisoned");
				Poll::Ready(Ok(pins.entry(path.clone()).or_insert(broadcast).clone()))
			}
		}
	}

	/// The bound broadcast, awaiting the origin's answer when it hasn't arrived yet.
	///
	/// Concurrent reads and cancelled waits retain the same request and result.
	pub async fn broadcast(&self) -> crate::Result<moq_net::broadcast::Consumer> {
		kio::wait(|waiter| self.poll_broadcast(waiter)).await
	}
}

/// Test helper: build an origin producer, spawning its driver on the ambient runtime.
#[cfg(test)]
pub(crate) fn produce_origin() -> moq_net::origin::Producer {
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

/// Test helper: a [`Source`] for the broadcast at `path`, resolved through `origin`.
#[cfg(test)]
pub(crate) async fn resolved(origin: &moq_net::origin::Producer, path: &str) -> Source {
	let consumer = origin.consume();
	let broadcast = consumer
		.request_broadcast(path, None)
		.await
		.expect("the broadcast resolves");
	Source::new(consumer, broadcast)
}

/// Test helper: serve `broadcast` at `test` (and every other path) on a throwaway origin's
/// dynamic handler and return a [`Source`] built from it, so exporter tests that build a
/// local broadcast still resolve references through an origin. The origin is leaked so the
/// broadcast stays reachable for the source's lifetime (harmless in a test binary).
#[cfg(test)]
pub(crate) async fn announced(broadcast: &moq_net::broadcast::Consumer) -> Source {
	let origin = produce_origin();
	let dynamic = origin.dynamic("", Default::default()).unwrap();
	let served = broadcast.clone();
	tokio::spawn(async move {
		while let Ok(request) = dynamic.requested_broadcast().await {
			request.accept(served.clone());
		}
	});
	let source = resolved(&origin, "test").await;
	Box::leak(Box::new(origin));
	source
}

#[cfg(test)]
mod tests {
	use super::*;
	use hang::catalog::{H264, VideoConfig};
	use moq_net::path::Relative;

	/// Let the origin's driver run the fronts that requests and announcements
	/// started: they serve asynchronously, shortly after the call returns.
	async fn settle() {
		for _ in 0..10 {
			tokio::task::yield_now().await;
		}
	}

	#[tokio::test]
	async fn binding_retains_a_pending_request_across_cancelled_and_repeated_reads() {
		let origin = produce_origin();
		let _catalog = origin.publish("pub", Default::default()).unwrap();
		settle().await;
		let source = resolved(&origin, "pub").await;
		let dynamic = origin.dynamic("media", Default::default()).unwrap();
		let binding = source.bind(Some(&Relative::new("media"))).unwrap();
		let request = dynamic.requested_broadcast().await.unwrap();

		assert!(
			binding.poll_broadcast(&kio::Waiter::noop()).is_pending(),
			"the handler has not answered"
		);

		tokio::select! {
			biased;
			_ = binding.broadcast() => panic!("the handler has not answered"),
			_ = std::future::ready(()) => {}
		}

		let producer = moq_net::broadcast::Info::default().produce();
		let (first, second, ()) = tokio::join!(binding.broadcast(), binding.broadcast(), async {
			request.accept(producer.consume());
		});
		let first = first.unwrap();
		assert!(!first.is_closed());
		assert!(first.is_clone(&second.unwrap()));
		drop((producer, dynamic));
		first.closed().await;
		assert!(first.is_clone(&binding.broadcast().await.unwrap()));
	}

	/// A failed lookup pins nothing: the guarantee starts at a path's first resolution.
	#[tokio::test]
	async fn failed_binding_does_not_retry_a_later_publisher() {
		let origin = produce_origin();
		let _catalog = origin.publish("pub", Default::default()).unwrap();
		settle().await;
		let source = resolved(&origin, "pub").await;
		let media = Relative::new("media");
		let binding = source.bind(Some(&media)).unwrap();
		assert!(binding.broadcast().await.is_err());
		let _publisher = origin.publish("media", Default::default()).unwrap();
		settle().await;
		assert!(!source.resolve(Some(&media)).await.unwrap().is_closed());
		assert!(binding.broadcast().await.is_err());
	}

	/// A source serves its path from the broadcast it was built from, even when a replacement
	/// already won the path: a track only the replacement carries is not read from it.
	#[tokio::test(start_paused = true)]
	async fn a_source_keeps_the_broadcast_it_was_built_from() {
		let origin = produce_origin();
		let _old = origin.publish("live", Default::default()).unwrap();
		settle().await;
		let consumer = origin.consume();
		let first = consumer.request_broadcast("live", None).await.unwrap();

		let new = origin.publish("live", Default::default()).unwrap();
		let _late = new.create_track("late", None).unwrap();
		settle().await;
		let source = Source::new(consumer, first.clone());
		assert!(source.broadcast().is_clone(&first));
		assert!(source.resolve(None).await.unwrap().is_clone(&first));
		assert!(
			source.subscribe_track(None, "late").await.is_err(),
			"the replacement's track is never read"
		);
	}

	/// A sibling reference stays on the broadcast it first resolved too.
	#[tokio::test(start_paused = true)]
	async fn a_source_keeps_the_sibling_it_resolved() {
		let origin = produce_origin();
		let _catalog = origin.publish("a/pub", Default::default()).unwrap();
		let _old = origin.publish("a/source", Default::default()).unwrap();
		settle().await;
		let source = resolved(&origin, "a/pub").await;
		let sibling = Relative::new("./source");
		let first = source.resolve(Some(&sibling)).await.expect("the sibling resolves");

		let new = origin.publish("a/source", Default::default()).unwrap();
		let _late = new.create_track("late", None).unwrap();
		settle().await;
		assert!(source.resolve(Some(&sibling)).await.unwrap().is_clone(&first));
		assert!(
			source.subscribe_track(Some(&sibling), "late").await.is_err(),
			"the replacement's track is never read"
		);
		let bound = source.bind(Some(&sibling)).unwrap().broadcast().await.unwrap();
		assert!(bound.is_clone(&first), "a binding honors the pin too");
	}

	/// Every self-reference serves the broadcast the source was built from, even once it
	/// ended and a replacement took the path.
	#[tokio::test(start_paused = true)]
	async fn self_references_keep_the_catalog_broadcast_after_replacement() {
		let origin = produce_origin();
		let old = origin.publish("a/live", Default::default()).unwrap();
		settle().await;
		let source = resolved(&origin, "a/live").await;
		drop(old);
		let _replacement = origin.publish("a/live", Default::default()).unwrap();
		settle().await;

		for reference in [None, Some(""), Some("live"), Some("./live"), Some("../a/live")] {
			let rel = reference.map(Relative::new);
			let bound = source.bind(rel.as_ref()).unwrap().broadcast().await.unwrap();
			assert!(
				bound.is_closed(),
				"{reference:?} must keep the original catalog broadcast"
			);
		}
	}

	#[tokio::test]
	async fn no_override_targets_catalog_broadcast() {
		let origin = produce_origin();
		let _producer = origin.create_broadcast("a/pub").unwrap();
		_producer.announce(Default::default()).unwrap();
		settle().await;

		let source = resolved(&origin, "a/pub").await;

		// No reference and an empty reference both resolve to the catalog broadcast.
		source
			.bind(None)
			.expect("no reference is always resolvable")
			.broadcast()
			.await
			.expect("catalog broadcast should resolve");
		let empty = Relative::empty();
		source
			.bind(Some(&empty))
			.expect("empty reference is always resolvable")
			.broadcast()
			.await
			.expect("empty reference should resolve to the catalog broadcast");
	}

	#[tokio::test]
	async fn subscribe_track_resolves_catalog_broadcast() {
		let origin = produce_origin();
		let producer = origin.create_broadcast("a/pub").unwrap();
		producer.announce(Default::default()).unwrap();
		// The track must exist for the subscription to resolve (SUBSCRIBE_OK).
		let _video = producer.create_track("video", None).unwrap();
		settle().await;

		let source = resolved(&origin, "a/pub").await;
		source
			.subscribe_track(None, "video")
			.await
			.expect("catalog track should resolve");
	}

	#[tokio::test]
	async fn self_reference_targets_catalog_broadcast() {
		let origin = produce_origin();
		let producer = origin.create_broadcast("a/pub").unwrap();
		producer.announce(Default::default()).unwrap();
		let _video = producer.create_track("video", None).unwrap();
		settle().await;

		let source = resolved(&origin, "a/pub").await;

		// Names the catalog within its own parent.
		let rel = Relative::new("./pub");
		source
			.subscribe_track(Some(&rel), "video")
			.await
			.expect("self-reference should resolve to the catalog broadcast");
	}

	#[tokio::test]
	async fn escaping_reference_is_rejected() {
		let origin = produce_origin();

		let catalog = origin.create_broadcast("a/pub").unwrap();
		catalog.announce(Default::default()).unwrap();
		let _catalog_video = catalog.create_track("video", None).unwrap();

		// The broadcast an escaping reference would land on if it clamped at the root.
		let clamped = origin.create_broadcast("elsewhere").unwrap();
		clamped.announce(Default::default()).unwrap();
		let _clamped_video = clamped.create_track("video", None).unwrap();
		settle().await;

		let source = resolved(&origin, "a/pub").await;

		// A reference resolves against the catalog's parent (`a`), so a second `..` walks
		// above the root. A lone `..` stops at the root, which still names a broadcast, so
		// it is not in this set.
		for reference in ["../../elsewhere", "../..", "../../.."] {
			let rel = Relative::new(reference);
			assert!(
				source.resolve_reference(Some(&rel)).is_none(),
				"{reference} should escape"
			);
			assert!(source.bind(Some(&rel)).is_err(), "{reference} should be rejected");

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

	/// A source over a broadcast created at `a/pub`, built without awaiting the origin.
	fn created(origin: &moq_net::origin::Producer) -> Source {
		let broadcast = origin.create_broadcast("a/pub").unwrap().consume();
		Source::new(origin.consume(), broadcast)
	}

	#[test]
	fn escaping_rendition_is_removed_while_valid_sibling_remains() {
		let origin = produce_origin();
		let source = created(&origin);
		let mut escaped = VideoConfig::new(H264 {
			profile: 0x42,
			constraints: 0,
			level: 0x1e,
			inline: false,
		});
		escaped.broadcast = Some(Relative::new("../../source").to_owned());
		let mut sibling = escaped.clone();
		sibling.broadcast = Some(Relative::new("./source").to_owned());

		let mut catalog = hang::Catalog::<()>::default();
		catalog.video.renditions.insert("escaped".to_string(), escaped);
		catalog.video.renditions.insert("sibling".to_string(), sibling);
		source.retain_selectable(&mut catalog);

		assert!(!catalog.video.renditions.contains_key("escaped"));
		assert!(catalog.video.renditions.contains_key("sibling"));
	}

	/// A disabled rendition is never selected, while its enabled sibling is.
	#[test]
	fn disabled_rendition_is_removed_while_enabled_sibling_remains() {
		let origin = produce_origin();
		let source = created(&origin);
		let enabled = VideoConfig::new(H264 {
			profile: 0x42,
			constraints: 0,
			level: 0x1e,
			inline: false,
		});
		let mut disabled = enabled.clone();
		disabled.enabled = false;
		let mut muted = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2);
		muted.enabled = false;

		let mut catalog = hang::Catalog::<()>::default();
		catalog.video.renditions.insert("disabled".to_string(), disabled);
		catalog.video.renditions.insert("enabled".to_string(), enabled);
		catalog.audio.renditions.insert("muted".to_string(), muted);
		source.retain_selectable(&mut catalog);

		assert_eq!(catalog.video.renditions.keys().collect::<Vec<_>>(), vec!["enabled"]);
		assert!(catalog.audio.renditions.is_empty());
	}

	/// The filter covers every section carrying renditions, not just the media ones. Text
	/// is the section that only exists on one side of the containment check by default, so
	/// it is the one that silently goes unchecked when the two sides drift.
	#[test]
	fn escaping_text_rendition_is_removed() {
		let origin = produce_origin();
		let source = created(&origin);

		let mut escaped = hang::catalog::TextConfig::new(hang::catalog::TextFormat::Vtt);
		escaped.broadcast = Some(Relative::new("../../source").to_owned());
		let mut sibling = escaped.clone();
		sibling.broadcast = Some(Relative::new("./source").to_owned());

		let mut catalog = hang::Catalog::<()>::default();
		catalog.text.renditions.insert("escaped".to_string(), escaped);
		catalog.text.renditions.insert("sibling".to_string(), sibling);
		source.retain_selectable(&mut catalog);

		assert!(!catalog.text.renditions.contains_key("escaped"));
		assert!(catalog.text.renditions.contains_key("sibling"));
	}

	#[tokio::test]
	async fn subscribe_track_resolves_referenced_broadcast() {
		let origin = produce_origin();

		let _catalog = origin.create_broadcast("a/pub").unwrap();
		_catalog.announce(Default::default()).unwrap();

		let referenced = origin.create_broadcast("a/source").unwrap();
		referenced.announce(Default::default()).unwrap();
		let _video = referenced.create_track("video", None).unwrap();
		settle().await;

		let source = resolved(&origin, "a/pub").await;

		// The reference resolves to `a/source`, whose "video" track answers the subscribe.
		let rel = Relative::new("./source");
		source
			.subscribe_track(Some(&rel), "video")
			.await
			.expect("referenced track should resolve");
	}

	#[tokio::test]
	async fn dot_resolves_output_parent() {
		let origin = produce_origin();

		let _catalog = origin.create_broadcast("a/source/transcode").unwrap();
		_catalog.announce(Default::default()).unwrap();

		let referenced = origin.create_broadcast("a/source").unwrap();
		referenced.announce(Default::default()).unwrap();
		let _video = referenced.create_track("video", None).unwrap();
		settle().await;

		let source = resolved(&origin, "a/source/transcode").await;
		let rel = Relative::new(".");
		source
			.subscribe_track(Some(&rel), "video")
			.await
			.expect("dot should resolve to the catalog broadcast's parent");
	}

	#[tokio::test]
	async fn dot_resolves_one_segment_catalog_to_root() {
		let origin = produce_origin();

		let _catalog = origin.create_broadcast("top").unwrap();
		_catalog.announce(Default::default()).unwrap();

		let root = origin.create_broadcast("").unwrap();
		root.announce(Default::default()).unwrap();
		let _video = root.create_track("video", None).unwrap();
		settle().await;

		let source = resolved(&origin, "top").await;
		let rel = Relative::new(".");
		source
			.subscribe_track(Some(&rel), "video")
			.await
			.expect("dot should resolve to the empty root broadcast");
	}
}
