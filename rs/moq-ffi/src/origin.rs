//! Publish, discover, and route broadcasts.

use std::sync::Arc;

use crate::consumer::MoqBroadcastConsumer;
use crate::error::MoqError;
use crate::ffi::{Background, Task};
use crate::producer::MoqBroadcastProducer;

/// Config used when creating an origin.
#[derive(Clone, Default, uniffi::Record)]
pub struct MoqOriginConfig {
	/// Maximum cached group bytes across broadcasts under this origin. Null is unbounded.
	#[uniffi(default = None)]
	pub cache_capacity_bytes: Option<u64>,
}

/// Scope for an announcement stream.
#[derive(Clone, Debug, Default, uniffi::Record)]
pub struct MoqAnnounceConfig {
	/// Literal path prefix beneath the origin.
	#[uniffi(default = "")]
	pub prefix: String,
	/// Pattern relative to `prefix`, or `None` for every path beneath it.
	#[uniffi(default = None)]
	pub filter: Option<String>,
	/// Also list hidden paths: those with a segment starting with `.` below the prefix.
	#[uniffi(default = false)]
	pub hidden: bool,
}

/// How a broadcast advertises its exact path: a static cost and the publisher epoch.
///
/// Pass one to `MoqPublication::announce`. The hop chain is the origin's to build,
/// so a local publisher cannot claim relay hops it never took.
#[derive(Clone, Debug, Default, PartialEq, Eq, uniffi::Record)]
pub struct MoqAnnounceRoute {
	/// Preference among routes covering the same prefix: lower wins. A publisher
	/// sets its production cost here: zero for content it is already producing,
	/// larger for content it would have to start producing on demand.
	#[uniffi(default = 0)]
	pub cost: u64,
	/// The publisher instance the route serves, as UUIDv7 text, or `None` when unknown.
	///
	/// A publisher that can restart sets a fresh one per run from `moq_mint_epoch`, so
	/// viewers see a restart instead of a stalled broadcast. Routes with the same epoch
	/// serve the same bytes, so redundant publishers share one. Among routes at a
	/// prefix the newest epoch wins, and changing it is a `MoqAnnounceEvent::Restart`.
	#[uniffi(default = None)]
	pub epoch: Option<String>,
}

impl TryFrom<MoqAnnounceRoute> for moq_net::origin::Route {
	type Error = MoqError;

	fn try_from(route: MoqAnnounceRoute) -> Result<Self, MoqError> {
		let mut out = moq_net::origin::Route::default().with_cost(moq_net::origin::Cost::new(route.cost));
		if let Some(epoch) = route.epoch {
			out = out.with_epoch(epoch.parse()?);
		}
		Ok(out)
	}
}

/// An observed path-prefix route: hops, a static cost, and the publisher epoch.
///
/// Carried by `MoqAnnounceConsumer` events. A route claims capability, not
/// inventory: by convention a publisher announces each broadcast's exact path,
/// so subscribers can enumerate broadcasts, while a service advertises a prefix
/// with `MoqOriginProducer::requests` and answers whatever is requested beneath it.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MoqRoute {
	/// Hop ids of the relay hops the route traversed, oldest first. 0 is the
	/// anonymous mark.
	pub hops: Vec<u64>,
	/// Preference among routes covering the same prefix: lower wins.
	pub cost: u64,
	/// Whether the chain holds a 0 anywhere. An anonymous route ranks below every
	/// fully identified one, whatever the costs say.
	pub anonymous: bool,
	/// The publisher instance the route serves, as UUIDv7 text, or `None` when unknown.
	pub epoch: Option<String>,
}

impl From<moq_net::origin::Route> for MoqRoute {
	fn from(route: moq_net::origin::Route) -> Self {
		Self {
			hops: route.hops.iter().map(|origin| origin.id()).collect(),
			cost: route.cost.value(),
			anonymous: route.is_anonymous(),
			epoch: route.epoch.map(|epoch| epoch.to_string()),
		}
	}
}

/// Mint a fresh publisher epoch from the wall clock and secure randomness, ordered newest last.
#[uniffi::export]
pub fn moq_mint_epoch() -> String {
	moq_net::Epoch::mint().to_string()
}

