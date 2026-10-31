//! Export input: an origin plus the catalog broadcast that drives the export.
//!
//! A hang catalog rendition may reference a track published in *another*
//! broadcast via its `broadcast` field (a path relative to the catalog's
//! broadcast, e.g. `./source`). Resolving that reference needs the catalog
//! broadcast's own path and an [`moq_net::origin::Consumer`] to fetch the
//! referenced broadcast from. [`Source`] bundles the two, and resolves every
//! referenced broadcast through the same origin so
//! [`request`](moq_net::origin::Consumer::request) deduplicates
//! shared subscriptions.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::task::{Poll, ready};

/// The broadcast held for each path, shared by a source's clones and its requests.
type Pins = Arc<Mutex<HashMap<moq_net::Path, moq_net::broadcast::Consumer>>>;

#[derive(Default)]
struct Priorities {
	value: i8,
	controls: HashMap<usize, std::sync::Weak<moq_net::track::Control>>,
}

/// A program's control registration, removed with the subscription that owns it.
pub(crate) struct Priority {
	control: Arc<moq_net::track::Control>,
	shared: Arc<Mutex<Priorities>>,
}

impl Drop for Priority {
	fn drop(&mut self) {
		self.shared
			.lock()
			.expect("priorities poisoned")
			.controls
			.remove(&(Arc::as_ptr(&self.control) as usize));
	}
}

/// The subscription side of an export: an origin and the catalog broadcast that drives it.
///
/// Every rendition (including ones whose catalog `broadcast` field references a sibling
/// broadcast) resolves against `origin`, so a source can always follow a cross-broadcast
/// reference. Build one with [`Source::new`].
///
/// A source serves its own path from the broadcast it was built from, and keeps the first
/// broadcast it resolves for each sibling, waiting for the sibling's first announcement if
/// nothing serves it yet. Every later request for a path is served from that handle
/// ([`broadcast`](Self::broadcast), [`catalog`](Self::catalog), [`resolve`](Self::resolve),
/// [`bind`](Self::bind), [`subscribe_track`](Self::subscribe_track), and an export's track
/// requests). A route replaced after that is never resolved, epoch or not, so two publishers
/// never splice into one export; a request on a held broadcast that has ended fails instead of
/// waiting for the next one. Clones share the held broadcasts.
///
/// A reader that follows a sibling's announcements instead, restarting on each new instance,
/// uses [`follow`](Self::follow), which holds nothing.
///
/// A reference may name the instance it expects with an epoch (the catalog's optional `epoch`
/// beside `broadcast`). A held or resolved broadcast serving another epoch fails that reference
/// with [`Error::EpochMismatch`](crate::Error::EpochMismatch) rather than replacing the held
/// one or falling back to it. A broadcast resolved over a route without an epoch cannot prove
/// its instance, so there the epoch is best effort: the reference reads the held broadcast.
#[derive(Clone)]
pub struct Source {
	origin: moq_net::origin::Consumer,
	path: moq_net::Path,
	pins: Pins,
	priority: Arc<Mutex<Priorities>>,
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
	/// [`origin.request`](moq_net::origin::Consumer::request), so it must be
	/// reachable through `origin` (by exact path, or served by a dynamic handler). A standalone
	/// broadcast has an empty path, so it is its own root and any `..` reference escapes.
	pub fn new(origin: moq_net::origin::Consumer, broadcast: moq_net::broadcast::Consumer) -> Self {
		let path = broadcast.info().path.clone();
		let pins = HashMap::from([(path.clone(), broadcast)]);
		Self {
			origin,
			path,
			pins: Arc::new(Mutex::new(pins)),
			priority: Arc::default(),
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
			priority: Arc::default(),
			received: self.received.clone(),
		}
	}

	/// Give one export its own delivery preferences while sharing bindings and counters.
	pub(crate) fn isolate(&mut self) {
		self.priority = Arc::default();
	}

	/// Set the subscriber priority of this program's current and future tracks.
	pub(crate) fn priority(&self, value: i8) -> moq_net::Result<()> {
		let mut priority = self.priority.lock().expect("priorities poisoned");
		priority.value = value;
		for control in priority.controls.values().filter_map(std::sync::Weak::upgrade) {
			match control.update(control.subscription().with_priority(value)) {
				Ok(()) | Err(moq_net::Error::Closed) => {}
				Err(err) => return Err(err),
			}
		}
		Ok(())
	}

