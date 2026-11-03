use std::task::{Poll, ready};

use super::WeakEntry;
use super::origin_impl::{Announce, Consumer, Moved, Requesting, RouteEvent, Routes};
use crate::{Error, Instance, Path, broadcast};

/// The announcements covering one path, as one broadcast coming online, being replaced, and
/// going offline. Created by [`Consumer::routes`].
///
/// Several routes can cover the path at once, such as the path itself and a prefix above it,
/// and a request resolves through the most specific one, so only that route's events come
/// through. Another route taking over is a [`Restart`](RouteEvent::Restart), unless both
/// carry the same instance: those serve one continuous stream, so it is an
/// [`Update`](RouteEvent::Update). One announced as a
/// [`Takeover`](RouteEvent::Takeover) of the serving route continues it, and comes through
/// as one. Routes beneath the path serve other broadcasts and are ignored.
pub(crate) struct Follow {
	announced: Routes,
	path: Path,
	/// Every route standing over the path.
	covering: Vec<Standing>,
	/// How many events the cursor has delivered.
	taken: u64,
	/// What `taken` was when the cursor last ran dry, so held no end.
	dry: u64,
	/// Nothing has been reported serving the path since the last end.
	idle: bool,
}

/// A route standing over the followed path.
///
/// The cursor delivers by prefix, not in order, so a route's end may have happened before
/// another prefix's start that it delivers first. Taking an event for a prefix proves no end
/// was queued there yet, so any route that started before that take stood before its end.
struct Standing {
	announce: Announce,
	/// When its start was taken.
	since: u64,
	/// When its latest event was taken.
	seen: u64,
}

impl Follow {
	pub(super) fn new(announced: Routes, path: Path) -> Self {
		Self {
			announced,
			path,
			covering: Vec::new(),
			taken: 0,
			dry: 0,
			idle: true,
		}
	}

	/// The next change to the route serving the path, or `None` once the origin closes.
	#[cfg(all(test, feature = "async"))]
	pub async fn next(&mut self) -> Option<RouteEvent> {
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

	/// Poll for the next change, registering `waiter` when there is none yet.
	///
	/// While nothing serves the path, every announcement already on hand is folded into one
	/// start, so the replay a late follower begins with (a covering prefix, then the exact path
	/// beneath it) starts on the route serving the path rather than starting and restarting.
	/// Once something serves it, each change comes through on its own: an end followed by a
	/// start is a gap that ended any request on the old route, even when both routes carry one
	/// instance. A route whose start came after the serving route's last event, with the cursor
	/// never running dry in between, may have arrived after its end, so taking over from it is
	/// reported as that gap.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Option<RouteEvent>> {
		if self.idle {
			let ended = loop {
				match self.poll_announced(waiter) {
					Poll::Ready(Some(event)) => {
						self.fold(event);
					}
					Poll::Ready(None) => break true,
					Poll::Pending => break false,
				}
			};
			return match (self.serving().map(|standing| standing.announce.clone()), ended) {
				(Some(serving), _) => {
					self.idle = false;
					Poll::Ready(Some(RouteEvent::Start(serving)))
				}
				(None, true) => Poll::Ready(None),
				(None, false) => Poll::Pending,
			};
		}

		loop {
			let Some(event) = ready!(self.poll_announced(waiter)) else {
				return Poll::Ready(None);
			};
			if let Some(event) = self.fold(event) {
				self.idle = matches!(event, RouteEvent::End(_));
				return Poll::Ready(Some(event));
			}
		}
	}

	/// Poll until every source feeding this follower has synced, registering `waiter` until
	/// then. A path [`poll_next`](Self::poll_next) has not started by then is not announced,
	/// rather than not heard yet. See [`Routes::poll_synced`].
	pub fn poll_synced(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.announced.poll_synced(waiter)
	}

	/// The cursor's next event, counting what it delivers and when it runs dry.
	fn poll_announced(&mut self, waiter: &kio::Waiter) -> Poll<Option<RouteEvent>> {
		let poll = self.announced.poll_next(waiter);
		match poll {
			Poll::Ready(Some(_)) => self.taken += 1,
			Poll::Pending => self.dry = self.taken,
			Poll::Ready(None) => {}
		}
		poll
	}