/// An origin in which broadcasts can be published.
#[derive(uniffi::Object)]
pub struct MoqOriginProducer {
	inner: Origin,
}

#[derive(Clone)]
pub(crate) struct Origin {
	pub inner: moq_net::origin::Producer,
	pub driver: Background,
}

impl std::ops::Deref for Origin {
	type Target = moq_net::origin::Producer;
	fn deref(&self) -> &Self::Target {
		&self.inner
	}
}

/// An origin from which broadcasts can be discovered and requested.
#[derive(uniffi::Object)]
pub struct MoqOriginConsumer {
	inner: moq_net::origin::Consumer,
	pub(crate) driver: Background,
}

/// A stream of broadcast announcement changes.
#[derive(uniffi::Object)]
pub struct MoqAnnounceConsumer {
	task: Task<Announced>,
}

#[derive(uniffi::Object)]
/// A served route: advertises a path prefix and yields the broadcast requests
/// beneath it for the application to accept or reject.
///
/// Keeps its origin running, like a published broadcast, after every
/// `MoqOriginProducer` is gone.
pub struct MoqOriginRequests {
	slot: Slot,
	task: std::sync::Mutex<Option<Arc<Task<OriginRequests>>>>,
}

#[derive(uniffi::Object)]
/// A pending dynamic broadcast request that must be accepted or rejected.
pub struct MoqBroadcastRequest {
	inner: std::sync::Mutex<Option<moq_net::origin::Request>>,
	driver: Background,
}

#[derive(uniffi::Object)]
/// A broadcast published on an origin: the broadcast to fill and its announcement.
///
/// Dropping it does not retract the route; closing the broadcast does.
pub struct MoqPublication {
	pub(crate) inner: moq_net::origin::Publication,
	broadcast: Arc<MoqBroadcastProducer>,
}

#[derive(uniffi::Object)]
/// A claim answer being filled before its exact route is announced.
pub struct MoqBroadcastPrepared {
	inner: std::sync::Mutex<Option<moq_net::origin::Prepared>>,
	broadcast: Arc<MoqBroadcastProducer>,
	driver: Background,
}

struct Announced {
	inner: moq_net::announce::Consumer,
	driver: Background,
}

/// The served route behind one `MoqOriginProducer::requests` handle, shared
/// between the handle and the task draining it. Emptied by the handle's cancel,
/// which retracts the route: the task sees the empty slot and ends.
type Slot = Arc<std::sync::Mutex<Option<moq_net::origin::Requests>>>;

struct OriginRequests {
	slot: Slot,
	driver: Background,
}

impl OriginRequests {
	async fn requested_broadcast(&mut self) -> Result<Arc<MoqBroadcastRequest>, MoqError> {
		let request = self
			.driver
			.watch(async {
				kio::wait(|waiter| match self.slot.lock().unwrap().as_ref() {
					Some(dynamic) => dynamic.poll_requested_broadcast(waiter),
					None => std::task::Poll::Ready(Err(moq_net::Error::Closed)),
				})
				.await
				.map_err(|err| match err {
					moq_net::Error::Closed => MoqError::Closed,
					other => other.into(),
				})
			})
			.await?;
		Ok(Arc::new(MoqBroadcastRequest::new(request, self.driver.clone())))
	}
}

impl Announced {
	async fn next(&mut self) -> Result<Option<MoqAnnounceEvent>, MoqError> {
		self.driver
			.watch(async { Ok(self.inner.next().await.map(Into::into)) })
			.await
	}
}

/// Follows one exact path, resolving each publisher instance that serves it.
struct Following {
	inner: moq_net::origin::Following,
	origin: moq_net::origin::Consumer,
	driver: Background,
}

impl Following {
	async fn available(&mut self) -> Result<Arc<MoqBroadcastConsumer>, MoqError> {
		let broadcast = self.driver.watch(async { Ok(self.inner.available().await?) }).await?;
		Ok(Arc::new(MoqBroadcastConsumer::routed(
			broadcast,
			self.origin.clone(),
			self.driver.clone(),
		)))
	}
}

