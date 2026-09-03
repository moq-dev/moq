use std::sync::Arc;

use crate::consumer::MoqBroadcastConsumer;
use crate::error::MoqError;
use crate::ffi::Task;
use crate::producer::MoqBroadcastProducer;

/// Options used when creating an origin.
#[derive(Clone, Default, uniffi::Record)]
pub struct MoqOriginOptions {
	/// Maximum cached group bytes across broadcasts under this origin. Null is unbounded.
	#[uniffi(default = None)]
	pub cache_capacity_bytes: Option<u64>,
}

/// A path-prefix route: an advertisement that content under `prefix` can be served.
///
/// Announce one with `MoqOriginProducer::announce` and observe them with
/// `MoqOriginConsumer::announced`. A route claims capability, not inventory: by
/// convention a publisher announces each broadcast's exact path, so subscribers
/// can enumerate broadcasts, while a service announces one short prefix and
/// answers whatever is requested beneath it.
#[derive(Clone, Default, uniffi::Record)]
pub struct MoqRoute {
	/// Hop ids of the relay hops the route traversed, oldest first.
	#[uniffi(default = [])]
	pub hops: Vec<u64>,
	/// Preference among routes covering the same prefix: lower wins. A publisher
	/// sets its production cost here: zero for content it is already producing,
	/// larger for content it would have to start producing on demand.
	#[uniffi(default = 0)]
	pub cost: u64,
}

impl From<moq_net::origin::Route> for MoqRoute {
	fn from(route: moq_net::origin::Route) -> Self {
		Self {
			hops: route.hops.iter().map(|origin| origin.id()).collect(),
			// The relay mesh prices a route twice (see `origin::Cost`); an
			// application only ever wants what pulling it costs today.
			cost: route.cost.warm,
		}
	}
}

impl TryFrom<MoqRoute> for moq_net::origin::Route {
	type Error = MoqError;

	fn try_from(route: MoqRoute) -> Result<Self, MoqError> {
		let mut out = moq_net::origin::Route::default().with_cost(route.cost);
		for id in route.hops {
			let origin = moq_net::Hop::new(id).map_err(|e| MoqError::InvalidRoute(e.to_string()))?;
			out = out
				.with_hop(origin)
				.map_err(|e| MoqError::InvalidRoute(e.to_string()))?;
		}
		Ok(out)
	}
}

#[derive(uniffi::Object)]
pub struct MoqOriginProducer {
	inner: moq_net::origin::Producer,
}

#[derive(uniffi::Object)]
pub struct MoqOriginConsumer {
	inner: moq_net::origin::Consumer,
}

#[derive(uniffi::Object)]
pub struct MoqAnnounced {
	task: Task<Announced>,
}

#[derive(uniffi::Object)]
/// A dynamic origin handler that serves broadcast requests not resolved by an existing route.
pub struct MoqOriginDynamic {
	task: std::sync::Mutex<Option<Arc<Task<OriginDynamic>>>>,
}

#[derive(uniffi::Object)]
/// A pending dynamic broadcast request that must be accepted or aborted.
pub struct MoqBroadcastRequest {
	inner: std::sync::Mutex<Option<moq_net::origin::Request>>,
}

struct Announced {
	inner: moq_net::announce::Consumer,
}

struct OriginDynamic {
	inner: moq_net::origin::Dynamic,
}

impl OriginDynamic {
	async fn requested_broadcast(&mut self) -> Result<Arc<MoqBroadcastRequest>, MoqError> {
		let request = self.inner.requested_broadcast().await?;
		Ok(Arc::new(MoqBroadcastRequest::new(request)))
	}
}

impl Announced {
	async fn next(&mut self) -> Result<Option<Arc<MoqAnnouncement>>, MoqError> {
		match self.inner.next().await {
			Some(update) => Ok(Some(Arc::new(MoqAnnouncement {
				prefix: update.prefix.as_path().to_string(),
				route: update.route.into(),
				active: update.active,
			}))),
			None => Ok(None),
		}
	}
}

/// Waits for a route covering one exact path, then resolves it.
struct AnnouncedBroadcast {
	origin: moq_net::origin::Consumer,
	path: moq_net::PathOwned,
}

