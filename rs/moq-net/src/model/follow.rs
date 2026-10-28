use std::task::{Poll, ready};

use super::WeakEntry;
use super::origin_impl::{Announce, AnnounceConsumer, AnnounceEvent, Consumer, Requesting};
use crate::{Epoch, Error, Path, broadcast};

/// The announcements covering one path, as one broadcast coming online, being replaced, and
/// going offline. Created by [`Consumer::follow`](crate::origin::Consumer::follow).
///
/// Several routes can cover the path at once, such as the path itself and a prefix above it,
/// and a request resolves through the most specific one, so only that route's events come
/// through. Another route taking over is a [`Restart`](AnnounceEvent::Restart), unless both
/// carry the same epoch: those serve the same bytes, so it is an
/// [`Update`](AnnounceEvent::Update). Routes beneath the path serve other broadcasts and are
/// ignored.
pub struct Follow {
	announced: AnnounceConsumer,
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
	pub(super) fn new(announced: AnnounceConsumer, path: Path) -> Self {
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
	#[cfg(feature = "async")]
	pub async fn next(&mut self) -> Option<AnnounceEvent> {
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

	/// Poll for the next change, registering `waiter` when there is none yet.
	///
	/// While nothing serves the path, every announcement already on hand is folded into one
	/// start, so the replay a late follower begins with (a covering prefix, then the exact path
	/// beneath it) starts on the route serving the path rather than starting and restarting.
	/// Once something serves it, each change comes through on its own: an end followed by a
	/// start is a gap that ended any request on the old route, even when both routes carry one
	/// epoch. A route whose start came after the serving route's last event, with the cursor
	/// never running dry in between, may have arrived after its end, so taking over from it is
	/// reported as that gap.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Option<AnnounceEvent>> {
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
					Poll::Ready(Some(AnnounceEvent::Start(serving)))
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
				self.idle = matches!(event, AnnounceEvent::End(_));
				return Poll::Ready(Some(event));
			}
		}
	}

	/// Poll until every source feeding this follower has synced, registering `waiter` until
	/// then. A path [`poll_next`](Self::poll_next) has not started by then is not announced,
	/// rather than not heard yet. See [`announce::Consumer::poll_synced`](crate::announce::Consumer::poll_synced).
	pub fn poll_synced(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.announced.poll_synced(waiter)
	}

	/// Wait until every source feeding this follower has synced; see [`Self::poll_synced`].
	#[cfg(feature = "async")]
	pub async fn synced(&self) {
		kio::wait(|waiter| self.poll_synced(waiter)).await
	}

	/// The cursor's next event, counting what it delivers and when it runs dry.
	fn poll_announced(&mut self, waiter: &kio::Waiter) -> Poll<Option<AnnounceEvent>> {
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
	fn fold(&mut self, event: AnnounceEvent) -> Option<AnnounceEvent> {
		let (AnnounceEvent::Start(announce)
		| AnnounceEvent::Update(announce)
		| AnnounceEvent::Restart(announce)
		| AnnounceEvent::End(announce)) = &event;
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
		let (announce, since, restart) = match event {
			AnnounceEvent::End(_) => (None, None, false),
			AnnounceEvent::Start(announce) => (Some(announce), None, false),
			AnnounceEvent::Update(announce) => (Some(announce), standing, false),
			AnnounceEvent::Restart(announce) => (Some(announce), standing, true),
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
			(None, Some((after, _))) => Some(AnnounceEvent::Start(after)),
			(Some((before, _)), None) => Some(AnnounceEvent::End(before)),
			// The serving route ended, and the one left may have started after it did.
			(Some((before, seen)), Some((after, since)))
				if before.prefix == prefix && before.prefix != after.prefix && since > self.dry.max(seen) =>
			{
				Some(AnnounceEvent::End(before))
			}
			(Some((before, _)), Some((after, _))) if before.prefix != after.prefix => {
				let same = before.route.epoch.is_some() && before.route.epoch == after.route.epoch;
				Some(match same {
					true => AnnounceEvent::Update(after),
					false => AnnounceEvent::Restart(after),
				})
			}
			// A route less specific than the serving one changed, which no request sees.
			(Some(_), Some((after, _))) if after.prefix != prefix => None,
			(Some(_), Some((after, _))) if restart => Some(AnnounceEvent::Restart(after)),
			(Some(_), Some((after, _))) => Some(AnnounceEvent::Update(after)),
		}
	}
}