/// A route over a prefix, carried by a [`MoqAnnounceEvent`].
///
/// Carries no broadcast: resolve a specific path with
/// `MoqOriginConsumer::request` (after an event proves it is
/// covered). The application decides which paths name broadcasts.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MoqAnnounce {
	/// The covered prefix, relative to the origin.
	pub prefix: String,
	/// What each wildcard matched. Only full matches are listed, so it is always set.
	pub captures: Option<Vec<String>>,
	/// The route serving the prefix: its epoch, hops, and a static cost.
	pub route: MoqRoute,
}

impl From<moq_net::announce::Announce> for MoqAnnounce {
	fn from(announce: moq_net::announce::Announce) -> Self {
		Self {
			prefix: announce.prefix.to_string(),
			captures: announce
				.captures
				.map(|captures| captures.into_iter().map(|capture| capture.to_string()).collect()),
			route: announce.route.into(),
		}
	}
}

/// What a [`MoqAnnounceConsumer`] yields.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MoqAnnounceEvent {
	/// A route now covers the prefix; the stream had none there.
	Start {
		/// The broadcast announcement affected by this event.
		announce: MoqAnnounce,
	},
	/// The route covering the prefix changed hops or cost.
	Update {
		/// The broadcast announcement affected by this event.
		announce: MoqAnnounce,
	},
	/// No route covers the prefix any more. Carries its last advertised route.
	End {
		/// The broadcast announcement affected by this event.
		announce: MoqAnnounce,
	},
	/// Another publisher instance now serves the prefix: drop what was resolved under
	/// it and request afresh.
	Restart {
		/// The broadcast announcement affected by this event.
		announce: MoqAnnounce,
	},
}

impl From<moq_net::announce::Event> for MoqAnnounceEvent {
	fn from(event: moq_net::announce::Event) -> Self {
		use moq_net::announce::Event;
		match event {
			Event::Start(announce) => Self::Start {
				announce: announce.into(),
			},
			Event::Update(announce) => Self::Update {
				announce: announce.into(),
			},
			Event::End(announce) => Self::End {
				announce: announce.into(),
			},
			Event::Restart(announce) => Self::Restart {
				announce: announce.into(),
			},
		}
	}
}

/// Follows a specific broadcast across the publisher instances that serve it.
#[derive(uniffi::Object)]
pub struct MoqFollowing {
	task: Task<Following>,
}

impl MoqOriginProducer {
	pub(crate) fn inner(&self) -> &Origin {
		&self.inner
	}

	/// Wrap an owned origin so it can cross the FFI boundary.
	pub(crate) fn from_inner(inner: Origin) -> Self {
		Self { inner }
	}

	fn from_config(config: MoqOriginConfig) -> Self {
		let mut origin = moq_net::origin::Config::default();
		if let Some(capacity) = config.cache_capacity_bytes {
			let cache = moq_net::cache::Config::default()
				.with_capacity(capacity)
				.with_expiry(origin.pool.expiry());
			origin.pool = moq_net::cache::Pool::new(cache);
		}

		Self { inner: spawn(origin) }
	}
}

/// Build an origin whose host handles retain its driver's completion.
pub(crate) fn spawn(config: moq_net::origin::Config) -> Origin {
	let (inner, driver) = moq_net::origin::Producer::new(config);
	#[cfg(not(target_arch = "wasm32"))]
	let driver = Background::driver(driver);
	#[cfg(target_arch = "wasm32")]
	let driver = crate::ffi::spawn(async move {
		moq_net::time::run(driver).await;
	});
	Origin { inner, driver }
}

impl MoqOriginConsumer {
	pub(crate) fn from_inner(origin: Origin) -> Self {
		Self {
			inner: origin.inner.consume(),
			driver: origin.driver,
		}
	}
}

