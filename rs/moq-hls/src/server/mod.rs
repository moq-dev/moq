//! HTTP server: serves HLS and DASH for MoQ broadcasts, fetching media on demand.
//!
//! Routes are path-based, so one server can expose many broadcasts:
//!
//! ```text
//! GET /{broadcast}/master.m3u8
//! GET /{broadcast}/manifest.mpd
//! GET /{broadcast}/@{instance}/{kind}/{rendition}/media.m3u8
//! GET /{broadcast}/@{instance}/{kind}/{rendition}/init.{hash}.mp4
//! GET /{broadcast}/@{instance}/{kind}/{rendition}/seg/{tag}.{segment}.m4s
//! GET /{broadcast}/@{instance}/{kind}/{rendition}/seg/{tag}.t{pts}.m4s
//! ```
//!
//! The master playlist and manifest list the publisher instance serving the broadcast now,
//! and every URL they list is under that `@{instance}`: its epoch, or an id the server mints
//! for an epochless instance. A publisher that restarts gets its own URLs, so a playlist URL
//! never resets its media sequence. A replaced instance keeps serving its URLs while its
//! broadcast stays open, so a player already on it keeps playing until its publisher goes,
//! then reloads the master to reach the replacement. A replaced instance watches its catalog
//! and records only while requests reach it: once a playlist window passes without one, it
//! stops, and the next request starts over under the same URLs.
//!
//! `{kind}` is `video` or `audio`, so a video and an audio rendition that share a
//! name address distinct resources. The two `seg/` forms name the same bytes: HLS
//! playlists address a segment by its number, the DASH manifest by its
//! `pts` in milliseconds (`$Time$`). `{hash}` is a hash of the init segment's bytes, so a
//! reconfigured rendition gets a new init URL. A broadcaster given a generation
//! ([`Broadcaster::set_generation`](crate::export::Broadcaster::set_generation))
//! prefixes every segment file with it (`seg/{generation}.{tag}.{segment}.m4s`), and
//! refuses segment URLs carrying any other.
//!
//! Every request is served. To gate access, wrap [`Server::router`] in your own
//! [`axum`] middleware. It runs before routing, so a rejected request never reaches
//! the origin, but it also sees the raw request URI rather than the path segments
//! the server decodes. A broadcast occupies every segment before the instance (or the
//! master and manifest), still percent-encoded: `/project/li%76e/master.m3u8` serves the
//! broadcast `project/live`. Decode each segment before matching it against a policy, or a
//! name can be encoded past the check.
//!
//! An embedder that authorizes each request itself (a token scoping the broadcast, a
//! stats-metered origin per tenant) skips the router instead: parse the path with
//! [`Route::parse`](crate::export::Route::parse), check
//! [`Route::broadcast`](crate::export::Route::broadcast), rewrite it relative to the scope, and
//! answer with [`Server::respond`] on a `Server` built from that scope's origin. The
//! decoded broadcast is what a policy should check.
//!
//! ```no_run
//! use axum::http::StatusCode;
//! use axum::middleware::{self, Next};
//! use axum::extract::Request;
//! use axum::response::Response;
//!
//! async fn gate(req: Request, next: Next) -> Result<Response, StatusCode> {
//!     match req.headers().get("authorization") {
//!         Some(token) if token == "secret" => Ok(next.run(req).await),
//!         _ => Err(StatusCode::UNAUTHORIZED),
//!     }
//! }
//!
//! # fn build(server: moq_hls::Server) -> axum::Router {
//! server.router().layer(middleware::from_fn(gate))
//! # }
//! ```

mod routes;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::task::Poll;
use std::time::Duration;

use axum::Router;
use moq_mux::follow;

use crate::export::{Broadcaster, Config};

/// How long to wait for the route serving a requested broadcast to answer: a covering
/// dynamic route may never do so.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);

/// HLS export HTTP server. Cheap to clone (shared inner).
#[derive(Clone)]
pub struct Server {
	inner: Arc<Inner>,
}

