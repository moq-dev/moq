//! Publish, discover, and route broadcasts.

use std::sync::Arc;
use std::task::{Poll, ready};

use crate::consumer::MoqBroadcastConsumer;
use crate::error::MoqError;
use crate::ffi::{Background, Polled};
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

/// How a route advertises itself: its visibility at creation, its price, and, rarely, a
/// stable instance.
///
/// The default announces at once, at cost 0, serving a fresh instance minted when the
/// route is created. The hop chain is the origin's to build, so a local publisher cannot
/// claim relay hops it never took.
#[derive(Clone, Debug, Default, PartialEq, Eq, uniffi::Record)]
pub struct MoqRouteOptions {
	/// Create the route hidden: it exists for nobody until [`MoqRoute::announce`]. For a
	/// publisher that fills its catalog first.
	#[uniffi(default = false)]
	pub hidden: bool,
	/// Preference among routes covering the same prefix: lower wins. A publisher
	/// sets its production cost here: zero for content it is already producing,
	/// larger for content it would have to start producing on demand.
	#[uniffi(default = 0)]
	pub cost: u64,
	/// A stable instance for the stream, or `None` to mint a fresh one.
	///
	/// Set one only for a publisher that continues its group sequences across restarts and
	/// raises `epoch` on each: readers stitch every route of one instance together, so
	/// restarting the sequences under an old instance splices unrelated groups into their
	/// tracks. Naming an instance grants nothing: publishing still needs the path's grant.
	#[uniffi(default = None)]
	pub instance: Option<String>,
	/// Which run of a stable `instance` this is: only its highest epoch is eligible, cheaper
	/// or not. Ignored without one.
	#[uniffi(default = 0)]
	pub epoch: u64,
}

impl TryFrom<MoqRouteOptions> for moq_net::origin::RouteOptions {
	type Error = MoqError;

	fn try_from(options: MoqRouteOptions) -> Result<Self, MoqError> {
		let mut out = match options.hidden {
			true => moq_net::origin::RouteOptions::hidden(),
			false => moq_net::origin::RouteOptions::default(),
		}
		.with_cost(options.cost);
		if let Some(instance) = options.instance {
			out = out.with_instance(instance.parse()?, options.epoch);
		}
		Ok(out)
	}
}

/// The read-only facts on a received route: hops, a static cost, and the stream's instance.
///
/// Carried by `MoqAnnounceConsumer` events. A route claims capability, not
/// inventory: by convention a publisher creates each broadcast's exact route,
/// so subscribers can enumerate broadcasts, while a service creates a prefix route
/// with `MoqOriginProducer::route_prefix` and answers whatever is requested beneath it.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MoqRouteInfo {
	/// Hop ids of the relay hops the route traversed, oldest first. 0 is the
	/// anonymous mark.
	pub hops: Vec<u64>,
	/// Preference among routes covering the same prefix: lower wins.
	pub cost: u64,
	/// Whether the chain holds a 0 anywhere. An anonymous route ranks below every
	/// fully identified one, whatever the costs say.
	pub anonymous: bool,
	/// The stream the route serves: routes with one instance are one continuous stream.
	pub instance: String,
}

impl From<moq_net::origin::RouteInfo> for MoqRouteInfo {
	fn from(route: moq_net::origin::RouteInfo) -> Self {
		Self {
			hops: route.hops().iter().map(|origin| origin.id()).collect(),
			cost: route.cost().value(),
			anonymous: route.is_anonymous(),
			instance: route.instance().to_string(),
		}
	}
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
	reads: Polled<moq_net::announce::Consumer>,
}