	/// The most specific route covering the path, which is the one a request resolves.
	fn serving(&self) -> Option<&Standing> {
		self.covering
			.iter()
			.max_by_key(|standing| standing.announce.prefix.as_str().len())
	}

	/// Apply one route's event, returning what it means for the path, if anything.
	fn fold(&mut self, event: RouteEvent) -> Option<RouteEvent> {
		let (RouteEvent::Start(announce)
		| RouteEvent::Update(announce)
		| RouteEvent::Restart(announce)
		| RouteEvent::Takeover { announce, .. }
		| RouteEvent::End(announce)) = &event;
		if !self.path.has_prefix(&announce.prefix) {
			return None;
		}
		let prefix = announce.prefix.clone();
		let before = self
			.serving()
			.map(|standing| (standing.announce.clone(), standing.seen));

		let standing = self
			.covering
			.iter()
			.position(|standing| standing.announce.prefix == prefix)
			.map(|index| self.covering.swap_remove(index).since);
		let (announce, since, restart, predecessor) = match event {
			RouteEvent::End(_) => (None, None, false, None),
			RouteEvent::Start(announce) => (Some(announce), None, false, None),
			RouteEvent::Update(announce) => (Some(announce), standing, false, None),
			RouteEvent::Restart(announce) => (Some(announce), standing, true, None),
			RouteEvent::Takeover { announce, predecessor } => (Some(announce), None, true, Some(predecessor)),
		};
		if let Some(announce) = announce {
			self.covering.push(Standing {
				announce,
				since: since.unwrap_or(self.taken),
				seen: self.taken,
			});
		}

		let after = self
			.serving()
			.map(|standing| (standing.announce.clone(), standing.since));
		match (before, after) {
			(None, None) => None,
			(None, Some((after, _))) => Some(RouteEvent::Start(after)),
			(Some((before, _)), None) => Some(RouteEvent::End(before)),
			// The serving route ended, and the one left may have started after it did.
			(Some((before, seen)), Some((after, since)))
				if before.prefix == prefix && before.prefix != after.prefix && since > self.dry.max(seen) =>
			{
				Some(RouteEvent::End(before))
			}
			// The serving route continues as this one.
			(Some((before, _)), Some((after, _)))
				if after.prefix == prefix && predecessor.as_ref() == Some(&before.prefix) =>
			{
				Some(RouteEvent::Takeover {
					announce: after,
					predecessor: before.prefix,
				})
			}
			(Some((before, _)), Some((after, _))) if before.prefix != after.prefix => {
				let same = before.route.instance == after.route.instance;
				Some(match same {
					true => RouteEvent::Update(after),
					false => RouteEvent::Restart(after),
				})
			}
			// A route less specific than the serving one changed, which no request sees.
			(Some(_), Some((after, _))) if after.prefix != prefix => None,
			(Some(_), Some((after, _))) if restart => Some(RouteEvent::Restart(after)),
			(Some(_), Some((after, _))) => Some(RouteEvent::Update(after)),
		}
	}
}

/// Each instance serving one path, handed out as it takes over. Created by
/// [`Consumer::follow`](crate::origin::Consumer::follow).
pub struct Following {
	consumer: Consumer,
	path: Path,
	/// The only instance handed out, if pinned.
	instance: Option<Instance>,
	follow: Follow,
	/// An instance serves the path that was not handed out yet.
	unseen: bool,
	/// The resolution in flight, kept across a dropped wait so a retry picks it up.
	resolving: Option<Requesting>,
	/// The broadcast handed out last, until the path goes offline or the broadcast ends. Weak,
	/// so it never keeps one the caller let go of from retiring.
	last: Option<broadcast::WeakConsumer>,
	/// Once the broadcast handed out last closed, waits for its instance to stop serving the
	/// path.
	moved: Option<Moved>,
	/// A change resolved to the broadcast handed out last while it was still open, so it counts
	/// as delivered only until that broadcast closes.
	absorbed: bool,
	/// A broadcast was handed out and its end not reported yet.
	live: bool,
	/// The broadcast handed out last stopped serving the path, and its end is owed.
	ended: bool,
}

impl Following {
	pub(super) fn new(consumer: Consumer, path: Path, instance: Option<Instance>, follow: Follow) -> Self {
		Self {
			consumer,
			path,
			instance,
			follow,
			unseen: false,
			resolving: None,
			last: None,
			moved: None,
			absorbed: false,
			live: false,
			ended: false,
		}
	}