struct Inner {
	origin: moq_net::origin::Consumer,
	/// Watches the whole scope while the server lives, so a lazily discovering client
	/// session asks its peer once rather than once per requested path, which an HTTP
	/// client could otherwise name without bound.
	_watch: moq_net::announce::Consumer,
	config: Config,
	/// Each requested broadcast, followed until nothing serves its path.
	broadcasts: Mutex<HashMap<String, Arc<Followed>>>,
}

/// The publisher instances serving one broadcast path.
struct Followed {
	instances: Mutex<Instances>,
	/// Follows the path. Aborted with the last handle, so a dropped server stops following.
	task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

struct Instances {
	/// The instance the master playlist and manifest list: the latest to take over.
	current: Arc<Instance>,
	/// Every instance a listed URL still names, the current one included. A replaced instance
	/// stays until its broadcast closes, so a player already on it keeps playing.
	by_id: HashMap<Arc<str>, Arc<Instance>>,
}

impl Followed {
	fn new(current: Arc<Instance>) -> Self {
		let by_id = HashMap::from([(current.id.clone(), current.clone())]);
		Self {
			instances: Mutex::new(Instances { current, by_id }),
			task: Mutex::new(None),
		}
	}

	fn current(&self) -> Arc<Instance> {
		self.instances.lock().unwrap().current.clone()
	}

	fn instance(&self, id: &str) -> Option<Arc<Instance>> {
		self.instances.lock().unwrap().by_id.get(id).cloned()
	}

	/// List `instance` from now on, retaining the one it replaces. An instance returning after a
	/// gap keeps its epoch, and so its id, which then names the new instance.
	fn start(&self, instance: Arc<Instance>) {
		let mut instances = self.instances.lock().unwrap();
		instances.by_id.insert(instance.id.clone(), instance.clone());
		std::mem::replace(&mut instances.current, instance).retire();
	}

	/// Keep serving every instance of `replaced` still open, unless this one lists its id.
	fn adopt(&self, replaced: &Followed) {
		let replaced = replaced.instances.lock().unwrap();
		let mut instances = self.instances.lock().unwrap();
		for (id, instance) in &replaced.by_id {
			if !instance.is_closed() {
				instances.by_id.entry(id.clone()).or_insert_with(|| {
					instance.retire();
					instance.clone()
				});
			}
		}
	}

	/// Forget every instance whose broadcast closed, registering `waiter` on the rest.
	fn forget_closed(&self, waiter: &kio::Waiter) {
		let mut instances = self.instances.lock().unwrap();
		instances
			.by_id
			.retain(|_, instance| instance.broadcast.poll_closed(waiter).is_pending());
	}
}

impl Drop for Followed {
	fn drop(&mut self) {
		if let Some(task) = self.task.lock().unwrap().take() {
			task.abort();
		}
	}
}

/// One publisher instance, which the URLs under `@{id}` name.
///
/// The current instance keeps its [`Broadcaster`], and so its catalog and record watchers, while
/// it is listed. A replaced one is retained only for players already on it, so it watches only
/// while read: a request starts a broadcaster, which stops once no request has reached it for a
/// playlist window, and the next request starts another under the same URLs.
struct Instance {
	id: Arc<str>,
	/// What every broadcaster for this instance reads, so a rebuilt one binds the same sibling
	/// broadcasts its predecessor's rows were listed against.
	source: moq_mux::Source,
	broadcast: moq_net::broadcast::Consumer,
	config: Config,
	watch: Mutex<Watch>,
}

#[derive(Default)]
struct Watch {
	/// Serving this instance: held while it is current, then until its reads go idle.
	broadcaster: Option<Arc<Broadcaster>>,
	/// The latest read since the instance was replaced, or `None` while it is current.
	read: Option<moq_clock::Instant>,
	/// Releases the broadcaster once reads go idle; running only while a replaced instance is
	/// watched.
	release: Option<tokio::task::JoinHandle<()>>,
}

impl Instance {
	/// The instance `source`'s broadcast serves, watched from now on as the current one.
	fn current(source: moq_mux::Source, config: Config) -> Arc<Self> {
		let broadcaster = Broadcaster::new(source.clone(), config.clone());
		Arc::new(Self {
			id: broadcaster.instance().clone(),
			broadcast: source.broadcast(),
			source,
			config,
			watch: Mutex::new(Watch {
				broadcaster: Some(broadcaster),
				..Default::default()
			}),
		})
	}