/// A route on an origin: an exact route owning one broadcast, or a prefix route yielding
/// the requests beneath it.
///
/// Dropping or cancelling it retracts the route, as does closing an exact route's
/// broadcast; a broadcast handle alone does not keep it announced. It keeps its
/// origin running after every `MoqOriginProducer` is gone.
#[derive(uniffi::Object)]
pub struct MoqRoute {
	/// The route itself, released on cancel.
	reads: Polled<moq_net::origin::Route>,
	/// An exact route's broadcast, wrapped once so every caller shares its catalog.
	broadcast: Option<Arc<MoqBroadcastProducer>>,
	/// Minted at creation and kept for the route's lifetime.
	instance: String,
	driver: Background,
}

#[derive(uniffi::Object)]
/// A pending broadcast request on a prefix route, answered by serving, routing, or rejecting it.
pub struct MoqBroadcastRequest {
	inner: std::sync::Mutex<Option<moq_net::origin::Request>>,
	driver: Background,
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
	/// What each wildcard matched, empty when the filter has no wildcards.
	pub captures: Vec<String>,
	/// The route serving the prefix: its instance, hops, and a static cost.
	pub route: MoqRouteInfo,
}

impl From<moq_net::announce::Announce> for MoqAnnounce {
	fn from(announce: moq_net::announce::Announce) -> Self {
		Self {
			prefix: announce.prefix.to_string(),
			captures: announce
				.captures
				.expect("bindings only list full announcement matches")
				.into_iter()
				.map(|capture| capture.to_string())
				.collect(),
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
	/// A route now covers the prefix and continues what the route at `predecessor`
	/// served beneath it: keep what was resolved through that one.
	Takeover {
		/// The broadcast announcement affected by this event.
		announce: MoqAnnounce,
		/// The prefix of the route it continues, at or above `announce`'s.
		predecessor: String,
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
			Event::Takeover { announce, predecessor } => Self::Takeover {
				announce: announce.into(),
				predecessor: predecessor.to_string(),
			},
		}
	}
}

/// Follows a specific broadcast across the publisher instances that serve it.
#[derive(uniffi::Object)]
pub struct MoqFollowing {
	reads: Polled<moq_net::origin::Following>,
	origin: moq_net::origin::Consumer,
	driver: Background,
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
	let driver = Background::driver(driver);
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

	/// Create a route at exactly `path`, owning a new broadcast to fill.
	///
	/// Visible at once unless `options.hidden`: a hidden route exists for nobody, on this
	/// origin or its peers, until [`MoqRoute::announce`], so create it hidden to fill the
	/// catalog first. The route serves a fresh instance, kept across withdraw and announce,
	/// unless `options` names one. Hold the route: dropping or cancelling it retracts it, and
	/// [`MoqBroadcastProducer::close`] ends both.
	pub fn route(&self, path: String, options: MoqRouteOptions) -> Result<Arc<MoqRoute>, MoqError> {
		let _guard = crate::ffi::enter();
		let options = options.try_into()?;
		self.inner.driver.check()?;
		// Surfaces Error::Unauthorized (out of scope) via the MoqError::Protocol conversion.
		let result = self.inner.route(path.as_str(), options);
		self.inner.driver.check()?;
		Ok(Arc::new(MoqRoute::exact(result?, self.inner.driver.clone())?))
	}