	/// Wait for the next change to the instance serving the path; see
	/// [`Consumer::follow`](crate::origin::Consumer::follow).
	///
	/// Returns the broadcast of each instance as it comes online or takes over, and `None`
	/// once the path the one handed out last served goes offline, or, when pinned, another
	/// instance takes it over. Returns [`Error::Closed`] once the origin closes, and any other
	/// resolution failure as-is, such as a handler refusing the path; the next call then waits
	/// for another instance to serve it. Dropping the future keeps the wait's progress for the
	/// next call.
	#[cfg(feature = "async")]
	pub async fn available(&mut self) -> Result<Option<broadcast::Consumer>, Error> {
		kio::wait(|waiter| self.poll_available(waiter)).await
	}

	/// Poll for the next change to the instance serving the path, registering `waiter` when
	/// there is none yet; see [`Self::available`].
	pub fn poll_available(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<broadcast::Consumer>, Error>> {
		loop {
			// Apply every change on hand before reading an answer, so a burst of them hands out
			// only the instance serving the path now. A resolution in flight answers for the
			// routes before the change, such as an end and a start for the instance it resolved,
			// so it starts over.
			if self.settle(waiter)? {
				self.resolving = None;
			}
			// Report an end before whatever serves next, so a reader drops the old broadcast first.
			if self.ended {
				self.ended = false;
				self.live = false;
				return Poll::Ready(Ok(None));
			}
			// The broadcast handed out ended while the path is still served: its instance's
			// routes may have gone, and another instance's, beneath the same announcement,
			// taken the path over. It also ends once the caller lets go of it, so only its
			// instance no longer serving the path makes whatever serves it now next, unless a
			// change it absorbed is owed.
			if !self.unseen
				&& self.follow.serving().is_some()
				&& let Some(last) = self.last.as_ref().map(broadcast::WeakConsumer::consume)
				&& last.poll_closed(waiter).is_ready()
			{
				let moved = match &last.info().instance {
					_ if self.absorbed => true,
					Some(instance) => self
						.moved
						.get_or_insert_with(|| {
							self.consumer
								.moved(self.path.clone(), instance.clone(), self.instance.is_some())
						})
						.poll(waiter)
						.is_ready(),
					None => true,
				};
				if moved {
					self.moved = None;
					self.unseen = true;
				}
			}
			if !self.unseen {
				return Poll::Pending;
			}

			let resolving = self
				.resolving
				.get_or_insert_with(|| self.consumer.routing(self.path.clone(), self.instance.clone()));
			let resolved = ready!(kio::Task::poll(resolving, waiter));
			self.resolving = None;
			// A refusal answers for the routes serving the path now: wait for another instance
			// to serve it before asking again, rather than asking the same handler at once.
			self.unseen = false;
			let broadcast = resolved?;

			// Resolving can run ahead of the follower: a takeover resolves at once while the
			// origin holds its restart, and that restart then names the instance handed out.
			// The caller freeing it before reading the restart is owed it again.
			let weak = broadcast.weak();
			if self.last.as_ref().is_some_and(|last| last.same_channel(&weak)) {
				self.absorbed = true;
				continue;
			}
			self.last = Some(weak);
			self.moved = None;
			self.absorbed = false;
			self.live = true;
			return Poll::Ready(Ok(Some(broadcast)));
		}
	}

	/// Apply every change on hand, returning whether any came through.
	fn settle(&mut self, waiter: &kio::Waiter) -> Result<bool, Error> {
		let mut changed = false;
		while let Poll::Ready(event) = self.follow.poll_next(waiter) {
			changed |= self.apply(event.ok_or(Error::Closed)?);
		}
		Ok(changed)
	}

	/// The broadcast handed out last stopped serving the path: owe its end, and hand out
	/// whatever serves next, even the same instance coming back.
	fn end(&mut self) {
		self.ended |= self.live;
		self.last = None;
		self.moved = None;
		self.absorbed = false;
	}

	/// Apply one change to the path, returning whether it is one.
	fn apply(&mut self, event: RouteEvent) -> bool {
		match event {
			// Another instance taking over ends a pinned follower's own stream, which is handed
			// out again once it serves the path again.
			RouteEvent::Restart(announce)
				if self
					.instance
					.as_ref()
					.is_some_and(|pin| announce.route.instance.as_ref() != Some(pin)) =>
			{
				self.end();
				return false;
			}
			// A pinned resolution refuses any other instance, waiting until its own serves again.
			RouteEvent::Start(_) | RouteEvent::Restart(_) => self.unseen = true,
			// An end is a gap that ended every request on the old route, so whatever serves the
			// path next is handed out, even the same instance coming back.
			RouteEvent::End(_) => {
				self.unseen = false;
				self.end();
			}
			// The same instance over another route, which its subscriptions ride out, or one
			// continuing what was handed out.
			RouteEvent::Update(_) | RouteEvent::Takeover { .. } => return false,
		}
		true
	}
}

#[cfg(all(test, feature = "async"))]
mod tests {
	use std::time::Duration;