	fn is_closed(&self) -> bool {
		self.broadcast.is_closed()
	}

	/// The broadcaster serving this instance, started if it was idle, and kept a while longer if
	/// it is a replaced one.
	fn broadcaster(self: &Arc<Self>) -> Arc<Broadcaster> {
		let mut watch = self.watch.lock().unwrap();
		let broadcaster = watch
			.broadcaster
			.get_or_insert_with(|| Broadcaster::with_instance(self.source.clone(), self.config.clone(), self.id.clone()))
			.clone();
		if watch.read.is_some() {
			self.read(&mut watch);
		}
		broadcaster
	}

	/// Stop holding the broadcaster for listing, and keep it only while read. A player already on
	/// this instance reads it again within a window, so its broadcaster carries on warm.
	fn retire(self: &Arc<Self>) {
		let mut watch = self.watch.lock().unwrap();
		if watch.read.is_none() {
			self.read(&mut watch);
		}
	}

	/// Note a read now, and release the broadcaster once reads go idle.
	fn read(self: &Arc<Self>, watch: &mut Watch) {
		watch.read = Some(moq_clock::now());
		if watch.release.is_none() && watch.broadcaster.is_some() {
			watch.release = Some(tokio::spawn(release_idle(Arc::downgrade(self), self.config.window)));
		}
	}
}

impl Drop for Instance {
	fn drop(&mut self) {
		if let Some(release) = self.watch.get_mut().unwrap().release.take() {
			release.abort();
		}
	}
}

/// Drop `instance`'s broadcaster once nothing has read it for `idle`, stopping its watchers unless
/// a response still holds it. Holds the instance weakly, so it never keeps it retained.
async fn release_idle(instance: Weak<Instance>, idle: Duration) {
	loop {
		let deadline = {
			let Some(instance) = instance.upgrade() else { return };
			let watch = instance.watch.lock().unwrap();
			let Some(read) = watch.read else { return };
			read + idle
		};
		moq_clock::sleep_until(deadline).await;

		let Some(instance) = instance.upgrade() else { return };
		let mut watch = instance.watch.lock().unwrap();
		if watch.read.is_some_and(|read| read + idle <= moq_clock::now()) {
			watch.release = None;
			let broadcaster = watch.broadcaster.take();
			drop(watch);
			drop(broadcaster);
			return;
		}
	}
}

impl Server {
	/// Build a server reading broadcasts from `origin`, which it watches whole while it
	/// lives. Every request is served; gate access by layering middleware onto
	/// [`router`](Self::router).
	pub fn new(origin: moq_net::origin::Consumer, config: Config) -> Self {
		Self {
			inner: Arc::new(Inner {
				_watch: origin.clone().with_hidden(true).announced(),
				origin,
				config,
				broadcasts: Mutex::new(HashMap::new()),
			}),
		}
	}

	/// The axum router for the HLS endpoints, ready to nest or wrap in middleware.
	pub fn router(&self) -> Router {
		routes::router(self.clone())
	}

	/// The [`Broadcaster`] for the publisher instance serving `name` now, following each
	/// instance that takes over. Returns `None` if the broadcast is not announced once the
	/// origin has synced with the relay.
	pub async fn broadcaster(&self, name: &str) -> Option<Arc<Broadcaster>> {
		Some(self.followed(name).await?.current().broadcaster())
	}

	/// The [`Broadcaster`] for the publisher instance `id` of `name`, until its broadcast
	/// closes. A replaced instance starts watching again if it was idle.
	pub(crate) async fn instance(&self, name: &str, id: &str) -> Option<Arc<Broadcaster>> {
		Some(self.followed(name).await?.instance(id)?.broadcaster())
	}

