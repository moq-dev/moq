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

/// The path a broadcast takes to reach this origin, and how preferable it is.
///
/// Dynamic: it changes when the serving route fails over or the publisher
/// re-advertises itself. Publish changes with `MoqBroadcastProducer::set_route`
/// and observe them with `MoqBroadcastConsumer::route_updates`.
#[derive(Clone, Default, uniffi::Record)]
pub struct MoqRoute {
	/// Origin ids of the relay hops the broadcast traversed, oldest first.
	#[uniffi(default = [])]
	pub hops: Vec<u64>,
	/// Preference among routes serving the same broadcast: lower wins.
	#[uniffi(default = 0)]
	pub cost: u64,
	/// Whether the broadcast is announced: advertised to subscribers via the origin.
	/// An unannounced broadcast stays reachable by exact path for subscribes and fetches.
	#[uniffi(default = false)]
	pub announce: bool,
}

impl From<moq_net::broadcast::Route> for MoqRoute {
	fn from(route: moq_net::broadcast::Route) -> Self {
		Self {
			hops: route.hops.iter().map(|origin| origin.id()).collect(),
			cost: route.cost,
			announce: route.announce,
		}
	}
}

impl TryFrom<MoqRoute> for moq_net::broadcast::Route {
	type Error = MoqError;

	fn try_from(route: MoqRoute) -> Result<Self, MoqError> {
		let mut out = moq_net::broadcast::Route::new()
			.with_cost(route.cost)
			.with_announce(route.announce);
		for id in route.hops {
			let origin = moq_net::Origin::new(id).map_err(|e| MoqError::InvalidRoute(e.to_string()))?;
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
pub struct MoqOriginDynamic {
	task: Task<OriginDynamic>,
}

#[derive(uniffi::Object)]
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
		loop {
			match self.inner.next().await {
				// Skip unannounce events; this surface only reports availability. A
				// replacement arrives as an unannounce/announce pair, so the caller
				// still sees a single announcement carrying the new broadcast.
				Some(moq_net::announce::Update { path, broadcast }) => {
					let Some(broadcast) = broadcast else {
						continue;
					};
					return Ok(Some(Arc::new(MoqAnnouncement {
						path: path.to_string(),
						broadcast: Arc::new(MoqBroadcastConsumer::new(broadcast)),
					})));
				}
				None => return Ok(None),
			}
		}
	}

	async fn available(&mut self) -> Result<Arc<MoqBroadcastConsumer>, MoqError> {
		loop {
			match self.inner.next().await {
				// Skip unannounce events; we're waiting for the broadcast to become available.
				Some(moq_net::announce::Update { broadcast, .. }) => match broadcast {
					Some(broadcast) => return Ok(Arc::new(MoqBroadcastConsumer::new(broadcast))),
					None => continue,
				},
				None => return Err(MoqError::Closed),
			}
		}
	}
}

/// A broadcast announcement from an origin.
#[derive(uniffi::Object)]
pub struct MoqAnnouncement {
	path: String,
	broadcast: Arc<MoqBroadcastConsumer>,
}

/// Waits for a specific broadcast to be announced.
#[derive(uniffi::Object)]
pub struct MoqAnnouncedBroadcast {
	task: Task<Announced>,
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
		let mut info = moq_net::origin::Info::new(moq_net::Origin::random());
		if let Some(capacity) = options.cache_capacity_bytes {
			info = info.with_pool(moq_net::cache::Pool::new(capacity));
		}

		Self { inner: info.produce() }
	}
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
		let shared = moq_net::Origin::random().produce();
		return (shared.clone(), shared);
	}

	let resolve = |origin: Option<&Arc<MoqOriginProducer>>| {
		origin
			.map(|o| o.inner().clone())
			.unwrap_or_else(|| moq_net::Origin::random().produce())
	};
	(resolve(publish), resolve(consume))
}

#[uniffi::export]
impl MoqOriginProducer {
	/// Create a new origin for publishing and/or consuming broadcasts.
	#[uniffi::constructor]
	pub fn new(options: MoqOriginOptions) -> Arc<Self> {
		let _guard = crate::ffi::RUNTIME.enter();
		Arc::new(Self::from_options(options))
	}

	/// Create a consumer for this origin.
	pub fn consume(&self) -> Arc<MoqOriginConsumer> {
		let _guard = crate::ffi::RUNTIME.enter();
		Arc::new(MoqOriginConsumer {
			inner: self.inner.consume(),
		})
	}