	use super::super::ProduceTest;
	use super::*;
	use crate::{
		Error, Hop, Instance, Pattern, Patterns,
		origin::{Cost, Route},
	};

	/// The follower's next event, which must arrive within the origin's update hold.
	async fn followed(follow: &mut Follow) -> (&'static str, String) {
		let event = kio::clock::timeout(Duration::from_secs(1), follow.next())
			.await
			.expect("no event")
			.expect("the origin closed");
		match event {
			RouteEvent::Start(announce) => ("start", announce.prefix.to_string()),
			RouteEvent::Update(announce) => ("update", announce.prefix.to_string()),
			RouteEvent::Restart(announce) => ("restart", announce.prefix.to_string()),
			RouteEvent::Takeover { announce, .. } => ("takeover", announce.prefix.to_string()),
			RouteEvent::End(announce) => ("end", announce.prefix.to_string()),
		}
	}

	/// The follower stays quiet past the origin's update hold.
	async fn quiet(follow: &mut Follow) {
		if let Ok(event) = kio::clock::timeout(Duration::from_secs(1), follow.next()).await {
			panic!("expected nothing, got {event:?}");
		}
	}

	fn route(instance: &Instance) -> Route {
		Route::default().with_instance(instance.clone(), 0)
	}

	#[kio::test]
	async fn follow_reports_the_route_serving_the_path() {
		let origin = Hop::new(1).unwrap().produce();
		let mut follow = origin.consume().path_routes("pool/job").unwrap();
		let instance = Instance::random();

		// A prefix above the path covers it.
		let pool = origin.requests_route("pool", route(&instance), None).unwrap();
		assert_eq!(followed(&mut follow).await, ("start", "pool".into()));

		// A route beneath the path serves another broadcast.
		let _beneath = origin.publish("pool/job/thumbnail", Route::default()).unwrap();
		quiet(&mut follow).await;

		// The path itself, from the same instance, takes over without a restart.
		let exact = origin.publish("pool/job", route(&instance)).unwrap();
		assert_eq!(followed(&mut follow).await, ("update", "pool/job".into()));

		// The covering prefix no longer serves the path, so its changes are not seen.
		pool.update_route(pool.route().with_cost(5)).unwrap();
		quiet(&mut follow).await;

		// Another instance at the path.
		exact.announce(route(&Instance::random())).unwrap();
		assert_eq!(followed(&mut follow).await, ("restart", "pool/job".into()));

		// It goes, so the prefix serves the path again: another instance.
		drop(exact);
		assert_eq!(followed(&mut follow).await, ("restart", "pool".into()));

		pool.update_route(pool.route().with_cost(9)).unwrap();
		assert_eq!(followed(&mut follow).await, ("update", "pool".into()));

		drop(pool);
		assert_eq!(followed(&mut follow).await, ("end", "pool".into()));
	}

	/// A follower that joins late starts on the route serving the path, not on whichever
	/// covering route the replay happened to reach first.
	#[kio::test]
	async fn follow_starts_on_the_serving_route_when_joining_late() {
		let origin = Hop::new(1).unwrap().produce();
		let _pool = origin.requests_route("pool", Route::default(), None).unwrap();
		let _exact = origin.publish("pool/job", Route::default()).unwrap();

		let mut follow = origin.consume().path_routes("pool/job").unwrap();
		assert_eq!(followed(&mut follow).await, ("start", "pool/job".into()));
		quiet(&mut follow).await;
	}