/// Each publisher instance serving one path, handed out as it takes over. Created by
/// [`Consumer::announced_broadcast`](crate::origin::Consumer::announced_broadcast).
pub struct AnnouncedBroadcast {
	consumer: Consumer,
	path: Path,
	/// The only instance handed out, if pinned.
	epoch: Option<Epoch>,
	follow: Follow,
	/// An instance serves the path that was not handed out yet.
	unseen: bool,
	/// The resolution in flight, kept across a dropped wait so a retry picks it up.
	resolving: Option<Requesting>,
	/// The broadcast handed out last, until the path goes offline. Weak, so it never keeps an
	/// instance the caller let go of from retiring.
	last: Option<broadcast::WeakConsumer>,
}

impl AnnouncedBroadcast {
	pub(super) fn new(consumer: Consumer, path: Path, epoch: Option<Epoch>, follow: Follow) -> Self {
		Self {
			consumer,
			path,
			epoch,
			follow,
			unseen: false,
			resolving: None,
			last: None,
		}
	}

	/// Wait for the next publisher instance serving the path; see
	/// [`Consumer::announced_broadcast`](crate::origin::Consumer::announced_broadcast).
	///
	/// Returns [`Error::Closed`] once the origin closes, and any other resolution failure
	/// as-is. Dropping the future keeps the wait's progress for the next call.
	#[cfg(feature = "async")]
	pub async fn available(&mut self) -> Result<broadcast::Consumer, Error> {
		kio::wait(|waiter| self.poll_available(waiter)).await
	}