	/// Create a dynamic handler for serving unannounced broadcasts on request.
	///
	/// Hold the returned object while missing broadcast requests should be accepted.
	/// Dropping it makes future requests to unknown broadcasts fail.
	pub fn dynamic(&self) -> Arc<MoqOriginDynamic> {
		let _guard = crate::ffi::RUNTIME.enter();
		Arc::new(MoqOriginDynamic {
			task: Task::new(OriginDynamic {
				inner: self.inner.dynamic(),
			}),
		})
	}

	/// Create a broadcast at `path` on this origin, returning the producer that feeds it.
	///
	/// The broadcast starts announced: the origin advertises the path so subscribers can discover
	/// it, becoming visible shortly after this returns. Toggle discoverability with
	/// [`MoqBroadcastProducer::set_announce`]; an unannounced broadcast stays reachable by exact
	/// path for subscribes and fetches without being announced.
	///
	/// [`MoqBroadcastProducer::finish`] unpublishes immediately. Dropping the producer
	/// without finishing is treated as a failure: the path lingers briefly so a
	/// replacement publisher can take over without subscribers noticing.
	pub fn create_broadcast(&self, path: String) -> Result<Arc<MoqBroadcastProducer>, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		// Surfaces Error::Unauthorized (out of scope) via the MoqError::Protocol conversion.
		let broadcast = self
			.inner
			.create_broadcast(path.as_str(), moq_net::broadcast::Route::new().with_announce(true))?;
		Ok(Arc::new(MoqBroadcastProducer::from_inner(broadcast)?))
	}
}

#[uniffi::export]
impl MoqOriginConsumer {
	/// Subscribe to all broadcast announcements under a prefix.
	pub fn announced(&self, prefix: String) -> Result<Arc<MoqAnnounced>, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let origin = self.inner.with_root(prefix).ok_or(MoqError::Unauthorized)?;
		Ok(Arc::new(MoqAnnounced {
			task: Task::new(Announced {
				inner: origin.announced(),
			}),
		}))
	}

	/// Wait for a specific broadcast to be announced by path.
	pub fn announced_broadcast(&self, path: String) -> Result<Arc<MoqAnnouncedBroadcast>, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let origin = self.inner.with_root(path).ok_or(MoqError::Unauthorized)?;
		Ok(Arc::new(MoqAnnouncedBroadcast {
			task: Task::new(Announced {
				inner: origin.announced(),
			}),
		}))
	}

	/// Request a broadcast by path, resolving as soon as it can be served.
	///
	/// Returns the announced broadcast immediately if one exists; otherwise falls back to a
	/// dynamic handler on the origin (if any) and resolves once it serves the broadcast, or
	/// errors if nothing can serve it. Unlike `announced_broadcast`, this does *not* wait
	/// indefinitely for a future announcement: it resolves or fails based on what is
	/// announced now plus any dynamic fallback. Drop the returned future to cancel.
	pub async fn request_broadcast(&self, path: String) -> Result<Arc<MoqBroadcastConsumer>, MoqError> {
		let broadcast = self.inner.request_broadcast(path.as_str()).await?;
		Ok(Arc::new(MoqBroadcastConsumer::new(broadcast)))
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
		self.task
			.run(|mut state| async move { state.requested_broadcast().await })
			.await
	}

	/// Cancel all current and future `requested_broadcast()` calls.
	pub fn cancel(&self) {
		self.task.cancel();
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
		let _guard = crate::ffi::RUNTIME.enter();
		let consumer = broadcast.consume_inner()?;
		let request = self.take()?;
		request.accept(&consumer);
		Ok(())
	}

	/// Abort the request with an application error code.
	pub fn abort(&self, error_code: u16) -> Result<(), MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let request = self.take()?;
		request.reject(moq_net::Error::App(error_code));
		Ok(())
	}
}

// ---- MoqAnnounced ----

#[uniffi::export]
impl MoqAnnounced {
	/// Get the next broadcast announcement. Returns `None` when the origin is closed.
	///
	/// Use `broadcast.closed()` to learn when a broadcast is unannounced.
	pub async fn next(&self) -> Result<Option<Arc<MoqAnnouncement>>, MoqError> {
		self.task.run(|mut state| async move { state.next().await }).await
	}

	/// Cancel all current and future `next()` calls.
	pub fn cancel(&self) {
		self.task.cancel();
	}
}

#[uniffi::export]
impl MoqAnnouncement {
	/// The path of the announced broadcast.
	pub fn path(&self) -> String {
		self.path.clone()
	}

	/// The broadcast consumer.
	pub fn broadcast(&self) -> Arc<MoqBroadcastConsumer> {
		self.broadcast.clone()
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
	pub fn cancel(&self) {
		self.task.cancel();
	}
}