	/// Create a route over `prefix` that yields the requests beneath it.
	///
	/// A route claims `prefix` and every path beneath it (the empty prefix
	/// claims every path). A service that only serves some of them advertises
	/// the covering prefix and rejects the rest as they are requested. Answer each
	/// [`MoqRoute::requested`] request; hold the route while it should stay advertised.
	pub fn route_prefix(&self, prefix: String, options: MoqRouteOptions) -> Result<Arc<MoqRoute>, MoqError> {
		let _guard = crate::ffi::enter();
		let options = options.try_into()?;
		self.inner.driver.check()?;
		let result = self.inner.route_prefix(prefix, options);
		self.inner.driver.check()?;
		Ok(Arc::new(MoqRoute::prefix(result?, self.inner.driver.clone())))
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
			reads: Polled::new(origin.announced()).supervised(self.driver.clone()),
		}))
	}

	/// Follow the broadcast at `path` across the instances that serve it.
	///
	/// Unlike `request`, this waits for future announcements and keeps following the
	/// instances serving the path. A local broadcast resolves once announced, like a remote
	/// one. Pass an `instance` to follow only that stream: another one taking over is never
	/// handed out, and the stream is handed out again if it returns after an end.
	#[uniffi::method(default(instance = None))]
	pub fn follow(&self, path: String, instance: Option<String>) -> Result<Arc<MoqFollowing>, MoqError> {
		let _guard = crate::ffi::enter();
		self.driver.check()?;
		let instance: Option<moq_net::Instance> = instance.as_deref().map(str::parse).transpose()?;
		// Following checks the permission eagerly, so an unreachable path fails here rather
		// than surfacing later as a `Closed` the caller can't tell from the origin ending.
		let inner = self.inner.follow(path.as_str(), instance)?;

		Ok(Arc::new(MoqFollowing {
			reads: Polled::new(inner).supervised(self.driver.clone()),
			origin: self.inner.clone(),
			driver: self.driver.clone(),
		}))
	}

	/// Request the broadcast at `path`, resolving one publisher instance as soon as it can be served.
	///
	/// Resolves through the best announced route covering the path: an announced broadcast
	/// on this origin, a route a session announced (served on demand by that session), or a
	/// dynamic handler on the origin. The most specific prefix wins, then the cheapest. An
	/// unannounced broadcast is unroutable once discovery has synced without a covering route.
	/// The request owns discovery of its path and retries route churn until then; no separate
	/// announcement stream is needed. Pass an `instance` to refuse any other stream. Drop the
	/// returned future to cancel.
	#[uniffi::method(default(instance = None))]
	pub async fn request(&self, path: String, instance: Option<String>) -> Result<Arc<MoqBroadcastConsumer>, MoqError> {
		let instance: Option<moq_net::Instance> = instance.as_deref().map(str::parse).transpose()?;
		let broadcast = self
			.driver
			.watch(async { Ok(self.inner.request(path.as_str(), instance).await?) })
			.await?;
		Ok(Arc::new(MoqBroadcastConsumer::routed(
			broadcast,
			self.inner.clone(),
			self.driver.clone(),
		)))
	}
}

// ---- MoqRoute ----

impl MoqRoute {
	/// Wrap an exact route, attaching the catalog track every FFI broadcast carries.
	fn exact(route: moq_net::origin::Route, driver: Background) -> Result<Self, MoqError> {
		let broadcast = MoqBroadcastProducer::from_inner(route.broadcast().clone(), driver.clone())?;
		Ok(Self::new(route, Some(Arc::new(broadcast)), driver))
	}

	fn prefix(route: moq_net::origin::Route, driver: Background) -> Self {
		Self::new(route, None, driver)
	}

	fn new(route: moq_net::origin::Route, broadcast: Option<Arc<MoqBroadcastProducer>>, driver: Background) -> Self {
		Self {
			instance: route.instance().to_string(),
			reads: Polled::new(route).supervised(driver.clone()),
			broadcast,
			driver,
		}
	}

	/// Run `f` against the route, refused with `Cancelled` once it was cancelled.
	fn with<R>(&self, f: impl FnOnce(&moq_net::origin::Route) -> Result<R, moq_net::Error>) -> Result<R, MoqError> {
		let _guard = crate::ffi::enter();
		self.driver.check()?;
		let result = self.reads.peek(f).ok_or(MoqError::Cancelled)?;
		self.driver.check()?;
		Ok(result?)
	}
}

#[uniffi::export]
impl MoqRoute {
	/// The broadcast this exact route owns, to fill with tracks.
	///
	/// Errors on a prefix route, which owns none: answer its requests instead.
	pub fn broadcast(&self) -> Result<Arc<MoqBroadcastProducer>, MoqError> {
		self.broadcast.clone().ok_or_else(|| MoqError::Internal {
			reason: "a prefix route owns no broadcast; answer its requests".into(),
		})
	}