	/// Start a new track at the program's current priority.
	pub(crate) fn subscription(&self, subscription: moq_net::track::Subscription) -> moq_net::track::Subscription {
		subscription.with_priority(self.priority.lock().expect("priorities poisoned").value)
	}

	/// Keep a track at its program's priority until the returned registration drops.
	pub(crate) fn control(&self, control: moq_net::track::Control) -> moq_net::Result<Priority> {
		let mut priority = self.priority.lock().expect("priorities poisoned");
		match control.update(control.subscription().with_priority(priority.value)) {
			Ok(()) | Err(moq_net::Error::Closed) => {}
			Err(err) => return Err(err),
		}
		let control = Arc::new(control);
		priority
			.controls
			.insert(Arc::as_ptr(&control) as usize, Arc::downgrade(&control));
		Ok(Priority {
			control,
			shared: self.priority.clone(),
		})
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
	/// reads any more keeps its final counts, until 1,024 renditions are held: then those no export
	/// reads make way for new ones. Fold this into an
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

	pub(crate) fn path(&self) -> &moq_net::Path {
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

	/// The broadcast `path` is pinned to, or a lookup that waits for the path's first
	/// announcement and pins its answer, either checked against `epoch` on every read.
	pub(crate) fn request_path(&self, path: &moq_net::Path, epoch: Option<&moq_net::Epoch>) -> Binding {
		let pinned = self.pins.lock().expect("pins poisoned").get(path).cloned();
		let bound = match pinned {
			Some(pinned) => Bound::Ready(pinned),
			// Unpinned even with an epoch: an epochless route still answers, best effort, and
			// the read refuses a route that proves another instance.
			None => {
				let mut following = self.origin.follow(path, None);
				// Issue the request now: a handler answers it whether or not anyone reads yet.
				match poll_first(&mut following, &kio::Waiter::noop()) {
					// The follower hands an instance out once, so an answer on hand is pinned now.
					Poll::Ready(Ok(broadcast)) => {
						let mut pins = self.pins.lock().expect("pins poisoned");
						Bound::Ready(pins.entry(path.clone()).or_insert(broadcast).clone())
					}
					_ => Bound::Requested {
						following: Box::new(Mutex::new(following)),
						pins: self.pins.clone(),
						path: path.clone(),
					},
				}
			}
		};
		Binding {
			bound,
			epoch: epoch.cloned(),
			sibling: *path != self.path,
		}
	}

	/// Resolve a rendition's optional broadcast reference to an origin path.
	///
	/// A missing or empty reference returns the catalog broadcast path. A valid reference
	/// may return the empty root path, which still names a broadcast. `None` means the
	/// reference walked above the root and names nothing.
	pub fn resolve_reference(&self, rel: Option<&moq_net::path::Relative>) -> Option<moq_net::Path> {
		match rel.filter(|rel| !rel.is_empty()) {
			Some(rel) => self.path.try_resolve(rel),
			None => Some(self.path.clone()),
		}
	}

	/// The path of the broadcast a rendition's `broadcast` reference names.
	///
	/// The erroring counterpart to [`Self::resolve_reference`], for the consumer side, where
	/// an escaping reference is a fault to report rather than a rendition to drop.
	fn target(&self, rel: Option<&moq_net::path::Relative>) -> crate::Result<moq_net::Path> {
		self.resolve_reference(rel).ok_or_else(|| {
			let rel = rel.map_or("", |rel| rel.as_str());
			tracing::error!(rel, catalog = %self.path, "broadcast reference escapes the root");
			crate::Error::EscapingBroadcast(rel.to_string())
		})
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
	/// broadcast from the origin, waiting for its first announcement if nothing serves it yet.
	/// `epoch` is the rendition's `epoch` field, the instance the broadcast must be (see
	/// [`Source`]). Use it when you need the broadcast handle itself (e.g. to FETCH individual
	/// groups) rather than a subscription.
	///
	/// A reference that escapes above the origin root is
	/// [`Error::EscapingBroadcast`](crate::Error::EscapingBroadcast): the rendition names
	/// no broadcast, so there is nothing to resolve.
	pub async fn resolve(
		&self,
		rel: Option<&moq_net::path::Relative>,
		epoch: Option<&moq_net::Epoch>,
	) -> crate::Result<moq_net::broadcast::Consumer> {
		self.bind(rel, epoch)?.broadcast().await
	}

	/// Start one broadcast request now and retain its result for repeated reads.
	///
	/// Unlike [`resolve`](Self::resolve), this issues the request without awaiting or polling.
	/// A path nothing announces yet resolves once something does, and a remote or dynamic
	/// handler may answer later. A client session that only discovers what is watched is asked
	/// about the path while the binding waits. Once answered, the binding never resolves the path again, even
	/// after its publisher is replaced or goes; [`follow`](Self::follow) does.
	///
	/// A path this source already resolved binds to that broadcast; otherwise the origin is
	/// asked, deduplicating repeat requests for one path so the catalog and every rendition share
	/// one upstream subscription, and its answer is pinned. Use [`Binding::new`] when the
	/// intended broadcast is already in hand.
	///
	/// Rejects an escaping reference exactly as [`resolve`](Self::resolve) does, and checks
	/// `epoch` on every read.
	pub fn bind(
		&self,
		rel: Option<&moq_net::path::Relative>,
		epoch: Option<&moq_net::Epoch>,
	) -> crate::Result<Binding> {
		Ok(self.request_path(&self.target(rel)?, epoch))
	}

	/// Resolve an optional cross-broadcast reference and subscribe to track `name`,
	/// awaiting SUBSCRIBE_OK.
	///
	/// `rel` is a rendition's catalog `broadcast` field: `None` (or an empty reference)
	/// subscribes on the catalog broadcast; anything else is resolved as in
	/// [`resolve`](Self::resolve), waiting for its first announcement and checked against
	/// `epoch`. A
	/// reference that escapes above the origin root is
	/// [`Error::EscapingBroadcast`](crate::Error::EscapingBroadcast).
	///
	/// This is the async counterpart to the poll-driven container exporters: consumers
	/// that wrap a raw [`moq_net::track::Subscriber`] themselves (e.g. the WebRTC egress)
	/// use it to honor cross-broadcast renditions without reimplementing the path math.
	pub async fn subscribe_track(
		&self,
		rel: Option<&moq_net::path::Relative>,
		epoch: Option<&moq_net::Epoch>,
		name: &str,
	) -> crate::Result<moq_net::track::Subscriber> {
		let broadcast = self.bind(rel, epoch)?.broadcast().await?;
		Ok(broadcast.track(name)?.subscribe(None).await?)
	}

	/// Follow the broadcast a rendition's reference names across its announcements.
	///
	/// Holds nothing: each instance serving a sibling is handed out as it comes online or takes
	/// over, and its end is reported, so a reader restarts the rendition on each one. A
	/// reference to the catalog broadcast itself hands out only the instance this source was
	/// built from and never reports its end, since a reader follows its catalog's own end and
	/// restarts by building a new source.
	///
	/// With an `epoch`, only that instance (or an epochless route, which cannot prove another
	/// one) is handed out: another taking over ends the rendition rather than substituting for
	/// it. A self-reference whose epoch the catalog instance contradicts fails with
	/// [`Error::EpochMismatch`](crate::Error::EpochMismatch), since no other instance can come.
	/// Rejects an escaping reference exactly as [`resolve`](Self::resolve) does.
	pub fn follow(
		&self,
		rel: Option<&moq_net::path::Relative>,
		epoch: Option<&moq_net::Epoch>,
	) -> crate::Result<crate::follow::Instances> {
		let path = self.target(rel)?;
		Ok(match path == self.path {
			true => {
				let broadcast = self.broadcast();
				check_epoch(epoch, &broadcast)?;
				crate::follow::Instances::fixed(broadcast)
			}
			false => crate::follow::Instances::new(&self.origin, &path, epoch)?,
		})
	}
}

/// Fail with [`Error::EpochMismatch`](crate::Error::EpochMismatch) when `broadcast` is proven to
/// be another instance than `epoch` names. An epochless route cannot prove its instance, so only
/// a known mismatch fails.
pub(crate) fn check_epoch(
	epoch: Option<&moq_net::Epoch>,
	broadcast: &moq_net::broadcast::Consumer,
) -> crate::Result<()> {
	let info = broadcast.info();
	match (epoch, &info.epoch) {
		(Some(expected), Some(actual)) if expected != actual => Err(crate::Error::EpochMismatch {
			path: info.path.to_string(),
			expected: expected.clone(),
			actual: actual.clone(),
		}),
		_ => Ok(()),
	}
}

/// A held broadcast or the retained result of one eagerly issued broadcast request.
///
/// A pending request waits for a path nothing announces yet and rides out route churn until it
/// resolves (see [`follow`](moq_net::origin::Consumer::follow)). Once the
/// path is pinned, reading the binding answers that pin and never looks the path up again, even
/// after its publisher is replaced or goes. Issuing a request does not establish an epoch
/// relationship with a separate catalog broadcast. A binding made with an epoch refuses a
/// broadcast proven to serve another one.
///
/// Build one with [`Source::bind`], or with [`Binding::new`] for a broadcast already in hand.
pub struct Binding {
	bound: Bound,
	/// The instance the reference named, if any.
	epoch: Option<moq_net::Epoch>,
	/// The reference names another broadcast than the catalog's.
	sibling: bool,
}

enum Bound {
	/// A broadcast the caller already holds.
	Ready(moq_net::broadcast::Consumer),
	/// A request issued when the binding was made, resolved once something announces the path,
	/// or the failure it ended with.
	Requested {
		following: Box<Mutex<Following>>,
		/// Where the answer is pinned, the first for its path winning.
		pins: Pins,
		path: moq_net::Path,
	},
}

impl Binding {
	/// Bind to a broadcast already in hand, for a reference that named it.
	pub fn new(broadcast: moq_net::broadcast::Consumer) -> Self {
		Self {
			bound: Bound::Ready(broadcast),
			epoch: None,
			sibling: false,
		}
	}

	/// Whether the reference names another broadcast than the catalog's.
	pub(crate) fn sibling(&self) -> bool {
		self.sibling
	}

	/// Poll for the bound broadcast without blocking.
	///
	/// Concurrent polls and cancelled waits retain the same request and result.
	pub fn poll_broadcast(&self, waiter: &kio::Waiter) -> Poll<crate::Result<moq_net::broadcast::Consumer>> {
		let broadcast = match &self.bound {
			Bound::Ready(broadcast) => broadcast.clone(),
			Bound::Requested { following, pins, path } => {
				// The follower stays locked until its answer is pinned: it hands each instance
				// out once, so a concurrent read that polled it in between would wait for the next.
				let mut following = following.lock().expect("following poisoned");
				// Once a path is pinned, its pin is the answer: a request that raced another for
				// the path takes the one pinned first, and the follower is never polled again, since
				// it would wait out the gap after the pinned instance goes.
				if let Some(pinned) = pins.lock().expect("pins poisoned").get(path) {
					pinned.clone()
				} else {
					let broadcast = ready!(poll_first(&mut following, waiter))?;
					// The path's first answer is pinned even when it fails this binding's epoch, so
					// no later reference resolves the path independently.
					let mut pins = pins.lock().expect("pins poisoned");
					pins.entry(path.clone()).or_insert(broadcast).clone()
				}
			}
		};
		Poll::Ready(check_epoch(self.epoch.as_ref(), &broadcast).map(|()| broadcast))
	}

	/// The bound broadcast, awaiting the origin's answer when it hasn't arrived yet.
	///
	/// Concurrent reads and cancelled waits retain the same request and result.
	pub async fn broadcast(&self) -> crate::Result<moq_net::broadcast::Consumer> {
		kio::wait(|waiter| self.poll_broadcast(waiter)).await
	}
}

/// A binding's follower, or the failure it ended with.
type Following = Result<moq_net::origin::Following, moq_net::Error>;

/// Poll a binding's follower for its first instance, keeping the failure it ends with so every
/// later read answers the same.
fn poll_first(
	following: &mut Following,
	waiter: &kio::Waiter,
) -> Poll<Result<moq_net::broadcast::Consumer, moq_net::Error>> {
	let result = match following {
		Ok(following) => ready!(following.poll_available(waiter)),
		Err(err) => Err(err.clone()),
	};
	if let Err(err) = &result {
		*following = Err(err.clone());
	}
	Poll::Ready(result)
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
	let broadcast = consumer.request(path, None).await.expect("the broadcast resolves");
	Source::new(consumer, broadcast)
}

/// Test helper: a [`Source`] for a standalone `broadcast`, resolving its references through
/// a throwaway origin that serves nothing. The origin is leaked so its driver runs for the
/// source's lifetime (harmless in a test binary).
#[cfg(test)]
pub(crate) async fn announced(broadcast: &moq_net::broadcast::Consumer) -> Source {
	let origin = produce_origin();
	let source = Source::new(origin.consume(), broadcast.clone());
	Box::leak(Box::new(origin));
	source
}

#[cfg(test)]
mod tests {
	use super::*;
	use hang::catalog::{H264, VideoConfig};
	use moq_net::path::Relative;

	/// Priorities belong to one export, follow late tracks, preserve delivery bounds, and
	/// retain no subscription once its owning reader drops.
	#[tokio::test(start_paused = true)]
	async fn export_priorities_follow_subscription_ownership() {
		let origin = produce_origin();
		let source = created(&origin);
		let mut independent = source.clone();
		independent.isolate();
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast.create_track("audio", None).unwrap();
		let preferences = moq_net::track::Subscription::default()
			.with_max_delay(std::time::Duration::from_secs(3))
			.with_range(5..=10);
		let first = track.subscribe(preferences.clone());
		let held = source.control(first.control()).unwrap();
		let other = track.subscribe(preferences.clone());
		let _other = independent.control(other.control()).unwrap();

		source.priority(-1).unwrap();
		assert_eq!(first.subscription(), preferences.clone().with_priority(-1));
		assert_eq!(other.subscription(), preferences);
		let late = track.subscribe(source.subscription(preferences.clone()));
		let late_held = source.control(late.control()).unwrap();
		assert_eq!(late.subscription(), first.subscription());
		source.priority(0).unwrap();
		assert_eq!(first.subscription(), preferences);
		assert_eq!(late.subscription(), preferences);
		track.finish().unwrap();
		source.priority(-1).unwrap(); // A closed track has no priority left to change.
		drop((held, late_held));
		assert!(source.priority.lock().unwrap().controls.is_empty());
		drop((first, late, other, _other));
		assert!(track.subscription().is_none());
	}

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
		let dynamic = origin.requests("media").unwrap();
		let binding = source.bind(Some(&Relative::new("media").unwrap()), None).unwrap();
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

		let prepared = request.prepare(moq_net::Epoch::mint()).unwrap();
		let (first, second, producer) = tokio::join!(binding.broadcast(), binding.broadcast(), async {
			prepared.publish().unwrap()
		});
		let first = first.unwrap();
		assert!(!first.is_closed());
		assert!(first.is_clone(&second.unwrap()));
		drop((producer, dynamic));
		first.closed().await;
		assert!(first.is_clone(&binding.broadcast().await.unwrap()));
	}

	/// A binding made before anything announces its sibling resolves on the first announcement
	/// and keeps that instance: past its replacement, and once it goes, answering the ended
	/// instance at once rather than waiting for the next one.
	#[tokio::test(start_paused = true)]
	async fn a_binding_waits_for_the_first_announcement_and_keeps_it() {
		let origin = produce_origin();
		let _catalog = origin.publish("pub", Default::default()).unwrap();
		settle().await;
		let source = resolved(&origin, "pub").await;
		let media = Relative::new("media").unwrap();
		let binding = source.bind(Some(&media), None).unwrap();
		settle().await;
		assert!(binding.poll_broadcast(&kio::Waiter::noop()).is_pending());

		let first_publisher = origin.publish("media", Default::default()).unwrap();
		let first = binding.broadcast().await.unwrap();
		assert!(!first.is_closed());

		let _second = origin.publish("media", Default::default()).unwrap();
		settle().await;
		assert!(binding.broadcast().await.unwrap().is_clone(&first));
		assert!(source.resolve(Some(&media), None).await.unwrap().is_clone(&first));

		drop(first_publisher);
		first.closed().await;
		assert!(binding.broadcast().await.unwrap().is_clone(&first));
		let late = source.bind(Some(&media), None).unwrap();
		assert!(
			late.broadcast().await.unwrap().is_clone(&first),
			"a later binding reads the pin"
		);

		// A new catalog instance starts fresh pins, which resolve the replacement.
		let fresh = source.switched(&source.broadcast());
		assert!(!fresh.resolve(Some(&media), None).await.unwrap().is_closed());
	}

	/// A source serves its path from the broadcast it was built from, even when a replacement
	/// already won the path: a track only the replacement carries is not read from it.
	#[tokio::test(start_paused = true)]
	async fn a_source_keeps_the_broadcast_it_was_built_from() {
		let origin = produce_origin();
		let _old = origin.publish("live", Default::default()).unwrap();
		settle().await;
		let consumer = origin.consume();
		let first = consumer.request("live", None).await.unwrap();

		let new = origin.publish("live", Default::default()).unwrap();
		let _late = new.create_track("late", None).unwrap();
		settle().await;
		let source = Source::new(consumer, first.clone());
		assert!(source.broadcast().is_clone(&first));
		assert!(source.resolve(None, None).await.unwrap().is_clone(&first));
		assert!(
			source.subscribe_track(None, None, "late").await.is_err(),
			"the replacement's track is never read"
		);
	}

	/// A sibling resolves through the session that announces it, even while another lazily
	/// discovering session on the origin never answers its announce request.
	#[tokio::test(start_paused = true)]
	async fn a_stalled_session_does_not_hold_up_a_sibling() {
		use crate::container::test_util::Link;

		let healthy = produce_origin();
		let _catalog = healthy.publish("a/pub", Default::default()).unwrap();
		let _sibling = healthy.publish("a/source", Default::default()).unwrap();
		let stalled = produce_origin();
		let subscriber = produce_origin();
		let _healthy = Link::lazy(&healthy, &subscriber, true).await;
		let _stalled = Link::lazy(&stalled, &subscriber, false).await;

		let timeout = std::time::Duration::from_secs(5);
		let consumer = subscriber.consume();
		let catalog = tokio::time::timeout(timeout, consumer.follow("a/pub", None).unwrap().available())
			.await
			.expect("the catalog resolves")
			.unwrap();
		let source = Source::new(consumer, catalog);
		let sibling = tokio::time::timeout(timeout, source.resolve(Some(&Relative::new("./source").unwrap()), None))
			.await
			.expect("the sibling resolves without the stalled session")
			.unwrap();
		assert!(!sibling.is_closed());

		// Nothing serves a later sibling yet and the stalled session never syncs, so its
		// lookup keeps waiting rather than failing, and resolves once it is published.
		let binding = source.bind(Some(&Relative::new("./late").unwrap()), None).unwrap();
		settle().await;
		assert!(binding.poll_broadcast(&kio::Waiter::noop()).is_pending());
		let _late = healthy.publish("a/late", Default::default()).unwrap();
		let late = tokio::time::timeout(timeout, binding.broadcast())
			.await
			.expect("the later sibling resolves")
			.unwrap();
		assert!(!late.is_closed());
	}

	/// A sibling reference stays on the broadcast it first resolved too.
	#[tokio::test(start_paused = true)]
	async fn a_source_keeps_the_sibling_it_resolved() {
		let origin = produce_origin();
		let _catalog = origin.publish("a/pub", Default::default()).unwrap();
		let _old = origin.publish("a/source", Default::default()).unwrap();
		settle().await;
		let source = resolved(&origin, "a/pub").await;
		let sibling = Relative::new("./source").unwrap();
		let first = source
			.resolve(Some(&sibling), None)
			.await
			.expect("the sibling resolves");

		let new = origin.publish("a/source", Default::default()).unwrap();
		let _late = new.create_track("late", None).unwrap();
		settle().await;
		assert!(source.resolve(Some(&sibling), None).await.unwrap().is_clone(&first));
		assert!(
			source.subscribe_track(Some(&sibling), None, "late").await.is_err(),
			"the replacement's track is never read"
		);
		let bound = source.bind(Some(&sibling), None).unwrap().broadcast().await.unwrap();
		assert!(bound.is_clone(&first), "a binding honors the pin too");
	}

	fn route(epoch: &moq_net::Epoch) -> moq_net::origin::Route {
		moq_net::origin::Route::default().with_epoch(epoch.clone())
	}

	fn is_mismatch(result: crate::Result<moq_net::broadcast::Consumer>, expected: &moq_net::Epoch) -> bool {
		matches!(result, Err(crate::Error::EpochMismatch { expected: ref named, .. }) if named == expected)
	}

	/// An omitted or matching epoch resolves the sibling; another epoch fails instead.
	#[tokio::test(start_paused = true)]
	async fn a_reference_epoch_must_match_the_resolved_sibling() {
		let origin = produce_origin();
		let (catalog, sibling, other) = (moq_net::Epoch::mint(), moq_net::Epoch::mint(), moq_net::Epoch::mint());
		let _catalog = origin.publish("a/pub", route(&catalog)).unwrap();
		let _sibling = origin.publish("a/source", route(&sibling)).unwrap();
		settle().await;
		let source = resolved(&origin, "a/pub").await;
		let rel = Relative::new("./source").unwrap();

		let first = source.resolve(Some(&rel), None).await.unwrap();
		assert_eq!(first.info().epoch.as_ref(), Some(&sibling));
		let matching = source.resolve(Some(&rel), Some(&sibling)).await.unwrap();
		assert!(matching.is_clone(&first));
		assert!(is_mismatch(source.resolve(Some(&rel), Some(&other)).await, &other));
		assert!(
			source.subscribe_track(Some(&rel), Some(&other), "video").await.is_err(),
			"a mismatched reference reads nothing"
		);
	}

	/// A replacement before the first resolution fails a reference naming the old instance,
	/// rather than falling back to the current winner.
	#[tokio::test(start_paused = true)]
	async fn a_replaced_sibling_fails_a_reference_to_the_old_epoch() {
		let origin = produce_origin();
		let (old_epoch, new_epoch) = (moq_net::Epoch::mint(), moq_net::Epoch::mint());
		let _catalog = origin.publish("a/pub", Default::default()).unwrap();
		let _old = origin.publish("a/source", route(&old_epoch)).unwrap();
		let _new = origin.publish("a/source", route(&new_epoch)).unwrap();
		settle().await;
		let source = resolved(&origin, "a/pub").await;
		let rel = Relative::new("./source").unwrap();

		assert!(is_mismatch(
			source.resolve(Some(&rel), Some(&old_epoch)).await,
			&old_epoch
		));
		// The path's first answer is pinned all the same: later references read that instance.
		let pinned = source.resolve(Some(&rel), None).await.unwrap();
		assert_eq!(pinned.info().epoch.as_ref(), Some(&new_epoch));
	}

	/// A later reference naming the replacement cannot move a pinned sibling, and a new
	/// catalog instance starts fresh pins that resolve it.
	#[tokio::test(start_paused = true)]
	async fn a_conflicting_epoch_cannot_replace_a_pin() {
		let origin = produce_origin();
		let (old_epoch, new_epoch) = (moq_net::Epoch::mint(), moq_net::Epoch::mint());
		let _catalog = origin.publish("a/pub", Default::default()).unwrap();
		let _old = origin.publish("a/source", route(&old_epoch)).unwrap();
		settle().await;
		let source = resolved(&origin, "a/pub").await;
		let rel = Relative::new("./source").unwrap();
		let first = source.resolve(Some(&rel), Some(&old_epoch)).await.unwrap();

		let _new = origin.publish("a/source", route(&new_epoch)).unwrap();
		settle().await;
		// A rendition added later names the replacement: it fails, and the pin stays.
		let binding = source.bind(Some(&rel), Some(&new_epoch)).unwrap();
		assert!(is_mismatch(binding.broadcast().await, &new_epoch));
		assert!(source.resolve(Some(&rel), None).await.unwrap().is_clone(&first));
		assert!(
			source
				.resolve(Some(&rel), Some(&old_epoch))
				.await
				.unwrap()
				.is_clone(&first)
		);

		// Concurrent consumers: a new catalog instance resolves the replacement while the old
		// source keeps reading its pin.
		let fresh = source.switched(&source.broadcast());
		let replaced = fresh.resolve(Some(&rel), Some(&new_epoch)).await.unwrap();
		assert_eq!(replaced.info().epoch.as_ref(), Some(&new_epoch));
		assert!(source.resolve(Some(&rel), None).await.unwrap().is_clone(&first));
	}

	/// A self-reference's epoch must not contradict the catalog broadcast's own.
	#[tokio::test(start_paused = true)]
	async fn a_self_reference_epoch_checks_the_catalog_instance() {
		let origin = produce_origin();
		let (catalog, other) = (moq_net::Epoch::mint(), moq_net::Epoch::mint());
		let _catalog = origin.publish("a/pub", route(&catalog)).unwrap();
		settle().await;
		let source = resolved(&origin, "a/pub").await;

		for reference in [None, Some("./pub")] {
			let rel = reference.map(|rel| Relative::new(rel).unwrap());
			let own = source.resolve(rel.as_ref(), Some(&catalog)).await.unwrap();
			assert!(own.is_clone(&source.broadcast()), "{reference:?}");
			assert!(is_mismatch(source.resolve(rel.as_ref(), Some(&other)).await, &other));
			assert!(source.follow(rel.as_ref(), Some(&catalog)).is_ok());
			assert!(matches!(
				source.follow(rel.as_ref(), Some(&other)),
				Err(crate::Error::EpochMismatch { .. })
			));
		}
	}

	/// An epochless route cannot prove its instance, so an explicit epoch is best effort there.
	#[tokio::test(start_paused = true)]
	async fn an_epochless_route_reads_an_epoch_reference_best_effort() {
		let origin = produce_origin();
		let _catalog = origin.publish("a/pub", Default::default()).unwrap();
		let _sibling = origin.publish("a/source", Default::default()).unwrap();
		settle().await;
		let source = resolved(&origin, "a/pub").await;
		let epoch = moq_net::Epoch::mint();

		let sibling = source
			.resolve(Some(&Relative::new("./source").unwrap()), Some(&epoch))
			.await
			.unwrap();
		assert!(sibling.info().epoch.is_none());
		assert!(source.resolve(None, Some(&epoch)).await.is_ok());
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
			let rel = reference.map(|rel| Relative::new(rel).unwrap());
			let bound = source.bind(rel.as_ref(), None).unwrap().broadcast().await.unwrap();
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
			.bind(None, None)
			.expect("no reference is always resolvable")
			.broadcast()
			.await
			.expect("catalog broadcast should resolve");
		let empty = Relative::empty();
		source
			.bind(Some(&empty), None)
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
			.subscribe_track(None, None, "video")
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
		let rel = Relative::new("./pub").unwrap();
		source
			.subscribe_track(Some(&rel), None, "video")
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
			let rel = Relative::new(reference).unwrap();
			assert!(
				source.resolve_reference(Some(&rel)).is_none(),
				"{reference} should escape"
			);
			assert!(source.bind(Some(&rel), None).is_err(), "{reference} should be rejected");

			// Neither `elsewhere` nor the catalog broadcast answers it: the rendition names
			// no broadcast at all.
			match source.resolve(Some(&rel), None).await {
				Err(crate::Error::EscapingBroadcast(_)) => {}
				Err(err) => panic!("{reference} failed with the wrong error: {err:?}"),
				Ok(_) => panic!("{reference} should not resolve to any broadcast"),
			}
			match source.subscribe_track(Some(&rel), None, "video").await {
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
		escaped.broadcast = Some(Relative::new("../../source").unwrap());
		let mut sibling = escaped.clone();
		sibling.broadcast = Some(Relative::new("./source").unwrap());

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
		escaped.broadcast = Some(Relative::new("../../source").unwrap());
		let mut sibling = escaped.clone();
		sibling.broadcast = Some(Relative::new("./source").unwrap());

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
		let rel = Relative::new("./source").unwrap();
		source
			.subscribe_track(Some(&rel), None, "video")
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
		let rel = Relative::new(".").unwrap();
		source
			.subscribe_track(Some(&rel), None, "video")
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
		let rel = Relative::new(".").unwrap();
		source
			.subscribe_track(Some(&rel), None, "video")
			.await
			.expect("dot should resolve to the empty root broadcast");
	}
}