	/// A gap with nothing serving the path ends any request on it, so a covering route that
	/// arrives after it is a start of its own, even under the same instance.
	#[kio::test]
	async fn follow_keeps_a_gap_between_routes_of_one_instance() {
		let origin = Hop::new(1).unwrap().produce();
		let instance = Instance::random();
		let exact = origin.publish("pool/job", route(&instance)).unwrap();
		let mut follow = origin.consume().path_routes("pool/job").unwrap();
		assert_eq!(followed(&mut follow).await, ("start", "pool/job".into()));

		// Both land before the follower reads again, and the cursor delivers the prefix's start
		// ahead of the path's end, but the path went unserved in between.
		drop(exact);
		let _pool = origin.requests_route("pool", route(&instance), None).unwrap();
		assert_eq!(followed(&mut follow).await, ("end", "pool/job".into()));
		assert_eq!(followed(&mut follow).await, ("start", "pool".into()));

		// A prefix standing while the path's route goes takes over in place.
		let exact = origin.publish("pool/job", route(&instance)).unwrap();
		assert_eq!(followed(&mut follow).await, ("update", "pool/job".into()));
		drop(exact);
		assert_eq!(followed(&mut follow).await, ("update", "pool".into()));
	}

	/// A prefix that started before the serving route's last change stood before the route's
	/// end, so it takes over in place even when the cursor never ran dry in between.
	#[kio::test]
	async fn follow_hands_over_to_a_prefix_that_started_before_the_last_change() {
		let origin = Hop::new(1).unwrap().produce();
		let exact = origin.publish("pool/job", route(&Instance::random())).unwrap();
		let mut follow = origin.consume().path_routes("pool/job").unwrap();
		assert_eq!(followed(&mut follow).await, ("start", "pool/job".into()));

		let instance = Instance::random();
		let _pool = origin.requests_route("pool", route(&instance), None).unwrap();
		exact.announce(route(&instance)).unwrap();
		assert_eq!(followed(&mut follow).await, ("restart", "pool/job".into()));

		drop(exact);
		assert_eq!(followed(&mut follow).await, ("update", "pool".into()));
	}

	/// Under another instance nothing says two routes serve the same bytes.
	#[kio::test]
	async fn follow_restarts_onto_a_more_specific_route_under_another_instance() {
		let origin = Hop::new(1).unwrap().produce();
		let mut follow = origin.consume().path_routes("pool/job").unwrap();

		let _pool = origin.requests_route("pool", Route::default(), None).unwrap();
		assert_eq!(followed(&mut follow).await, ("start", "pool".into()));

		let _exact = origin.publish("pool/job", Route::default()).unwrap();
		assert_eq!(followed(&mut follow).await, ("restart", "pool/job".into()));
	}

	/// A route whose claim covers only paths beneath the followed one never serves it, so it
	/// cannot mask the route that does, even when it would win the prefix on cost.
	#[kio::test]
	async fn follow_ignores_a_route_scoped_beneath_the_path() {
		let origin = Hop::new(1).unwrap().produce();
		let chat = Patterns::from("room/*/chat".parse::<Pattern>().unwrap());
		let _chat = origin
			.scope("", &chat)
			.unwrap()
			.requests_route("room/alice", Route::default(), None)
			.unwrap();
		let mut follow = origin.consume().path_routes("room/alice").unwrap();

		let first = origin.publish("room/alice", Route::default().with_cost(5)).unwrap();
		let event = kio::clock::timeout(Duration::from_secs(1), follow.next())
			.await
			.unwrap()
			.unwrap();
		assert!(
			matches!(&event, RouteEvent::Start(announce) if announce.route.cost == Cost::new(5)),
			"{event:?}"
		);

		// A republish is another instance of the route that serves the path.
		let _second = origin.publish("room/alice", Route::default().with_cost(5)).unwrap();
		drop(first);
		let event = kio::clock::timeout(Duration::from_secs(1), follow.next())
			.await
			.unwrap()
			.unwrap();
		assert!(
			matches!(&event, RouteEvent::Restart(announce) if announce.route.cost == Cost::new(5)),
			"{event:?}"
		);
	}

	/// The next change handed out, which must arrive within the origin's update hold.
	async fn changed(announced: &mut Following) -> Option<broadcast::Consumer> {
		kio::clock::timeout(Duration::from_secs(1), announced.available())
			.await
			.expect("no change")
			.expect("the wait failed")
	}