	/// The stream this route serves, minted at creation unless the options named one.
	///
	/// Named `instance_id` rather than `instance` because the generated C and C++
	/// objects keep their handle in a private field named `instance`; the wrappers
	/// expose it as `instance`.
	pub fn instance_id(&self) -> String {
		self.instance.clone()
	}

	/// Make the route visible, or keep it so.
	///
	/// A route created by [`MoqBroadcastRequest::route`] also answers the requesters waiting
	/// on its path. Errors with `Closed` once its broadcast closed, and with a protocol
	/// `Unroutable` when a request's route can never announce because the request was
	/// released or its prefix route retracted meanwhile.
	pub fn announce(&self) -> Result<(), MoqError> {
		self.with(|route| route.announce())
	}

	/// Hide the route from everyone, keeping its broadcast and instance: `announce` brings the
	/// same stream back. Tracks already in flight carry on.
	pub fn withdraw(&self) -> Result<(), MoqError> {
		self.with(|route| {
			route.withdraw();
			Ok(())
		})
	}

	/// Re-price the route in place: lower wins among routes covering the same prefix. A
	/// hidden route keeps the price for when it next announces. Errors with `Closed` once its
	/// broadcast closed.
	pub fn set_cost(&self, cost: u64) -> Result<(), MoqError> {
		self.with(|route| route.set_cost(cost))
	}

	/// Wait for the next broadcast a consumer requests beneath this prefix route.
	///
	/// Serve a held broadcast, route a new one, or reject the returned request; the requesting
	/// consumer stays pending until then. Returns `None` once the origin closes, and at once on
	/// an exact route, which yields no requests.
	pub async fn requested(&self) -> Result<Option<Arc<MoqBroadcastRequest>>, MoqError> {
		let request = self
			.reads
			.wait(|route, waiter| Poll::Ready(Ok(ready!(route.poll_requested(waiter)))))
			.await?;
		Ok(request.map(|request| Arc::new(MoqBroadcastRequest::new(request, self.driver.clone()))))
	}

	/// Retract the route and release it here, not when the handle is.
	///
	/// Terminal: pending requests are rejected and later calls fail with `Cancelled`. An exact
	/// route's broadcast lives on for its other handles until
	/// [`MoqBroadcastProducer::close`].
	pub fn cancel(&self) {
		self.reads.cancel();
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

	/// Answer with the path's own exact route, owning a new broadcast.
	///
	/// The route serves a fresh instance unless `options` names one, so a job that restarts
	/// is a new stream. Created visible, it announces now and answers the waiting
	/// requesters; created hidden, they wait until [`MoqRoute::announce`], so the job can
	/// fill its catalog first, and cancelling the route before then refuses them. Ending the
	/// route withdraws only this path.
	pub fn route(&self, options: MoqRouteOptions) -> Result<Arc<MoqRoute>, MoqError> {
		let _guard = crate::ffi::enter();
		let options = options.try_into()?;
		self.driver.check()?;
		let request = self.take()?;
		let result = request.route(options);
		self.driver.check()?;
		Ok(Arc::new(MoqRoute::exact(result?, self.driver.clone())?))
	}

	/// Serve a held broadcast under the prefix route's instance without announcing an exact route.
	///
	/// Every answer at this path must continue the same sequences and bytes. Start a new
	/// prefix route for different content, or route a job with its own instance.
	pub fn serve(&self, broadcast: &Arc<MoqBroadcastConsumer>) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		self.driver.check()?;
		broadcast.driver.check()?;
		self.take()?.serve(&broadcast.inner);
		self.driver.check()?;
		Ok(())
	}

	/// Reject the request with an application error code.
	pub fn reject(&self, error_code: u16) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let request = self.take()?;
		request.reject(moq_net::Error::App(error_code));
		Ok(())
	}
}

// ---- MoqAnnounceConsumer ----

