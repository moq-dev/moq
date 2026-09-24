use std::sync::Arc;

use crate::consumer::MoqBroadcastConsumer;
use crate::error::MoqError;
use crate::ffi::Task;
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
}

/// A path-prefix route: hops and costs for an advertisement.
///
/// Pair one with `MoqBroadcastProducer::announce` for an exact path, or with
/// `MoqOriginProducer::dynamic` for a prefix. Observe them with
/// `MoqOriginConsumer::announced`. A route claims capability, not inventory: a publisher advertises each broadcast's exact path to peers once ready,
/// while local consumers can enumerate it from creation, while a service advertises a prefix and answers
/// whatever is requested beneath it.
#[derive(Clone, Debug, Default, PartialEq, Eq, uniffi::Record)]
pub struct MoqRoute {
	/// Hop ids of the relay hops the route traversed, oldest first. 0 is the
	/// anonymous mark and is legal on a received chain.
	#[uniffi(default = [])]
	pub hops: Vec<u64>,
	/// Preference among routes covering the same prefix: lower wins. A publisher
	/// sets its production cost here: zero for content it is already producing,
	/// larger for content it would have to start producing on demand.
	#[uniffi(default = 0)]
	pub cost: u64,
	/// The same path with every warm discount removed: what pulling the content
	/// would cost if no relay along it were carrying anything. `None` means the
	/// same as `cost`, which is right for a publisher seeding a production cost.
	#[uniffi(default = None)]
	pub cold: Option<u64>,
	/// Whether the chain holds a 0 anywhere. An anonymous route ranks below every
	/// fully identified one, whatever the costs say.
	#[uniffi(default = false)]
	pub anonymous: bool,
}

impl From<moq_net::origin::Route> for MoqRoute {
	fn from(route: moq_net::origin::Route) -> Self {
		Self {
			hops: route.hops.iter().map(|origin| origin.id()).collect(),
			cost: route.cost.warm,
			cold: Some(route.cost.cold),
			anonymous: route.is_anonymous(),
		}
	}
}

impl TryFrom<MoqRoute> for moq_net::origin::Route {
	type Error = MoqError;