	/// The next instance handed out, which must arrive within the origin's update hold.
	async fn available(announced: &mut Following) -> broadcast::Consumer {
		changed(announced).await.expect("an end, not an instance")
	}

	/// The broadcast handed out last stops serving, with nothing in its place.
	async fn ended(announced: &mut Following) {
		if let Some(broadcast) = changed(announced).await {
			panic!("expected an end, got {:?}", broadcast.info().instance);
		}
	}

	/// Nothing more is handed out past the origin's update hold.
	async fn none(announced: &mut Following) {
		let hold = super::super::origin_impl::DEFAULT_UPDATE_HOLD * 2;
		if let Ok(result) = kio::clock::timeout(hold, announced.available()).await {
			panic!(
				"expected nothing, got {:?}",
				result.map(|broadcast| broadcast.map(|broadcast| broadcast.info().instance.clone()))
			);
		}
	}

	/// The next request a dynamic route receives for its path.
	async fn requested(dynamic: &crate::origin::Requests) -> crate::origin::Request {
		kio::clock::timeout(Duration::from_secs(1), dynamic.requested_broadcast())
			.await
			.expect("never asked")
			.expect("the route closed")
	}

	/// Another instance taking over while a wait resolves is the one it hands out, so the
	/// restart the origin holds for it must not hand it out again while the caller holds it.
	#[kio::test]
	async fn announced_hands_out_a_held_takeover_once() {
		let origin = Hop::new(1).unwrap().produce();
		let dynamic = origin.requests("live").unwrap();
		let mut announced = origin.consume().follow("live", None).unwrap();

		// The wait is on the dynamic route's answer when the exact path takes over under another instance.
		let (_takeover, _publish) = futures::join!(available(&mut announced), async {
			let _unanswered = requested(&dynamic).await;
			origin.publish("live", Route::default()).unwrap()
		});
		none(&mut announced).await;
	}

	/// The route a wait resolves through leaving, and another instance arriving before it
	/// resolves, reach the follower as an end and a start alongside the answer. That start must
	/// not hand the newcomer out again.
	#[kio::test]
	async fn announced_hands_out_a_gap_while_resolving_once() {
		let origin = Hop::new(1).unwrap().produce();
		let dynamic = origin.requests("").unwrap();
		let mut announced = origin.consume().follow("live", None).unwrap();

		let (_arrival, _publish) = futures::join!(available(&mut announced), async {
			let _unanswered = requested(&dynamic).await;
			drop(dynamic);
			kio::clock::sleep(super::super::origin_impl::DEFAULT_UPDATE_HOLD * 2).await;
			origin.publish("live", Route::default()).unwrap()
		});
		none(&mut announced).await;
	}

	/// An end is a gap that ended every request on the old route, even when a route above the
	/// path with the same instance carries the instance on. So the instance serving the path
	/// afterwards is handed out again, though it is the one the caller still holds.
	#[kio::test]
	async fn announced_hands_out_the_instance_again_after_an_end() {
		let origin = Hop::new(1).unwrap().produce();
		let instance = Instance::random();
		let mut announced = origin.consume().follow("live", None).unwrap();
		let exact = origin.publish("live", route(&instance)).unwrap();
		let first = available(&mut announced).await;

		let dynamic = origin.requests_route("", route(&instance), None).unwrap();
		exact.unannounce();
		ended(&mut announced).await;
		let served = broadcast::Info::new().produce();
		let (again, ()) = futures::join!(available(&mut announced), async {
			requested(&dynamic).await.accept(&served);
		});
		assert!(again.is_clone(&first), "the route above did not carry the instance on");
	}

	/// An announced request that pins an instance ends when another instance takes over instead of
	/// following it.
	#[kio::test]
	async fn announced_pinned_refuses_another_instance() {
		let origin = Hop::new(1).unwrap().produce();
		let instance = Instance::random();
		let _first = origin.publish("live", route(&instance)).unwrap();
		let mut announced = origin.consume().follow("live", instance.clone()).unwrap();
		let pinned = available(&mut announced).await;
		assert_eq!(pinned.info().instance.as_ref(), Some(&instance));

		let second = origin.publish("live", route(&Instance::random())).unwrap();
		ended(&mut announced).await;
		none(&mut announced).await;

		// Unpinned, the same takeover is followed.
		let mut unpinned = origin.consume().follow("live", None).unwrap();
		let current = available(&mut unpinned).await;
		assert_ne!(current.info().instance.as_ref(), Some(&instance));

		// The pinned instance serves again once the other goes, and is handed out again.
		drop(second);
		assert_eq!(
			available(&mut announced).await.info().instance.as_ref(),
			Some(&instance)
		);
		assert_eq!(available(&mut unpinned).await.info().instance.as_ref(), Some(&instance));
	}