impl AnnouncedBroadcast {
	async fn available(&mut self) -> Result<Arc<MoqBroadcastConsumer>, MoqError> {
		// `routed_broadcast` rides out the churn between a route covering the path
		// and the path actually resolving (failover, an advertise-only announce
		// racing its handler).
		let broadcast = self.origin.routed_broadcast(&self.path).await?;
		Ok(Arc::new(MoqBroadcastConsumer::routed(broadcast, self.origin.clone())))
	}
}

/// A route announcement (or retraction) from an origin.
///
/// Carries no broadcast: resolve a specific path with
/// `MoqOriginConsumer::request_broadcast` (after this announcement proves it is
/// covered). The application decides which paths name broadcasts.
#[derive(uniffi::Object)]
pub struct MoqAnnouncement {
	prefix: String,
	route: MoqRoute,
	active: bool,
}

/// A live route advertisement, from `MoqOriginProducer::announce`.
///
/// The route stays advertised until `cancel` is called or the handle drops.
#[derive(uniffi::Object)]
pub struct MoqAnnounce {
	inner: std::sync::Mutex<Option<moq_net::announce::Producer>>,
}

/// Waits for a specific broadcast to be announced.
#[derive(uniffi::Object)]
pub struct MoqAnnouncedBroadcast {
	task: Task<AnnouncedBroadcast>,
}

impl MoqOriginProducer {
	pub(crate) fn inner(&self) -> &moq_net::origin::Producer {
		&self.inner
	}

	/// Wrap an existing `moq_net::origin::Producer` (e.g. one auto-created
	/// during `MoqClient::connect`) so it can cross the FFI boundary.
	pub(crate) fn from_inner(inner: moq_net::origin::Producer) -> Self {
		Self { inner }
	}

	fn from_options(options: MoqOriginOptions) -> Self {
		let mut info = moq_net::origin::Info::new(moq_net::Hop::random());
		if let Some(capacity) = options.cache_capacity_bytes {
			let config = moq_net::cache::Config::default()
				.with_capacity(capacity)
				.with_expiry(info.pool.expiry());
			info = info.with_pool(moq_net::cache::Pool::new(config));
		}

		Self { inner: spawn(info) }
	}
}

/// Build an origin producer, spawning its driver on the FFI runtime.
pub(crate) fn spawn(info: moq_net::origin::Info) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(info);
	#[cfg(not(target_arch = "wasm32"))]
	crate::ffi::spawn(driver.run(moq_tokio::runtime::Runtime::<()>::new()));
	#[cfg(target_arch = "wasm32")]
	crate::ffi::spawn(driver.run(crate::runtime::Runtime));
	producer
}

impl MoqOriginConsumer {
	pub(crate) fn from_inner(inner: moq_net::origin::Consumer) -> Self {
		Self { inner }
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
) -> (moq_net::origin::Producer, moq_net::origin::Producer) {
	if publish.is_none() && consume.is_none() {
		// Clones of a Producer share the underlying origin, so this is one origin, not two.
		let shared = spawn(moq_net::Hop::random().into());
		return (shared.clone(), shared);
	}

	let resolve = |origin: Option<&Arc<MoqOriginProducer>>| {
		origin
			.map(|o| o.inner().clone())
			.unwrap_or_else(|| spawn(moq_net::Hop::random().into()))
	};
	(resolve(publish), resolve(consume))
}

#[uniffi::export]
impl MoqOriginProducer {
	/// Create a new origin for publishing and/or consuming broadcasts.
	#[uniffi::constructor]
	pub fn new(options: MoqOriginOptions) -> Arc<Self> {
		let _guard = crate::ffi::enter();
		Arc::new(Self::from_options(options))
	}

	/// Create a consumer for this origin.
	pub fn consume(&self) -> Arc<MoqOriginConsumer> {
		let _guard = crate::ffi::enter();
		Arc::new(MoqOriginConsumer {
			inner: self.inner.consume(),
		})
	}

	/// Create a dynamic handler for serving unannounced broadcasts on request.
	///
	/// Hold the returned object while missing broadcast requests should be accepted.
	/// Dropping it makes future requests to unknown broadcasts fail.
	pub fn dynamic(&self) -> Arc<MoqOriginDynamic> {
		let _guard = crate::ffi::enter();
		Arc::new(MoqOriginDynamic {
			task: std::sync::Mutex::new(Some(Arc::new(Task::new(OriginDynamic {
				inner: self.inner.dynamic(),
			})))),
		})
	}