	fn try_from(route: MoqRoute) -> Result<Self, MoqError> {
		let cold = route.cold.unwrap_or(route.cost);
		let mut hops = moq_net::Hops::new();
		for id in route.hops {
			let origin = if id == 0 {
				moq_net::Hop::UNKNOWN
			} else {
				moq_net::Hop::new(id).map_err(|e| MoqError::InvalidRoute(e.to_string()))?
			};
			hops.push(origin).map_err(|e| MoqError::InvalidRoute(e.to_string()))?;
		}
		Ok(moq_net::origin::Route::default()
			.with_cost(moq_net::origin::Cost { warm: route.cost, cold })
			.with_hops(hops))
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
pub struct MoqAnnounceConsumer {
	task: Task<Announced>,
}

#[derive(uniffi::Object)]
/// A served route: advertises a path prefix and yields the broadcast requests
/// beneath it for the application to accept or reject.
pub struct MoqOriginDynamic {
	slot: Slot,
	task: std::sync::Mutex<Option<Arc<Task<OriginDynamic>>>>,
}

#[derive(uniffi::Object)]
/// A pending dynamic broadcast request that must be accepted or rejected.
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
	async fn next(&mut self) -> Result<Option<Arc<MoqAnnounceUpdate>>, MoqError> {
		// The bindings have no caught-up marker yet.
		let update = loop {
			match self.inner.next().await {
				Some(moq_net::announce::Event::Update(update)) => break Some(update),
				Some(moq_net::announce::Event::Live) => continue,
				None => break None,
			}
		};
		match update {
			Some(update) => Ok(Some(Arc::new(MoqAnnounceUpdate {
				prefix: update.prefix.to_string(),
				captures: update
					.captures
					.map(|captures| captures.into_iter().map(|capture| capture.to_string()).collect()),
				route: update.route.into(),
				active: update.kind.is_active(),
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
		// `routed_broadcast` rides out the churn between something covering the path
		// and the path actually resolving (failover, an advertise-only announce
		// racing its handler).
		let broadcast = self.origin.routed_broadcast(&self.path).await?;
		Ok(Arc::new(MoqBroadcastConsumer::routed(broadcast, self.origin.clone())))
	}
}

/// A route announcement (or retraction) from an origin.
///
/// Carries no broadcast: resolve a specific path with
/// `MoqOriginConsumer::request_broadcast` (after this update proves it is
/// covered). Its prefix is relative to the origin. The application decides
/// which paths name broadcasts.
#[derive(uniffi::Object)]
pub struct MoqAnnounceUpdate {
	prefix: String,
	captures: Option<Vec<String>>,
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

/// Build an origin producer, spawning its driver on the FFI runtime.
pub(crate) fn spawn(config: moq_net::origin::Config) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(config);
	crate::ffi::spawn(async move {
		moq_net::time::run(driver).await;
	});
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
		Arc::new(MoqOriginConsumer {
			inner: self.inner.consume(),
		})
	}

	/// Advertise `prefix` and serve the requests beneath it.
	///
	/// A route claims `prefix` and every path beneath it (the empty prefix
	/// claims every path). A service that only serves some of them advertises
	/// the covering prefix and rejects the rest as they are requested. Hold
	/// the returned handle while the route should stay advertised and missing
	/// broadcasts should be served. Create, attach this for tracks served on
	/// demand, populate, then announce.
	pub fn dynamic(&self, prefix: String, route: MoqRoute) -> Result<Arc<MoqOriginDynamic>, MoqError> {
		let _guard = crate::ffi::enter();
		let route: moq_net::origin::Route = route.try_into()?;
		let dynamic = self.inner.dynamic(prefix, route)?;
		let slot = Arc::new(std::sync::Mutex::new(Some(dynamic)));
		Ok(Arc::new(MoqOriginDynamic {
			slot: slot.clone(),
			task: std::sync::Mutex::new(Some(Arc::new(Task::new(OriginDynamic { slot })))),
		}))
	}

	/// Create a broadcast at `path` on this origin, returning the producer that feeds it.
	///
	/// The broadcast appears on this origin's local announcement streams immediately.
	/// Advertise it to peers with
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
	/// Subscribe to routes matching a pattern scope; updates stay relative to the origin.
	pub fn announced(&self, config: MoqAnnounceConfig) -> Result<Arc<MoqAnnounceConsumer>, MoqError> {
		let _guard = crate::ffi::enter();
		let filter = match config.filter {
			Some(filter) => filter.parse::<moq_net::Pattern>()?,
			None => moq_net::Pattern::all(),
		};
		let filter = filter.rooted(&config.prefix)?;
		let origin = self.inner.scope("", &moq_net::Patterns::from(filter))?;
		Ok(Arc::new(MoqAnnounceConsumer {
			task: Task::new(Announced {
				inner: origin.announced(),
			}),
		}))
	}

	/// Resolve the broadcast at `path`, waiting until something can serve it.
	///
	/// This is how you resolve a path right after connecting: announcements arrive over the
	/// session after it opens, so `request_broadcast` on its own races them. A
	/// local broadcast appears on this origin's cursor when created, whether or not
	/// it has been advertised to peers.
	pub fn announced_broadcast(&self, path: String) -> Result<Arc<MoqAnnouncedBroadcast>, MoqError> {
		let _guard = crate::ffi::enter();
		let path = moq_net::Path::new(&path).to_owned();

		// Probe the permission eagerly so an unreachable path fails here, rather than
		// surfacing later as a `Closed` the caller can't tell from the origin ending.
		self.inner
			.scope(&path, &moq_net::Patterns::from(moq_net::Pattern::all()))?;

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
	/// handler on the origin (if any). Unlike `announced_broadcast`, this answers for what is
	/// reachable *now* and errors if nothing can serve the path. Drop the returned future to
	/// cancel.
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
	/// Wait for the next requested broadcast no local broadcast resolves under
	/// this handle's prefix.
	///
	/// Returns a [`MoqBroadcastRequest`]: accept it with a broadcast producer or reject
	/// it with an application error code. The requesting consumer stays pending until then.
	pub async fn requested_broadcast(&self) -> Result<Arc<MoqBroadcastRequest>, MoqError> {
		let task = self.task.lock().unwrap().clone().ok_or(MoqError::Closed)?;
		task.run(|mut state| async move { state.requested_broadcast().await })
			.await
	}

	/// Re-price the route in place: replace its hops and costs. The prefix cannot
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
	/// Get the next route announcement or retraction. Returns `None` when the origin is closed.
	pub async fn next(&self) -> Result<Option<Arc<MoqAnnounceUpdate>>, MoqError> {
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
impl MoqAnnounceUpdate {
	/// The covered prefix, relative to the origin.
	pub fn prefix(&self) -> String {
		self.prefix.clone()
	}

	/// What each wildcard matched, or `None` when the route only overlaps the scope.
	pub fn captures(&self) -> Option<Vec<String>> {
		self.captures.clone()
	}

	/// The route serving the prefix: its hops and costs.
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
	/// Use `broadcast.closed()` to learn when the broadcast ends.
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
