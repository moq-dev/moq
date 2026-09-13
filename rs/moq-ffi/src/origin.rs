use std::sync::Arc;

use crate::cancel::{self, MoqCancel};
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

/// A path-prefix route: hops and cost for an advertisement.
///
/// Pair one with `MoqBroadcastProducer::announce` for an exact path, or with
/// `MoqOriginProducer::dynamic` for a pattern. Observe them with
/// `MoqOriginConsumer::announced`. A route claims capability, not inventory: by
/// convention a publisher announces each broadcast's exact path, so subscribers
/// can enumerate broadcasts, while a service advertises a pattern and answers
/// whatever is requested beneath it.
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
/// A served route: advertises a path pattern and yields the broadcast requests
/// beneath it for the application to accept or reject.
pub struct MoqOriginDynamic {
	slot: Slot,
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

/// The served route behind one `MoqOriginProducer::dynamic` handle, shared
/// between the handle and the task draining it. Emptied by the handle's cancel,
/// which retracts the route: the task sees the empty slot and ends.
type Slot = Arc<std::sync::Mutex<Option<moq_net::origin::Dynamic>>>;

struct OriginDynamic {
	slot: Slot,
}

impl OriginDynamic {
	async fn requested_broadcast(&mut self) -> Result<Arc<MoqBroadcastRequest>, MoqError> {
		let request = kio::wait(|waiter| match self.slot.lock().unwrap().as_ref() {
			Some(dynamic) => dynamic.poll_requested_broadcast(waiter),
			None => std::task::Poll::Ready(Err(moq_net::Error::Closed)),
		})
		.await
		.map_err(|err| match err {
			moq_net::Error::Closed => MoqError::Closed,
			other => other.into(),
		})?;
		Ok(Arc::new(MoqBroadcastRequest::new(request)))
	}
}

impl Announced {
	async fn next(&mut self) -> Result<Option<Arc<MoqAnnouncement>>, MoqError> {
		match self.inner.next().await {
			Some(update) => Ok(Some(Arc::new(MoqAnnouncement {
				prefix: update
					.pattern
					.as_prefix()
					.unwrap_or_else(|| update.pattern.as_str())
					.to_owned(),
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

	/// Advertise `pattern` and serve the requests beneath it.
	///
	/// `pattern` uses the `moq_net::Pattern` dialect; a prefix is `foo/**`.
	/// Wildcards are advertised, but only prefix-shaped patterns serve requests. Hold
	/// the returned handle while the route should stay advertised and missing
	/// broadcasts should be served. Create, attach this for tracks served on
	/// demand, populate, then announce.
	pub fn dynamic(&self, pattern: String, route: MoqRoute) -> Result<Arc<MoqOriginDynamic>, MoqError> {
		let _guard = crate::ffi::enter();
		let pattern: moq_net::Pattern = pattern.parse()?;
		let route: moq_net::origin::Route = route.try_into()?;
		let dynamic = self.inner.dynamic(pattern, route)?;
		let slot = Arc::new(std::sync::Mutex::new(Some(dynamic)));
		Ok(Arc::new(MoqOriginDynamic {
			slot: slot.clone(),
			task: std::sync::Mutex::new(Some(Arc::new(Task::new(OriginDynamic { slot })))),
		}))
	}

	/// Create a broadcast at `path` on this origin, returning the producer that feeds it.
	///
	/// The broadcast starts unadvertised: reachable by exact path for subscribes
	/// and fetches, but not visible to announcement streams. Advertise it with
	/// [`MoqBroadcastProducer::announce`] after populating tracks; an on-demand
	/// handler is [`Self::dynamic`]. Create, `dynamic()` if tracks are served on
	/// demand, populate, then announce.
	///
	/// [`MoqBroadcastProducer::finish`] unpublishes immediately. Dropping the producer
	/// without finishing also unpublishes, but subscribers observe the end as a
	/// failure rather than a deliberate one.
	pub fn create_broadcast(&self, path: String) -> Result<Arc<MoqBroadcastProducer>, MoqError> {
		let _guard = crate::ffi::enter();
		// Surfaces Error::Unauthorized (out of scope) via the MoqError::Protocol conversion.
		let broadcast = self.inner.create_broadcast(path.as_str())?;
		Ok(Arc::new(MoqBroadcastProducer::from_inner(broadcast)?))
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
	///
	/// `cancel` aborts this call alone; see [`MoqCancel`].
	#[uniffi::method(default(cancel = None))]
	pub async fn request_broadcast(
		&self,
		path: String,
		cancel: Option<Arc<MoqCancel>>,
	) -> Result<Arc<MoqBroadcastConsumer>, MoqError> {
		cancel::guard(cancel, async {
			let broadcast = self.inner.request_broadcast(path.as_str()).await?;
			Ok(Arc::new(MoqBroadcastConsumer::routed(broadcast, self.inner.clone())))
		})
		.await
	}
}

// ---- MoqOriginDynamic ----

#[uniffi::export]
impl MoqOriginDynamic {
	/// Wait for the next requested broadcast no local broadcast resolves under
	/// this handle's pattern.
	///
	/// Returns a [`MoqBroadcastRequest`]: accept it with a broadcast producer or abort
	/// it with an application error code. The requesting consumer stays pending until then.
	pub async fn requested_broadcast(&self) -> Result<Arc<MoqBroadcastRequest>, MoqError> {
		let task = self.task.lock().unwrap().clone().ok_or(MoqError::Closed)?;
		task.run(|mut state| async move { state.requested_broadcast().await })
			.await
	}

	/// Re-price the route in place: replace its hops and cost. The pattern cannot
	/// change; call `dynamic` again instead.
	pub fn update(&self, route: MoqRoute) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let route: moq_net::origin::Route = route.try_into()?;
		let slot = self.slot.lock().unwrap();
		let dynamic = slot.as_ref().ok_or(MoqError::Closed)?;
		Ok(dynamic.update(route)?)
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