/// Resolve the (publish, subscribe) origin pair backing a session.
///
/// With neither side wired, both sides share ONE origin, so a broadcast announced on a session
/// is discoverable through that same session's consumer. Wiring either side opts out of the
/// loopback and gives the other side a fresh origin, keeping the two directions isolated.
pub(crate) fn resolve_pair(
	publish: Option<&Arc<MoqOriginProducer>>,
	consume: Option<&Arc<MoqOriginProducer>>,
) -> (Origin, Origin) {
	if publish.is_none() && consume.is_none() {
		// Clones of a Producer share the underlying origin, so this is one origin, not two.
		let shared = spawn(moq_net::origin::Config::default());
		return (shared.clone(), shared);
	}

	let resolve = |origin: Option<&Arc<MoqOriginProducer>>| {
		origin
			.map(|o| o.inner().clone())
			.unwrap_or_else(|| spawn(moq_net::origin::Config::default()))
	};
	(resolve(publish), resolve(consume))
}

#[uniffi::export]
impl MoqOriginProducer {
	/// Create a new origin for publishing and/or consuming broadcasts.
	#[uniffi::constructor]
	pub fn new(config: MoqOriginConfig) -> Arc<Self> {
		let _guard = crate::ffi::enter();
		Arc::new(Self::from_config(config))
	}

	/// Create a consumer for this origin.
	pub fn consume(&self) -> Arc<MoqOriginConsumer> {
		let _guard = crate::ffi::enter();
		Arc::new(MoqOriginConsumer::from_inner(self.inner.clone()))
	}

	/// Advertise `prefix` and serve the requests beneath it.
	///
	/// A route claims `prefix` and every path beneath it (the empty prefix
	/// claims every path). A service that only serves some of them advertises
	/// the covering prefix and rejects the rest as they are requested. Hold
	/// the returned handle while the route should stay advertised and missing
	/// broadcasts should be served.
	pub fn requests(&self, prefix: String) -> Result<Arc<MoqOriginRequests>, MoqError> {
		let _guard = crate::ffi::enter();
		self.inner.driver.check()?;
		let result = self.inner.requests(prefix);
		self.inner.driver.check()?;
		let dynamic = result?;
		let slot = Arc::new(std::sync::Mutex::new(Some(dynamic)));
		Ok(Arc::new(MoqOriginRequests {
			slot: slot.clone(),
			task: std::sync::Mutex::new(Some(Arc::new(Task::new(OriginRequests {
				slot,
				driver: self.inner.driver.clone(),
			})))),
		}))
	}

	/// Create a broadcast at `path` on this origin, returning its publication.
	///
	/// The broadcast exists for nobody, on this origin or its peers, until
	/// [`MoqPublication::announce`]: until then announcement streams skip it
	/// and requests for its path are unroutable. Announce after populating
	/// tracks; an on-demand handler is [`MoqBroadcastProducer::requests`]. Create,
	/// `requests()` if tracks are served on demand, populate, then announce.
	///
	/// [`MoqBroadcastProducer::close`] ends it for good; dropping its last handle,
	/// the publication and `requests()` included, does the same.
	pub fn create_broadcast(&self, path: String) -> Result<Arc<MoqPublication>, MoqError> {
		let _guard = crate::ffi::enter();
		self.inner.driver.check()?;
		// Surfaces Error::Unauthorized (out of scope) via the MoqError::Protocol conversion.
		let result = self.inner.create_broadcast(path.as_str());
		self.inner.driver.check()?;
		let publication = result?;
		let broadcast = Arc::new(MoqBroadcastProducer::from_inner(
			publication.broadcast().clone(),
			self.inner.driver.clone(),
		)?);
		Ok(Arc::new(MoqPublication {
			inner: publication,
			broadcast,
		}))
	}
}

#[uniffi::export]
impl MoqOriginConsumer {
	/// Subscribe to routes whose names match a pattern scope; updates stay relative to the origin.
	pub fn announced(&self, config: MoqAnnounceConfig) -> Result<Arc<MoqAnnounceConsumer>, MoqError> {
		let _guard = crate::ffi::enter();
		self.driver.check()?;
		let filter = match config.filter {
			Some(filter) => filter.parse::<moq_net::Pattern>()?,
			None => moq_net::Pattern::all(),
		};
		let filter = filter.rooted(&config.prefix)?;
		let origin = self
			.inner
			.scope("", &moq_net::Patterns::from(filter))?
			.with_hidden(config.hidden);
		Ok(Arc::new(MoqAnnounceConsumer {
			task: Task::new(Announced {
				inner: origin.announced(),
				driver: self.driver.clone(),
			}),
		}))
	}