	/// Follow `name` from the instance serving it now, unless it is followed already.
	async fn followed(&self, name: &str) -> Option<Arc<Followed>> {
		// Once the listed instance closes, its follower is about to move on or forget the path, so
		// resolve whatever serves the path now instead.
		if let Some(followed) = self.inner.broadcasts.lock().unwrap().get(name)
			&& !followed.current().is_closed()
		{
			return Some(followed.clone());
		}

		let instances = follow::Instances::new(&self.inner.origin, name, None)
			.map_err(|err| tracing::warn!(%err, %name, "failed to follow broadcast"))
			.ok()?;
		// A request answers once the origin has synced with the relay, so a path with no route
		// fails now rather than waiting out a publisher that has gone. The source resolves a
		// rendition's sibling `broadcast` reference through the same origin.
		let broadcast = match moq_clock::timeout(RESOLVE_TIMEOUT, self.inner.origin.request(name, None)).await {
			Ok(Ok(broadcast)) => broadcast,
			Ok(Err(moq_net::Error::Unroutable)) => return None,
			Ok(Err(err)) => {
				tracing::warn!(%err, %name, "failed to resolve broadcast");
				return None;
			}
			Err(_) => {
				tracing::warn!(%name, "timed out resolving broadcast");
				return None;
			}
		};
		let followed = Arc::new(Followed::new(self.inner.serve(broadcast.clone())));

		{
			let mut broadcasts = self.inner.broadcasts.lock().unwrap();
			if let Some(existing) = broadcasts.get(name) {
				if !existing.current().is_closed() {
					return Some(existing.clone());
				}
				// A player may still be on a replaced instance, and an epochless one would not get
				// its id back.
				followed.adopt(existing);
			}
			broadcasts.insert(name.to_string(), followed.clone());
		}

		let task = tokio::spawn(run_follow(
			Arc::downgrade(&self.inner),
			name.to_string(),
			Arc::downgrade(&followed),
			instances,
			broadcast,
		));
		*followed.task.lock().unwrap() = Some(task);
		Some(followed)
	}
}

impl Inner {
	/// The instance `broadcast`, watched as the current one.
	fn serve(&self, broadcast: moq_net::broadcast::Consumer) -> Arc<Instance> {
		let source = moq_mux::Source::new(self.origin.consume(), broadcast);
		Instance::current(source, self.config.clone())
	}
}

/// Follow `name`'s instances into `followed` until nothing serves the path, then forget it, so the
/// next request follows the path afresh. Holds both weakly, so it never keeps the server's state
/// alive; dropping `followed` aborts it. `served` is the instance `followed` starts on, which the
/// follower hands out first unless another took over since, or folds away if it closed first.
async fn run_follow(
	inner: Weak<Inner>,
	name: String,
	followed: Weak<Followed>,
	mut instances: follow::Instances,
	served: moq_net::broadcast::Consumer,
) {
	let mut served = Some(served);
	let (inner, followed) = loop {
		let next = kio::wait(|waiter| {
			let Some(followed) = followed.upgrade() else {
				return Poll::Ready(None);
			};
			followed.forget_closed(waiter);
			if let Poll::Ready(next) = instances.poll_next(waiter) {
				return Poll::Ready(Some(next));
			}
			// The follower folds away an instance that started and ended before its first poll,
			// so the served instance closing before it is handed out means nothing serves the path.
			match &served {
				Some(served) => served.poll_closed(waiter).map(|()| Some(Ok(follow::Event::End))),
				None => Poll::Pending,
			}
		})
		.await;
		let (Some(inner), Some(followed)) = (inner.upgrade(), followed.upgrade()) else {
			return;
		};
		match next {
			Some(Ok(follow::Event::Start(broadcast))) => {
				if !served.take().is_some_and(|served| served.is_clone(&broadcast)) {
					followed.start(inner.serve(broadcast));
				}
			}
			Some(Ok(follow::Event::End)) => break (inner, followed),
			Some(Err(err)) => {
				tracing::warn!(%err, %name, "stopped following broadcast");
				break (inner, followed);
			}
			None => return,
		}
	};

	let mut broadcasts = inner.broadcasts.lock().unwrap();
	if broadcasts
		.get(&name)
		.is_some_and(|current| Arc::ptr_eq(current, &followed))
	{
		broadcasts.remove(&name);
	}
}

#[cfg(test)]
mod tests {
	/// Build an origin producer, spawning its driver on the ambient runtime.
	fn produce_origin() -> moq_net::origin::Producer {
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

	use super::*;

	/// Let the origin's spawned attach/detach tasks run: a created broadcast
	/// becomes routable (and a finished one unroutable) asynchronously.
	async fn settle() {
		for _ in 0..10 {
			tokio::task::yield_now().await;
		}
	}

	/// Mirrors the middleware example in this module's docs, which `doctest = false`
	/// keeps out of the compiler's reach.
	#[tokio::test]
	async fn router_middleware_gates_requests() {
		use axum::body::Body;
		use axum::extract::Request;
		use axum::http::{StatusCode, header};
		use axum::middleware::{self, Next};
		use axum::response::Response;
		use tower::ServiceExt;

		async fn gate(req: Request, next: Next) -> Result<Response, StatusCode> {
			match req.headers().get(header::AUTHORIZATION) {
				Some(token) if token == "secret" => Ok(next.run(req).await),
				_ => Err(StatusCode::UNAUTHORIZED),
			}
		}

		// An origin with no broadcasts: a request that reaches the handlers 404s, so a 401
		// proves the middleware rejected it first.
		let origin = produce_origin();
		let server = Server::new(origin.consume(), Config::default());
		let app = server.router().layer(middleware::from_fn(gate));

		let denied = app
			.clone()
			.oneshot(Request::builder().uri("/live/master.m3u8").body(Body::empty()).unwrap())
			.await
			.unwrap();
		assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

		let allowed = app
			.oneshot(
				Request::builder()
					.uri("/live/master.m3u8")
					.header(header::AUTHORIZATION, "secret")
					.body(Body::empty())
					.unwrap(),
			)
			.await
			.unwrap();
		assert_eq!(allowed.status(), StatusCode::NOT_FOUND);
		// Pin the 404 to our handler: an unmatched route would also 404, but without
		// the no-store that not_found() sets, so a broken URI can't fake this pass.
		assert_eq!(allowed.headers()[header::CACHE_CONTROL], "no-store");
	}

	#[tokio::test]
	async fn router_matches_multisegment_broadcast_when_nested() {
		use axum::body::Body;
		use axum::extract::Request;
		use axum::http::{StatusCode, header};
		use tower::ServiceExt;

		let origin = produce_origin();
		let server = Server::new(origin.consume(), Config::default());
		let app = Router::new().nest("/hls", server.router());

		let response = app
			.oneshot(
				Request::builder()
					.uri("/hls/project/live/master.m3u8")
					.body(Body::empty())
					.unwrap(),
			)
			.await
			.unwrap();

		assert_eq!(response.status(), StatusCode::NOT_FOUND);
		// The handler adds no-store. Axum's unmatched-route 404 does not.
		assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
	}

	/// A synced origin with no route for the path answers at once, rather than waiting out
	/// RESOLVE_TIMEOUT for a publisher that is not there. Paused time would skip that wait,
	/// so any elapsed time means it happened.
	#[tokio::test(start_paused = true)]
	async fn unannounced_broadcast_answers_without_waiting() {
		let origin = produce_origin();
		let server = Server::new(origin.consume(), Config::default());

		let start = moq_clock::now();
		assert!(server.broadcaster("live").await.is_none());
		assert_eq!(start.elapsed(), Duration::ZERO);
	}

	async fn closed_instance() -> Arc<Instance> {
		let origin = produce_origin();
		let producer = origin.create_broadcast("gone").expect("publish allowed");
		producer.announce(Default::default()).expect("publish allowed");
		settle().await;
		let instance = Instance::current(crate::resolved(&origin, "gone").await, Config::default());
		// Close the publisher so the resolved broadcast (and the instance) reports closed.
		producer.close();
		settle().await;
		instance
	}

	#[tokio::test]
	async fn broadcaster_replaces_finished_cached_instance() {
		let origin = produce_origin();
		let server = Server::new(origin.consume(), Config::default());
		let stale = closed_instance().await;

		server
			.inner
			.broadcasts
			.lock()
			.unwrap()
			.insert("live".to_string(), Arc::new(Followed::new(stale.clone())));
		let _producer = origin.create_broadcast("live").expect("publish allowed");
		_producer.announce(Default::default()).expect("publish allowed");
		settle().await;

		let fresh = server.broadcaster("live").await.expect("broadcast announced");

		assert!(!Arc::ptr_eq(&stale.broadcaster(), &fresh));
		assert!(server.inner.broadcasts.lock().unwrap().contains_key("live"));
	}

	/// Dropping the last server clone releases its broadcasters while their publisher stays
	/// live, every time a server is replaced, but a broadcaster a response still holds keeps
	/// serving.
	#[tokio::test]
	async fn dropping_the_server_releases_its_broadcasters() {
		let origin = produce_origin();
		let producer = origin.create_broadcast("live").expect("publish allowed");
		producer.announce(Default::default()).expect("publish allowed");
		settle().await;

		for _ in 0..4 {
			let server = Server::new(origin.consume(), Config::default());
			let broadcaster = server.broadcaster("live").await.expect("broadcast announced");
			let released = Arc::downgrade(&broadcaster);
			drop(broadcaster);
			drop(server);
			settle().await;
			assert_eq!(released.strong_count(), 0, "the dropped server kept its broadcaster");
		}

		let server = Server::new(origin.consume(), Config::default());
		let held = server.broadcaster("live").await.expect("broadcast announced");
		drop(server);
		settle().await;
		assert_eq!(Arc::strong_count(&held), 1, "only the response holds it now");
		assert!(!held.is_closed(), "a held broadcaster keeps serving");
		producer.close();
	}

	/// Wait out the origin's update hold until `ready` holds.
	async fn eventually(mut ready: impl AsyncFnMut() -> bool) {
		for _ in 0..100 {
			if ready().await {
				return;
			}
			moq_clock::sleep(Duration::from_millis(100)).await;
		}
		panic!("never happened");
	}

	/// A replacement becomes the instance the entry points list, under its own id, while the old
	/// instance keeps serving its URLs until its broadcast closes. Once nothing serves the path,
	/// the server forgets it.
	#[tokio::test(start_paused = true)]
	async fn a_replaced_instance_serves_until_it_closes() {
		let origin = produce_origin();
		let server = Server::new(origin.consume(), Config::default());
		let (old_epoch, new_epoch) = (moq_net::Epoch::mint(), moq_net::Epoch::mint());
		let route = |epoch: &moq_net::Epoch| moq_net::origin::Route::default().with_epoch(epoch.clone());

		let old = origin.publish("live", route(&old_epoch)).expect("publish allowed");
		settle().await;
		let first = server.broadcaster("live").await.expect("broadcast announced");
		assert_eq!(first.instance().as_ref(), old_epoch.as_str());

		let new = origin.publish("live", route(&new_epoch)).expect("publish allowed");
		eventually(async || {
			let current = server.broadcaster("live").await.expect("broadcast announced");
			current.instance().as_ref() == new_epoch.as_str()
		})
		.await;
		let kept = server.instance("live", old_epoch.as_str()).await;
		assert!(
			kept.is_some_and(|kept| Arc::ptr_eq(&kept, &first)),
			"the old instance serves"
		);

		drop(old);
		eventually(async || server.instance("live", old_epoch.as_str()).await.is_none()).await;
		assert!(server.instance("live", new_epoch.as_str()).await.is_some());

		drop(new);
		eventually(async || server.inner.broadcasts.lock().unwrap().is_empty()).await;
	}

	/// An instance that goes before the follower first runs is folded away by it, so the served
	/// instance closing is what tells the server that nothing serves the path.
	#[tokio::test(start_paused = true)]
	async fn an_instance_gone_before_the_follower_runs_is_forgotten() {
		let origin = produce_origin();
		let server = Server::new(origin.consume(), Config::default());
		let publication = origin
			.publish("live", moq_net::origin::Route::default())
			.expect("publish allowed");
		settle().await;

		// Nothing yields between spawning the follower and returning, so it has not run yet.
		let broadcaster = server.broadcaster("live").await.expect("broadcast announced");
		drop(publication);
		eventually(async || server.inner.broadcasts.lock().unwrap().is_empty()).await;
		assert!(broadcaster.is_closed());
	}

	/// A request that finds the listed instance closed before the follower does resolves the path
	/// afresh, yet a replaced instance still open keeps its URLs. An epochless one gets a new id
	/// when resolved again, so its old URLs would never serve otherwise.
	#[tokio::test]
	async fn a_closed_current_instance_keeps_the_open_ones() {
		let origin = produce_origin();
		let server = Server::new(origin.consume(), Config::default());
		let _old = origin
			.publish("live", moq_net::origin::Route::default())
			.expect("publish allowed");
		settle().await;
		let open = Instance::current(crate::resolved(&origin, "live").await, Config::default());

		// The follower has not yet seen the takeover instance close.
		let followed = Followed::new(open.clone());
		followed.start(closed_instance().await);
		server
			.inner
			.broadcasts
			.lock()
			.unwrap()
			.insert("live".to_string(), Arc::new(followed));

		let current = server.broadcaster("live").await.expect("broadcast announced");
		assert!(!current.is_closed());
		let kept = server.instance("live", &open.id).await;
		assert!(
			kept.is_some_and(|kept| Arc::ptr_eq(&kept, &open.broadcaster())),
			"the open instance lost its URLs"
		);
	}

	/// Whether a replaced `instance` holds a broadcaster, and so its watchers. It runs the task
	/// that releases one exactly while it does.
	fn watching(instance: &Instance) -> bool {
		let watch = instance.watch.lock().unwrap();
		assert_eq!(watch.broadcaster.is_some(), watch.release.is_some());
		watch.broadcaster.is_some()
	}

	/// A replaced instance watches only while read: its warm broadcaster stops once a window
	/// passes without a request, a later request starts another under the same id, which reads
	/// keep going, and which stops again once they stop.
	#[tokio::test(start_paused = true)]
	async fn a_replaced_instance_watches_only_while_read() {
		let origin = produce_origin();
		let config = Config::default();
		let server = Server::new(origin.consume(), config.clone());
		let (old_epoch, new_epoch) = (moq_net::Epoch::mint(), moq_net::Epoch::mint());
		let route = |epoch: &moq_net::Epoch| moq_net::origin::Route::default().with_epoch(epoch.clone());

		let _old = origin.publish("live", route(&old_epoch)).expect("publish allowed");
		settle().await;
		let first = Arc::downgrade(&server.broadcaster("live").await.expect("broadcast announced"));
		let _new = origin.publish("live", route(&new_epoch)).expect("publish allowed");
		eventually(async || {
			let current = server.broadcaster("live").await.expect("broadcast announced");
			current.instance().as_ref() == new_epoch.as_str()
		})
		.await;
		let followed = server.followed("live").await.expect("followed");
		let old = followed.instance(old_epoch.as_str()).expect("the old instance is retained");
		assert!(watching(&old), "a player on the replaced instance keeps it warm");

		moq_clock::sleep(config.window).await;
		settle().await;
		assert!(!watching(&old), "an unread replaced instance holds no watchers");
		assert_eq!(first.strong_count(), 0, "its broadcaster is gone");

		let restarted = server.instance("live", old_epoch.as_str()).await.expect("still served");
		assert_eq!(restarted.instance().as_ref(), old_epoch.as_str());
		assert!(watching(&old), "a request restarts it");
		let restarted = Arc::downgrade(&restarted);
		for _ in 0..4 {
			moq_clock::sleep(config.window / 2).await;
			let read = server.instance("live", old_epoch.as_str()).await.expect("still served");
			assert!(
				Arc::ptr_eq(&read, &restarted.upgrade().expect("held")),
				"reads keep the one broadcaster"
			);
		}

		moq_clock::sleep(config.window).await;
		settle().await;
		assert!(!watching(&old), "it stops again once its readers go idle");
		assert_eq!(restarted.strong_count(), 0);
	}

	/// A rebuilt broadcaster keeps the id an epochless instance's URLs were listed under.
	#[tokio::test(start_paused = true)]
	async fn an_epochless_instance_restarts_under_its_id() {
		let origin = produce_origin();
		let _publication = origin
			.publish("live", moq_net::origin::Route::default())
			.expect("publish allowed");
		settle().await;
		let config = Config::default();
		let instance = Instance::current(crate::resolved(&origin, "live").await, config.clone());
		instance.retire();
		moq_clock::sleep(config.window).await;
		settle().await;
		assert!(!watching(&instance));

		assert_eq!(instance.broadcaster().instance(), &instance.id);
	}
}