#[uniffi::export]
impl MoqAnnounceConsumer {
	/// Get the next announce event. Returns `None` when the origin is closed.
	pub async fn next(&self) -> Result<Option<MoqAnnounceEvent>, MoqError> {
		self.reads
			.wait(|announced, waiter| Poll::Ready(Ok(ready!(announced.poll_next(waiter)).map(Into::into))))
			.await
	}

	/// Cancel all current and future `next()` calls.
	///
	/// Terminal: the announcement stream is released here, not when the handle is.
	pub fn cancel(&self) {
		self.reads.cancel();
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
		let broadcast = self
			.reads
			.wait(|following, waiter| Poll::Ready(Ok(ready!(following.poll_available(waiter))?)))
			.await?;
		Ok(Arc::new(MoqBroadcastConsumer::routed(
			broadcast,
			self.origin.clone(),
			self.driver.clone(),
		)))
	}

	/// Cancel all current and future `available()` calls.
	///
	/// Terminal: the announcement watch is released here, not when the handle is.
	pub fn cancel(&self) {
		self.reads.cancel();
	}
}

#[cfg(test)]
mod supervision_tests {
	use super::*;

	fn hidden() -> MoqRouteOptions {
		MoqRouteOptions {
			hidden: true,
			..Default::default()
		}
	}

	#[tokio::test]
	async fn serving_held_content_consumes_only_the_request() {
		let origin = MoqOriginProducer::new(Default::default());
		let requests = origin.route_prefix("room".into(), Default::default()).unwrap();
		let source = origin.route("source".into(), hidden()).unwrap().broadcast().unwrap();
		let held = source.consume().unwrap();
		let consumer = origin.consume();
		let (resolved, ()) = tokio::join!(consumer.request("room/camera".into(), None), async {
			let request = requests.requested().await.unwrap().unwrap();
			request.serve(&held).unwrap();
			assert!(matches!(request.serve(&held), Err(MoqError::Closed)));
		});
		assert!(resolved.is_ok());
		assert!(!held.inner.is_closed(), "serving leaves the caller's consumer usable");
		requests.cancel();
		source.close().unwrap();
	}

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
		let _route = healthy.route("healthy".into(), Default::default()).unwrap();
		healthy.consume().request("healthy".into(), None).await.unwrap();
	}

	#[tokio::test]
	async fn a_broadcast_keeps_the_origin_failure_after_its_origin_handle_is_released() {
		let (inner, completion, trigger) = crate::ffi::panicking_origin();
		let origin = MoqOriginProducer::from_inner(Origin {
			inner,
			driver: completion.clone(),
		});
		let route = origin.route("retained".into(), hidden()).unwrap();
		let dynamic = route.broadcast().unwrap().requests().unwrap();
		drop(origin);
		*trigger
			.write()
			.unwrap_or_else(|_| unreachable!("test keeps its panic trigger open")) = true;
		assert!(matches!(completion.finished().await, Err(MoqError::Task { .. })));
		drop(completion);
		assert!(matches!(route.announce(), Err(MoqError::Task { .. })));
		assert!(matches!(dynamic.requested_track().await, Err(MoqError::Task { .. })));
		let healthy = MoqOriginProducer::new(Default::default());
		let _route = healthy.route("healthy".into(), Default::default()).unwrap();
	}

	#[tokio::test]
	async fn a_finish_after_a_driver_panic_reports_it_until_the_close() {
		let (inner, completion, trigger) = crate::ffi::panicking_origin();
		let origin = MoqOriginProducer::from_inner(Origin {
			inner,
			driver: completion.clone(),
		});
		let broadcast = origin.route("retained".into(), hidden()).unwrap().broadcast().unwrap();
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
		let broadcast = origin.route("retained".into(), hidden()).unwrap().broadcast().unwrap();
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
		let route = origin.route("routed".into(), Default::default()).unwrap();
		let broadcast = route.broadcast().unwrap();
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
		drop(route);
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