	/// Follow the broadcast at `path` across the publisher instances that serve it.
	///
	/// Unlike `request`, this waits for future announcements and keeps following publisher
	/// instances. A local broadcast resolves once announced, like a remote one. Pass an
	/// `epoch` (UUIDv7 text) to follow only that instance: another one taking over is never
	/// handed out, and the instance is handed out again if it returns after an end.
	#[uniffi::method(default(epoch = None))]
	pub fn follow(&self, path: String, epoch: Option<String>) -> Result<Arc<MoqFollowing>, MoqError> {
		let _guard = crate::ffi::enter();
		self.driver.check()?;
		let epoch: Option<moq_net::Epoch> = epoch.as_deref().map(str::parse).transpose()?;
		// Following checks the permission eagerly, so an unreachable path fails here rather
		// than surfacing later as a `Closed` the caller can't tell from the origin ending.
		let inner = self.inner.follow(path.as_str(), epoch)?;

		Ok(Arc::new(MoqFollowing {
			task: Task::new(Following {
				inner,
				origin: self.inner.clone(),
				driver: self.driver.clone(),
			}),
		}))
	}

	/// Request the broadcast at `path`, resolving one publisher instance as soon as it can be served.
	///
	/// Resolves through the best announced route covering the path: an announced broadcast
	/// on this origin, a route a session announced (served on demand by that session), or a
	/// dynamic handler on the origin. The most specific prefix wins, then the cheapest. An
	/// unannounced broadcast is unroutable once discovery has synced without a covering route.
	/// The request owns discovery of its path and retries route churn until then; no separate
	/// announcement stream is needed. Pass an `epoch` (UUIDv7 text) to refuse any other
	/// publisher instance. Drop the returned future to cancel.
	#[uniffi::method(default(epoch = None))]
	pub async fn request(&self, path: String, epoch: Option<String>) -> Result<Arc<MoqBroadcastConsumer>, MoqError> {
		let epoch: Option<moq_net::Epoch> = epoch.as_deref().map(str::parse).transpose()?;
		let broadcast = self
			.driver
			.watch(async { Ok(self.inner.request(path.as_str(), epoch).await?) })
			.await?;
		Ok(Arc::new(MoqBroadcastConsumer::routed(
			broadcast,
			self.inner.clone(),
			self.driver.clone(),
		)))
	}
}

// ---- MoqOriginRequests ----

#[uniffi::export]
impl MoqOriginRequests {
	/// Wait for the next broadcast requested through this handle's prefix.
	///
	/// Returns a [`MoqBroadcastRequest`]: accept it with a broadcast producer or reject
	/// it with an application error code. The requesting consumer stays pending until then.
	pub async fn requested_broadcast(&self) -> Result<Arc<MoqBroadcastRequest>, MoqError> {
		let task = self.task.lock().unwrap().clone().ok_or(MoqError::Closed)?;
		task.run(|mut state| async move { state.requested_broadcast().await })
			.await
	}

	/// Stop serving and retract the route. Terminal: this handler is released
	/// here, not when the handle is, so pending requests are rejected before
	/// this returns.
	pub fn cancel(&self) {
		let _guard = crate::ffi::enter();
		let dynamic = self.slot.lock().unwrap().take();
		drop(dynamic);
		if let Some(task) = self.task.lock().unwrap().take() {
			task.cancel();
		}
	}
}

// ---- MoqBroadcastRequest ----

impl MoqBroadcastRequest {
	fn new(request: moq_net::origin::Request, driver: Background) -> Self {
		Self {
			inner: std::sync::Mutex::new(Some(request)),
			driver,
		}
	}

	fn take(&self) -> Result<moq_net::origin::Request, MoqError> {
		self.inner.lock().unwrap().take().ok_or(MoqError::Closed)
	}
}

#[uniffi::export]
impl MoqBroadcastRequest {
	/// The requested broadcast path.
	pub fn path(&self) -> Result<String, MoqError> {
		let guard = self.inner.lock().unwrap();
		let request = guard.as_ref().ok_or(MoqError::Closed)?;
		Ok(request.path().to_string())
	}