	/// A follower started before anything serves the path waits for it, reports its end, and
	/// hands out what serves it next.
	#[kio::test]
	async fn announced_reports_the_path_going_offline() {
		let origin = Hop::new(1).unwrap().produce();
		let mut announced = origin.consume().follow("live", None).unwrap();
		none(&mut announced).await;

		let first = origin.publish("live", Route::default()).unwrap();
		let started = available(&mut announced).await;
		drop(first);
		ended(&mut announced).await;
		none(&mut announced).await;

		let _second = origin.publish("live", Route::default()).unwrap();
		assert!(!available(&mut announced).await.is_clone(&started));
	}

	#[kio::test]
	async fn announced_pinned_hands_out_its_instance_after_a_gap() {
		let origin = Hop::new(1).unwrap().produce();
		let instance = Instance::random();
		let first = origin.publish("live", route(&instance)).unwrap();
		let mut announced = origin.consume().follow("live", instance.clone()).unwrap();
		assert_eq!(
			available(&mut announced).await.info().instance.as_ref(),
			Some(&instance)
		);
		first.unannounce();
		ended(&mut announced).await;
		none(&mut announced).await;
		first.announce(route(&instance)).unwrap();
		assert_eq!(
			available(&mut announced).await.info().instance.as_ref(),
			Some(&instance)
		);
	}

	#[kio::test]
	async fn announced_reports_a_dropped_prepared_answer() {
		let origin = Hop::new(1).unwrap().produce();
		let claim = origin.requests("live").unwrap();
		let mut announced = origin.consume().follow("live", None).unwrap();
		let (result, ()) = futures::join!(
			kio::clock::timeout(Duration::from_secs(1), announced.available()),
			async {
				let prepared = requested(&claim).await.prepare().unwrap();
				drop(prepared);
			}
		);
		assert!(matches!(
			result.expect("the selected refusal must answer"),
			Err(Error::Unroutable)
		));
	}

	/// A refusal is reported once, and the follower then waits for another instance rather than
	/// asking the refusing handler again.
	#[kio::test]
	async fn announced_waits_for_another_instance_after_a_refusal() {
		let origin = Hop::new(1).unwrap().produce();
		let claim = origin.requests("live").unwrap();
		let mut announced = origin.consume().follow("live", None).unwrap();
		let (result, ()) = futures::join!(
			kio::clock::timeout(Duration::from_secs(1), announced.available()),
			async { requested(&claim).await.reject(Error::NotFound) }
		);
		assert!(matches!(result.expect("the refusal answers"), Err(Error::NotFound)));
		none(&mut announced).await;
		let again = kio::clock::timeout(Duration::from_secs(1), claim.requested_broadcast()).await;
		assert!(again.is_err(), "the refusing handler was asked again");

		// A publisher takes the path over from the claim, and is handed out.
		let _publisher = origin.publish("live", route(&Instance::random())).unwrap();
		let _served = available(&mut announced).await;
	}

	/// A path no pattern can spell is refused rather than followed through the whole scope,
	/// where a route that never serves it could win its prefix.
	#[test]
	fn follow_refuses_a_path_no_pattern_can_spell() {
		let origin = Hop::new(1).unwrap().produce();
		assert!(matches!(
			origin.consume().path_routes("camera*main"),
			Err(Error::InvalidPath(_))
		));
	}

	/// A path the consumer's scope can never cover fails at once instead of waiting forever.
	#[test]
	fn follow_refuses_a_path_outside_the_scope() {
		let origin = Hop::new(1).unwrap().produce();
		let scope = Patterns::from(Pattern::subtree("pool/job/cam").unwrap());
		let scoped = origin.consume().scope("", &scope).unwrap();
		assert!(matches!(scoped.path_routes("pool/job"), Err(Error::Unauthorized)));
	}
}