	/// Create a broadcast at `path` on this origin, returning the producer that feeds it.
	///
	/// The broadcast starts announced: the origin advertises the exact path as a route so
	/// subscribers can discover it, becoming visible shortly after this returns. Toggle
	/// discoverability with [`MoqBroadcastProducer::set_announce`]; an unannounced broadcast
	/// stays reachable by exact path for subscribes and fetches without being announced.
	///
	/// [`MoqBroadcastProducer::finish`] unpublishes immediately. Dropping the producer
	/// without finishing also unpublishes, but subscribers observe the end as a
	/// failure rather than a deliberate one.
	pub fn create_broadcast(&self, path: String) -> Result<Arc<MoqBroadcastProducer>, MoqError> {
		let _guard = crate::ffi::enter();
		// Surfaces Error::Unauthorized (out of scope) via the MoqError::Protocol conversion.
		let broadcast = self.inner.create_broadcast(path.as_str())?;
		let announcement = self.inner.announce(path.as_str(), Default::default())?;
		Ok(Arc::new(MoqBroadcastProducer::from_inner_announced(
			broadcast,
			Some(crate::producer::AnnounceState {
				origin: self.inner.clone(),
				path: moq_net::Path::new(&path).to_owned(),
				announcement: Some(announcement),
			}),
		)?))
	}

	/// Advertise a route: a claim that paths under `prefix` can be served.
	///
	/// The route is visible to subscribers until the returned handle is cancelled
	/// (or dropped). Announcing is independent of `create_broadcast`: announce one
	/// short prefix and serve requests beneath it with [`Self::dynamic`], or
	/// advertise extra exact paths.
	pub fn announce(&self, prefix: String, route: MoqRoute) -> Result<Arc<MoqAnnounce>, MoqError> {
		let _guard = crate::ffi::enter();
		let route: moq_net::origin::Route = route.try_into()?;
		let announcement = self.inner.announce(prefix.as_str(), route)?;
		Ok(Arc::new(MoqAnnounce {
			inner: std::sync::Mutex::new(Some(announcement)),
		}))
	}
}

#[uniffi::export]
impl MoqAnnounce {
	/// Re-price the route in place: replace its hops and cost. The prefix cannot
	/// change; announce a new route instead.
	pub fn update(&self, route: MoqRoute) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let route: moq_net::origin::Route = route.try_into()?;
		let guard = self.inner.lock().unwrap();
		let announcement = guard.as_ref().ok_or(MoqError::Closed)?;
		announcement.update(route)?;
		Ok(())
	}

	/// Retract the route. Terminal: the advertisement is withdrawn here, not when
	/// the handle is released.
	pub fn cancel(&self) {
		let _guard = crate::ffi::enter();
		self.inner.lock().unwrap().take();
	}
}

#[uniffi::export]
impl MoqOriginConsumer {
	/// Subscribe to all route announcements under a prefix.
	pub fn announced(&self, prefix: String) -> Result<Arc<MoqAnnounced>, MoqError> {
		let _guard = crate::ffi::enter();
		let origin = self.inner.with_root(prefix).ok_or(MoqError::Unauthorized)?;
		Ok(Arc::new(MoqAnnounced {
			task: Task::new(Announced {
				inner: origin.announced(),
			}),
		}))
	}

	/// Wait for a route to cover `path`, then resolve the broadcast there.
	///
	/// This is how you resolve a path right after connecting: announcements arrive over the
	/// session after it opens, so `request_broadcast` on its own races them.
	pub fn announced_broadcast(&self, path: String) -> Result<Arc<MoqAnnouncedBroadcast>, MoqError> {
		let _guard = crate::ffi::enter();
		let path = moq_net::Path::new(&path).to_owned();

		// Probe the permission eagerly so an unreachable path fails here, rather than
		// surfacing later as a `Closed` the caller can't tell from the origin ending.
		self.inner.with_root(&path).ok_or(MoqError::Unauthorized)?;

		Ok(Arc::new(MoqAnnouncedBroadcast {
			task: Task::new(AnnouncedBroadcast {
				// The wait runs on the *unrooted* cursor. `announced_broadcast` narrows with
				// `scope`, which leaves the root alone, so the broadcast is handed out named by
				// its full path. Rooting the cursor at `path` would name it "" instead, making
				// it its own root, and a catalog's `../sibling` reference would read as escaping.
				origin: self.inner.clone(),
				path,
			}),
		}))
	}