	/// Start answering as a new publisher instance, hidden until published.
	pub fn prepare(&self, epoch: String) -> Result<Arc<MoqBroadcastPrepared>, MoqError> {
		let _guard = crate::ffi::enter();
		self.driver.check()?;
		let request = self.take()?;
		let result = request.prepare(epoch.parse()?);
		self.driver.check()?;
		let prepared = result?;
		let broadcast = Arc::new(MoqBroadcastProducer::from_inner(
			prepared.broadcast().clone(),
			self.driver.clone(),
		)?);
		Ok(Arc::new(MoqBroadcastPrepared {
			inner: std::sync::Mutex::new(Some(prepared)),
			broadcast,
			driver: self.driver.clone(),
		}))
	}

	/// Reject the request with an application error code.
	pub fn reject(&self, error_code: u16) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let request = self.take()?;
		request.reject(moq_net::Error::App(error_code));
		Ok(())
	}
}

#[uniffi::export]
impl MoqBroadcastPrepared {
	/// The unannounced broadcast to fill before publishing; only `publish` announces it.
	pub fn broadcast(&self) -> Arc<MoqBroadcastProducer> {
		self.broadcast.clone()
	}

	/// Announce the filled broadcast and resolve every waiting requester.
	pub fn publish(&self) -> Result<Arc<MoqPublication>, MoqError> {
		let _guard = crate::ffi::enter();
		self.driver.check()?;
		let prepared = self.inner.lock().unwrap().take().ok_or(MoqError::Closed)?;
		let result = prepared.publish();
		self.driver.check()?;
		Ok(Arc::new(MoqPublication {
			inner: result?,
			broadcast: self.broadcast.clone(),
		}))
	}
}

#[uniffi::export]
impl MoqPublication {
	/// The broadcast to fill, without announcement control.
	pub fn broadcast(&self) -> Arc<MoqBroadcastProducer> {
		self.broadcast.clone()
	}

	/// Advertise the broadcast's exact path as a route, or re-price it in place.
	///
	/// Until announced, the broadcast is invisible and unroutable for local
	/// consumers and peers alike. Errors with `Closed` once the broadcast closed.
	pub fn announce(&self, route: MoqAnnounceRoute) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let route: moq_net::origin::Route = route.try_into()?;
		self.broadcast.with_state(|_| Ok(self.inner.announce(route)?))
	}

	/// Retract the advertisement, if any, keeping the filled tracks alive.
	///
	/// Local consumers and peers alike stop discovering and requesting it;
	/// tracks already in flight carry on. Announcing again brings it back. Errors with
	/// `Closed` once the broadcast closed.
	pub fn unannounce(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		self.broadcast.with_state(|_| {
			self.inner.unannounce();
			Ok(())
		})
	}
}

// ---- MoqAnnounceConsumer ----

#[uniffi::export]
impl MoqAnnounceConsumer {
	/// Get the next announce event. Returns `None` when the origin is closed.
	pub async fn next(&self) -> Result<Option<MoqAnnounceEvent>, MoqError> {
		self.task.run(|mut state| async move { state.next().await }).await
	}

	/// Cancel all current and future `next()` calls.
	///
	/// Terminal: the announcement stream is released here, not when the handle is.
	pub fn cancel(&self) {
		self.task.cancel();
	}
}

// ---- MoqFollowing ----

#[uniffi::export]
impl MoqFollowing {
	/// Wait for the next publisher instance serving the broadcast. Returns `Closed` if cancelled
	/// or the origin is closed.
	///
	/// The first call resolves the instance serving the path, once one is announced. Each later
	/// call waits for another to take over, as a restarted publisher or a replacement session
	/// does, or for the broadcast to return after it went offline. Drop everything read from the
	/// previous instance then and read the new one: the old one's tracks end when its route
	/// dies, with an error unless this side closed that session.
	pub async fn available(&self) -> Result<Arc<MoqBroadcastConsumer>, MoqError> {
		self.task.run(|mut state| async move { state.available().await }).await
	}