	/// Poll for the next publisher instance serving the path, registering `waiter` when there
	/// is none yet.
	pub fn poll_available(&mut self, waiter: &kio::Waiter) -> Poll<Result<broadcast::Consumer, Error>> {
		loop {
			// Apply every change on hand before reading an answer, so a burst of them hands out
			// only the instance serving the path now. A resolution in flight answers for the
			// routes before the change, such as an end and a start for the instance it resolved,
			// so it starts over.
			if self.settle(waiter)? {
				self.resolving = None;
			}
			if !self.unseen {
				return Poll::Pending;
			}

			let resolving = self
				.resolving
				.get_or_insert_with(|| self.consumer.routing(self.path.clone(), self.epoch.clone()));
			let resolved = ready!(kio::Task::poll(resolving, waiter));
			self.resolving = None;
			let broadcast = resolved?;
			self.unseen = false;

			// Resolving can run ahead of the follower: a takeover resolves at once while the
			// origin holds its restart, and that restart then names the instance handed out.
			let weak = broadcast.weak();
			if self.last.as_ref().is_some_and(|last| last.same_channel(&weak)) {
				continue;
			}
			self.last = Some(weak);
			return Poll::Ready(Ok(broadcast));
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

	/// Apply one change to the path, returning whether it is one.
	fn apply(&mut self, event: AnnounceEvent) -> bool {
		match event {
			// A pinned resolution refuses any other instance, waiting until its own serves again.
			AnnounceEvent::Start(_) | AnnounceEvent::Restart(_) => self.unseen = true,
			// An end is a gap that ended every request on the old route, so whatever serves the
			// path next is handed out, even the same instance coming back.
			AnnounceEvent::End(_) => {
				self.unseen = false;
				self.last = None;
			}
			// The same instance over another route, which its subscriptions ride out.
			AnnounceEvent::Update(_) => return false,
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
		Epoch, Error, Hop, Pattern, Patterns,
		origin::{Cost, Route},
	};

	/// The follower's next event, which must arrive within the origin's update hold.
	async fn followed(follow: &mut Follow) -> (&'static str, String) {
		let event = moq_clock::timeout(Duration::from_secs(1), follow.next())
			.await
			.expect("no event")
			.expect("the origin closed");
		match event {
			AnnounceEvent::Start(announce) => ("start", announce.prefix.to_string()),
			AnnounceEvent::Update(announce) => ("update", announce.prefix.to_string()),
			AnnounceEvent::Restart(announce) => ("restart", announce.prefix.to_string()),
			AnnounceEvent::End(announce) => ("end", announce.prefix.to_string()),
		}
	}

	/// The follower stays quiet past the origin's update hold.
	async fn quiet(follow: &mut Follow) {
		if let Ok(event) = moq_clock::timeout(Duration::from_secs(1), follow.next()).await {
			panic!("expected nothing, got {event:?}");
		}
	}

	fn route(epoch: &Epoch) -> Route {
		Route::default().with_epoch(epoch.clone())
	}

	#[moq_clock::test]
	async fn follow_reports_the_route_serving_the_path() {
		let origin = Hop::new(1).unwrap().produce();
		let mut follow = origin.consume().follow("pool/job").unwrap();
		let epoch = Epoch::mint();

		// A prefix above the path covers it.
		let pool = origin.requests_route("pool", route(&epoch)).unwrap();
		assert_eq!(followed(&mut follow).await, ("start", "pool".into()));

		// A route beneath the path serves another broadcast.
		let _beneath = origin.publish("pool/job/thumbnail", Route::default()).unwrap();
		quiet(&mut follow).await;

		// The path itself, from the same instance, takes over without a restart.
		let exact = origin.publish("pool/job", route(&epoch)).unwrap();
		assert_eq!(followed(&mut follow).await, ("update", "pool/job".into()));

		// The covering prefix no longer serves the path, so its changes are not seen.
		pool.update_route(pool.route().with_cost(5)).unwrap();
		quiet(&mut follow).await;

		// Another instance at the path.
		exact.announce(route(&Epoch::mint())).unwrap();
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
	#[moq_clock::test]
	async fn follow_starts_on_the_serving_route_when_joining_late() {
		let origin = Hop::new(1).unwrap().produce();
		let _pool = origin.requests_route("pool", Route::default()).unwrap();
		let _exact = origin.publish("pool/job", Route::default()).unwrap();

		let mut follow = origin.consume().follow("pool/job").unwrap();
		assert_eq!(followed(&mut follow).await, ("start", "pool/job".into()));
		quiet(&mut follow).await;
	}

	/// A gap with nothing serving the path ends any request on it, so a covering route that
	/// arrives after it is a start of its own, even under the same epoch.
	#[moq_clock::test]
	async fn follow_keeps_a_gap_between_routes_of_one_epoch() {
		let origin = Hop::new(1).unwrap().produce();
		let epoch = Epoch::mint();
		let exact = origin.publish("pool/job", route(&epoch)).unwrap();
		let mut follow = origin.consume().follow("pool/job").unwrap();
		assert_eq!(followed(&mut follow).await, ("start", "pool/job".into()));

		// Both land before the follower reads again, and the cursor delivers the prefix's start
		// ahead of the path's end, but the path went unserved in between.
		drop(exact);
		let _pool = origin.requests_route("pool", route(&epoch)).unwrap();
		assert_eq!(followed(&mut follow).await, ("end", "pool/job".into()));
		assert_eq!(followed(&mut follow).await, ("start", "pool".into()));

		// A prefix standing while the path's route goes takes over in place.
		let exact = origin.publish("pool/job", route(&epoch)).unwrap();
		assert_eq!(followed(&mut follow).await, ("update", "pool/job".into()));
		drop(exact);
		assert_eq!(followed(&mut follow).await, ("update", "pool".into()));
	}

	/// A prefix that started before the serving route's last change stood before the route's
	/// end, so it takes over in place even when the cursor never ran dry in between.
	#[moq_clock::test]
	async fn follow_hands_over_to_a_prefix_that_started_before_the_last_change() {
		let origin = Hop::new(1).unwrap().produce();
		let exact = origin.publish("pool/job", route(&Epoch::mint())).unwrap();
		let mut follow = origin.consume().follow("pool/job").unwrap();
		assert_eq!(followed(&mut follow).await, ("start", "pool/job".into()));

		let epoch = Epoch::mint();
		let _pool = origin.requests_route("pool", route(&epoch)).unwrap();
		exact.announce(route(&epoch)).unwrap();
		assert_eq!(followed(&mut follow).await, ("restart", "pool/job".into()));

		drop(exact);
		assert_eq!(followed(&mut follow).await, ("update", "pool".into()));
	}

	/// Without an epoch nothing says two routes serve the same bytes.
	#[moq_clock::test]
	async fn follow_restarts_onto_a_more_specific_route_without_an_epoch() {
		let origin = Hop::new(1).unwrap().produce();
		let mut follow = origin.consume().follow("pool/job").unwrap();

		let _pool = origin.requests_route("pool", Route::default()).unwrap();
		assert_eq!(followed(&mut follow).await, ("start", "pool".into()));

		let _exact = origin.publish("pool/job", Route::default()).unwrap();
		assert_eq!(followed(&mut follow).await, ("restart", "pool/job".into()));
	}

	/// A route whose claim covers only paths beneath the followed one never serves it, so it
	/// cannot mask the route that does, even when it would win the prefix on cost.
	#[moq_clock::test]
	async fn follow_ignores_a_route_scoped_beneath_the_path() {
		let origin = Hop::new(1).unwrap().produce();
		let chat = Patterns::from("room/*/chat".parse::<Pattern>().unwrap());
		let _chat = origin
			.scope("", &chat)
			.unwrap()
			.requests_route("room/alice", Route::default())
			.unwrap();
		let mut follow = origin.consume().follow("room/alice").unwrap();

		let first = origin.publish("room/alice", Route::default().with_cost(5)).unwrap();
		let event = moq_clock::timeout(Duration::from_secs(1), follow.next())
			.await
			.unwrap()
			.unwrap();
		assert!(
			matches!(&event, AnnounceEvent::Start(announce) if announce.route.cost == Cost::new(5)),
			"{event:?}"
		);

		// A republish is another instance of the route that serves the path.
		let _second = origin.publish("room/alice", Route::default().with_cost(5)).unwrap();
		drop(first);
		let event = moq_clock::timeout(Duration::from_secs(1), follow.next())
			.await
			.unwrap()
			.unwrap();
		assert!(
			matches!(&event, AnnounceEvent::Restart(announce) if announce.route.cost == Cost::new(5)),
			"{event:?}"
		);
	}

	/// The next instance handed out, which must arrive within the origin's update hold.
	async fn available(announced: &mut AnnouncedBroadcast) -> broadcast::Consumer {
		moq_clock::timeout(Duration::from_secs(1), announced.available())
			.await
			.expect("no instance")
			.expect("the wait failed")
	}

	/// Nothing more is handed out past the origin's update hold.
	async fn none(announced: &mut AnnouncedBroadcast) {
		let hold = super::super::origin_impl::DEFAULT_UPDATE_HOLD * 2;
		if let Ok(result) = moq_clock::timeout(hold, announced.available()).await {
			panic!(
				"expected nothing, got {:?}",
				result.map(|broadcast| broadcast.info().epoch.clone())
			);
		}
	}

	/// The next request a dynamic route receives for its path.
	async fn requested(dynamic: &crate::origin::Requests) -> crate::origin::Request {
		moq_clock::timeout(Duration::from_secs(1), dynamic.requested_broadcast())
			.await
			.expect("never asked")
			.expect("the route closed")
	}

	/// Another instance taking over while a wait resolves is the one it hands out, so the
	/// restart the origin holds for it must not hand it out again while the caller holds it.
	#[moq_clock::test]
	async fn announced_hands_out_a_held_takeover_once() {
		let origin = Hop::new(1).unwrap().produce();
		let dynamic = origin.requests("live").unwrap();
		let mut announced = origin.consume().announced_broadcast("live", None).unwrap();

		// The wait is on the dynamic route's answer when the exact path takes over without an epoch.
		let (_takeover, _publish) = futures::join!(available(&mut announced), async {
			let _unanswered = requested(&dynamic).await;
			origin.publish("live", Route::default()).unwrap()
		});
		none(&mut announced).await;
	}

	/// The follower holds what it handed out weakly: a takeover the caller let go of retires,
	/// and the restart the origin held for it hands out a fresh front.
	#[moq_clock::test]
	async fn announced_hands_out_a_dropped_takeover_again() {
		let origin = Hop::new(1).unwrap().produce();
		let dynamic = origin.requests("live").unwrap();
		let mut announced = origin.consume().announced_broadcast("live", None).unwrap();

		let (dropped, _publish) = futures::join!(available(&mut announced), async {
			let _unanswered = requested(&dynamic).await;
			origin.publish("live", Route::default()).unwrap()
		});
		let front = dropped.weak();
		drop(dropped);
		let again = available(&mut announced).await;
		assert!(front.is_closed(), "the follower kept the dropped front alive");
		assert!(!again.weak().same_channel(&front));
	}

	/// The route a wait resolves through leaving, and another instance arriving before it
	/// resolves, reach the follower as an end and a start alongside the answer. That start must
	/// not hand the newcomer out again.
	#[moq_clock::test]
	async fn announced_hands_out_a_gap_while_resolving_once() {
		let origin = Hop::new(1).unwrap().produce();
		let dynamic = origin.requests("").unwrap();
		let mut announced = origin.consume().announced_broadcast("live", None).unwrap();

		let (_arrival, _publish) = futures::join!(available(&mut announced), async {
			let _unanswered = requested(&dynamic).await;
			drop(dynamic);
			moq_clock::sleep(super::super::origin_impl::DEFAULT_UPDATE_HOLD * 2).await;
			origin.publish("live", Route::default()).unwrap()
		});
		none(&mut announced).await;
	}

	/// An end is a gap that ended every request on the old route, even when a route above the
	/// path with the same epoch carries the instance on. So the instance serving the path
	/// afterwards is handed out again, though it is the one the caller still holds.
	#[moq_clock::test]
	async fn announced_hands_out_the_instance_again_after_an_end() {
		let origin = Hop::new(1).unwrap().produce();
		let epoch = Epoch::mint();
		let mut announced = origin.consume().announced_broadcast("live", None).unwrap();
		let exact = origin.publish("live", route(&epoch)).unwrap();
		let first = available(&mut announced).await;

		let dynamic = origin.requests_route("", route(&epoch)).unwrap();
		exact.unannounce();
		let served = broadcast::Info::new().produce();
		let (again, ()) = futures::join!(available(&mut announced), async {
			requested(&dynamic).await.accept(&served);
		});
		assert!(again.is_clone(&first), "the route above did not carry the instance on");
	}

	/// An announced request that pins an epoch refuses another instance taking over instead of
	/// following it.
	#[moq_clock::test]
	async fn announced_pinned_refuses_another_instance() {
		let origin = Hop::new(1).unwrap().produce();
		let epoch = Epoch::mint();
		let _first = origin.publish("live", route(&epoch)).unwrap();
		let mut announced = origin.consume().announced_broadcast("live", epoch.clone()).unwrap();
		let pinned = available(&mut announced).await;
		assert_eq!(pinned.info().epoch.as_ref(), Some(&epoch));

		let _second = origin.publish("live", route(&Epoch::mint())).unwrap();
		none(&mut announced).await;

		// Unpinned, the same takeover is followed.
		let mut unpinned = origin.consume().announced_broadcast("live", None).unwrap();
		let current = available(&mut unpinned).await;
		assert_ne!(current.info().epoch.as_ref(), Some(&epoch));
	}

	#[moq_clock::test]
	async fn announced_pinned_hands_out_its_instance_after_a_gap() {
		let origin = Hop::new(1).unwrap().produce();
		let epoch = Epoch::mint();
		let first = origin.publish("live", route(&epoch)).unwrap();
		let mut announced = origin.consume().announced_broadcast("live", epoch.clone()).unwrap();
		assert_eq!(available(&mut announced).await.info().epoch.as_ref(), Some(&epoch));
		first.unannounce();
		none(&mut announced).await;
		first.announce(route(&epoch)).unwrap();
		assert_eq!(available(&mut announced).await.info().epoch.as_ref(), Some(&epoch));
	}

	#[moq_clock::test]
	async fn announced_reports_a_dropped_prepared_answer() {
		let origin = Hop::new(1).unwrap().produce();
		let claim = origin.requests("live").unwrap();
		let mut announced = origin.consume().announced_broadcast("live", None).unwrap();
		let (result, ()) = futures::join!(
			moq_clock::timeout(Duration::from_secs(1), announced.available()),
			async {
				let prepared = requested(&claim).await.prepare(Epoch::mint()).unwrap();
				drop(prepared);
			}
		);
		assert!(matches!(
			result.expect("the selected refusal must answer"),
			Err(Error::Unroutable)
		));
	}

	/// A path no pattern can spell is refused rather than followed through the whole scope,
	/// where a route that never serves it could win its prefix.
	#[test]
	fn follow_refuses_a_path_no_pattern_can_spell() {
		let origin = Hop::new(1).unwrap().produce();
		assert!(matches!(
			origin.consume().follow("camera*main"),
			Err(Error::InvalidPath(_))
		));
	}

	/// A path the consumer's scope can never cover fails at once instead of waiting forever.
	#[test]
	fn follow_refuses_a_path_outside_the_scope() {
		let origin = Hop::new(1).unwrap().produce();
		let scope = Patterns::from(Pattern::subtree("pool/job/cam").unwrap());
		let scoped = origin.consume().scope("", &scope).unwrap();
		assert!(matches!(scoped.follow("pool/job"), Err(Error::Unauthorized)));
	}
}