	/// Request a broadcast by path, resolving as soon as it can be served.
	///
	/// Resolution order: a local broadcast at the exact path, then the best announced route
	/// covering the path (served on demand by the session that announced it), then a dynamic
	/// handler on the origin (if any). Errors if nothing can serve it. Unlike
	/// `announced_broadcast`, this does *not* wait for a future announcement. Drop the
	/// returned future to cancel.
	///
	/// Calling this straight after connecting therefore races the session's announcements
	/// and can report a live broadcast as unroutable. Await `announced_broadcast` first.
	pub async fn request_broadcast(&self, path: String) -> Result<Arc<MoqBroadcastConsumer>, MoqError> {
		let broadcast = self.inner.request_broadcast(path.as_str()).await?;
		Ok(Arc::new(MoqBroadcastConsumer::routed(broadcast, self.inner.clone())))
	}
}

// ---- MoqOriginDynamic ----

#[uniffi::export]
impl MoqOriginDynamic {
	/// Wait for the next requested broadcast that is not announced.
	///
	/// Returns a [`MoqBroadcastRequest`]: accept it with a broadcast producer or abort
	/// it with an application error code. The requesting consumer stays pending until then.
	pub async fn requested_broadcast(&self) -> Result<Arc<MoqBroadcastRequest>, MoqError> {
		let task = self.task.lock().unwrap().clone().ok_or(MoqError::Closed)?;
		task.run(|mut state| async move { state.requested_broadcast().await })
			.await
	}

	/// Stop serving dynamic requests and cancel all current and future `requested_broadcast()`
	/// calls.
	///
	/// Terminal: the dynamic origin is released here, not when the handle is, so any pending
	/// request is rejected.
	pub fn cancel(&self) {
		if let Some(task) = self.task.lock().unwrap().take() {
			task.cancel();
		}
	}
}

// ---- MoqBroadcastRequest ----

impl MoqBroadcastRequest {
	fn new(request: moq_net::origin::Request) -> Self {
		Self {
			inner: std::sync::Mutex::new(Some(request)),
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

	/// Accept the request with an unannounced broadcast.
	pub fn accept(&self, broadcast: &MoqBroadcastProducer) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let consumer = broadcast.consume_inner()?;
		let request = self.take()?;
		request.accept(&consumer);
		Ok(())
	}

	/// Abort the request with an application error code.
	pub fn abort(&self, error_code: u16) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let request = self.take()?;
		request.reject(moq_net::Error::App(error_code));
		Ok(())
	}
}

// ---- MoqAnnounced ----

#[uniffi::export]
impl MoqAnnounced {
	/// Get the next route announcement or retraction. Returns `None` when the origin is closed.
	pub async fn next(&self) -> Result<Option<Arc<MoqAnnouncement>>, MoqError> {
		self.task.run(|mut state| async move { state.next().await }).await
	}

	/// Cancel all current and future `next()` calls.
	///
	/// Terminal: the announcement stream is released here, not when the handle is.
	pub fn cancel(&self) {
		self.task.cancel();
	}
}

#[uniffi::export]
impl MoqAnnouncement {
	/// The covered prefix, relative to the `announced` call's prefix.
	pub fn path(&self) -> String {
		self.prefix.clone()
	}

	/// The route serving the prefix: its hops and cost.
	pub fn route(&self) -> MoqRoute {
		self.route.clone()
	}

	/// Whether the route is active (`true`) or was retracted (`false`). A repeated
	/// active announcement for the same prefix is a metadata update.
	pub fn active(&self) -> bool {
		self.active
	}
}

// ---- MoqAnnouncedBroadcast ----

#[uniffi::export]
impl MoqAnnouncedBroadcast {
	/// Wait until the broadcast is announced. Returns `Closed` if cancelled or the origin is closed.
	///
	/// Use `broadcast.closed()` to learn when a broadcast is unannounced.
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