	/// Cancel all current and future `available()` calls.
	///
	/// Terminal: the announcement watch is released here, not when the handle is.
	pub fn cancel(&self) {
		self.task.cancel();
	}
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod supervision_tests {
	use super::*;

	#[tokio::test]
	async fn a_panicked_origin_task_reaches_the_host_while_another_origin_keeps_working() {
		let (inner, driver, trigger) = crate::ffi::panicking_origin();
		let failed = MoqOriginProducer::from_inner(Origin { inner, driver });
		let failed = failed.consume().announced(Default::default()).unwrap();
		*trigger
			.write()
			.unwrap_or_else(|_| unreachable!("test keeps its panic trigger open")) = true;
		assert!(matches!(failed.next().await, Err(MoqError::Task { .. })));

		let healthy = MoqOriginProducer::new(Default::default());
		let publication = healthy.create_broadcast("healthy".into()).unwrap();
		publication.announce(Default::default()).unwrap();
		healthy.consume().request("healthy".into(), None).await.unwrap();
	}

	#[tokio::test]
	async fn a_broadcast_keeps_the_origin_failure_after_its_origin_handle_is_released() {
		let (inner, completion, trigger) = crate::ffi::panicking_origin();
		let origin = MoqOriginProducer::from_inner(Origin {
			inner,
			driver: completion.clone(),
		});
		let publication = origin.create_broadcast("retained".into()).unwrap();
		let dynamic = publication.broadcast().requests().unwrap();
		drop(origin);
		*trigger
			.write()
			.unwrap_or_else(|_| unreachable!("test keeps its panic trigger open")) = true;
		assert!(matches!(completion.finished().await, Err(MoqError::Task { .. })));
		drop(completion);
		assert!(matches!(
			publication.announce(Default::default()),
			Err(MoqError::Task { .. })
		));
		assert!(matches!(dynamic.requested_track().await, Err(MoqError::Task { .. })));
		let healthy = MoqOriginProducer::new(Default::default());
		healthy
			.create_broadcast("healthy".into())
			.unwrap()
			.announce(Default::default())
			.unwrap();
	}

	#[tokio::test]
	async fn a_finish_after_a_driver_panic_reports_it_until_the_close() {
		let (inner, completion, trigger) = crate::ffi::panicking_origin();
		let origin = MoqOriginProducer::from_inner(Origin {
			inner,
			driver: completion.clone(),
		});
		let broadcast = origin.create_broadcast("retained".into()).unwrap().broadcast();
		// A dynamic handle outlives the consumed producer, so only an explicit close ends it.
		let _dynamic = broadcast.requests().unwrap();
		let consumer = broadcast.state.lock().unwrap().as_ref().unwrap().broadcast.consume();
		let catalog = crate::media::MoqMediaCatalogProducer::new(broadcast.clone()).unwrap();
		*trigger
			.write()
			.unwrap_or_else(|_| unreachable!("test keeps its panic trigger open")) = true;
		assert!(matches!(completion.finished().await, Err(MoqError::Task { .. })));
		assert!(matches!(catalog.finish(), Err(MoqError::Task { .. })));
		// The handle stays the caller's to close, and the close still ends the broadcast.
		assert!(!consumer.is_closed());
		assert!(matches!(broadcast.close(), Err(MoqError::Task { .. })));
		assert!(consumer.is_closed());
	}

	#[tokio::test]
	async fn container_importers_keep_the_origin_failure_after_other_owners_are_released() {
		use crate::media::{MoqContainerFormat, MoqContainerInit, MoqMediaContainerProducer, MoqMediaTimebase};
		let (inner, completion, trigger) = crate::ffi::panicking_origin();
		let origin = MoqOriginProducer::from_inner(Origin {
			inner,
			driver: completion.clone(),
		});
		let broadcast = origin.create_broadcast("retained".into()).unwrap().broadcast();
		// One importer shares a timebase and one doesn't, so both reservation paths carry the driver.
		let timebase = MoqMediaTimebase::new(broadcast.clone()).unwrap();
		let container = MoqMediaContainerProducer::new(
			&broadcast,
			MoqContainerInit {
				format: MoqContainerFormat::Ts,
				data: Vec::new(),
				timebase: Some(timebase.clone()),
			},
		)
		.unwrap();
		let independent = MoqMediaContainerProducer::new(
			&broadcast,
			MoqContainerInit {
				format: MoqContainerFormat::Ts,
				data: Vec::new(),
				timebase: None,
			},
		)
		.unwrap();
		drop(timebase);
		drop(broadcast);
		drop(origin);
		*trigger
			.write()
			.unwrap_or_else(|_| unreachable!("test keeps its panic trigger open")) = true;
		assert!(matches!(completion.finished().await, Err(MoqError::Task { .. })));
		drop(completion);
		assert!(matches!(container.cut(), Err(MoqError::Task { .. })));
		assert!(matches!(independent.write(Vec::new()), Err(MoqError::Task { .. })));
		assert!(matches!(container.finish(), Err(MoqError::Task { .. })));
		assert!(matches!(independent.finish(), Err(MoqError::Task { .. })));
	}

	#[tokio::test]
	async fn readers_derived_from_a_routed_broadcast_report_the_origin_panic() {
		let (inner, completion, trigger) = crate::ffi::panicking_origin();
		let origin = MoqOriginProducer::from_inner(Origin {
			inner,
			driver: completion.clone(),
		});
		let publication = origin.create_broadcast("routed".into()).unwrap();
		publication.announce(Default::default()).unwrap();
		let broadcast = publication.broadcast();
		let track = broadcast.publish_track("data".into(), None).unwrap();
		let open = track.append_group().unwrap();
		let consumer = origin.consume().request("routed".into(), None).await.unwrap();
		let subscriber = consumer.subscribe_track("data".into(), None).await.unwrap();
		let group = subscriber.recv_group().await.unwrap().unwrap();
		let catalog = crate::media::MoqMediaCatalogConsumer::subscribe(&consumer)
			.await
			.unwrap();
		drop(consumer);
		drop(origin);
		*trigger
			.write()
			.unwrap_or_else(|_| unreachable!("test keeps its panic trigger open")) = true;
		assert!(matches!(completion.finished().await, Err(MoqError::Task { .. })));
		drop(completion);
		// Teardown drops the producers too, which the readers would otherwise report as `dropped`.
		drop(open);
		drop(track);
		drop(broadcast);
		drop(publication);
		let result = subscriber.recv_group().await.map(|group| group.is_some());
		assert!(matches!(result, Err(MoqError::Task { .. })), "{result:?}");
		let result = group.read_frame().await.map(|frame| frame.is_some());
		assert!(matches!(result, Err(MoqError::Task { .. })), "{result:?}");
		let result = catalog.next().await.map(|catalog| catalog.is_some());
		assert!(matches!(result, Err(MoqError::Task { .. })), "{result:?}");
	}

	#[tokio::test]
	async fn a_driver_panic_is_recorded_before_teardown_wakes_the_host() {
		struct Driver {
			dropped: Option<tokio::sync::oneshot::Sender<()>>,
			ack: std::sync::mpsc::Receiver<()>,
		}
		impl moq_net::time::Driver for Driver {
			fn poll(
				&mut self,
				_: moq_net::time::Instant,
				_: &kio::Waiter,
			) -> Result<Option<moq_net::time::Instant>, moq_net::Error> {
				panic!("injected driver panic");
			}
		}
		impl Drop for Driver {
			fn drop(&mut self) {
				let _ = self.dropped.take().unwrap().send(());
				self.ack.recv().unwrap();
			}
		}
		let (dropped, teardown) = tokio::sync::oneshot::channel();
		let (ack, released) = std::sync::mpsc::channel();
		let driver = Background::driver(Driver {
			dropped: Some(dropped),
			ack: released,
		});
		let result: Result<(), MoqError> = driver
			.watch(async {
				let _ = teardown.await;
				Err(MoqError::Cancelled)
			})
			.await;
		ack.send(()).unwrap();
		assert!(matches!(driver.finished().await, Err(MoqError::Task { .. })));
		assert!(matches!(result, Err(MoqError::Task { .. })), "{result:?}");
	}
}
