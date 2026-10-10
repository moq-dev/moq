use std::task::{Poll, ready};

use super::origin_impl::{Announce, AnnounceConsumer, AnnounceEvent};
use crate::PathOwned;

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
	path: PathOwned,
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
	pub(super) fn new(announced: AnnounceConsumer, path: PathOwned) -> Self {
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

#[cfg(test)]
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
		let event = moq_net_sim::timeout(Duration::from_secs(1), follow.next())
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
		if let Ok(event) = moq_net_sim::timeout(Duration::from_secs(1), follow.next()).await {
			panic!("expected nothing, got {event:?}");
		}
	}

	fn route(epoch: &Epoch) -> Route {
		Route::default().with_epoch(epoch.clone())
	}

	#[moq_net_sim::test]
	async fn follow_reports_the_route_serving_the_path() {
		let origin = Hop::new(1).unwrap().produce();
		let mut follow = origin.consume().follow("pool/job").unwrap();
		let epoch = Epoch::mint();

		// A prefix above the path covers it.
		let pool = origin.dynamic("pool", route(&epoch)).unwrap();
		assert_eq!(followed(&mut follow).await, ("start", "pool".into()));

		// A route beneath the path serves another broadcast.
		let _beneath = origin.publish("pool/job/thumbnail", Route::default()).unwrap();
		quiet(&mut follow).await;

		// The path itself, from the same instance, takes over without a restart.
		let exact = origin.publish("pool/job", route(&epoch)).unwrap();
		assert_eq!(followed(&mut follow).await, ("update", "pool/job".into()));

		// The covering prefix no longer serves the path, so its changes are not seen.
		pool.update(pool.route().with_cost(5)).unwrap();
		quiet(&mut follow).await;

		// Another instance at the path.
		exact.announce(route(&Epoch::mint())).unwrap();
		assert_eq!(followed(&mut follow).await, ("restart", "pool/job".into()));

		// It goes, so the prefix serves the path again: another instance.
		drop(exact);
		assert_eq!(followed(&mut follow).await, ("restart", "pool".into()));

		pool.update(pool.route().with_cost(9)).unwrap();
		assert_eq!(followed(&mut follow).await, ("update", "pool".into()));

		drop(pool);
		assert_eq!(followed(&mut follow).await, ("end", "pool".into()));
	}

	/// A follower that joins late starts on the route serving the path, not on whichever
	/// covering route the replay happened to reach first.
	#[moq_net_sim::test]
	async fn follow_starts_on_the_serving_route_when_joining_late() {
		let origin = Hop::new(1).unwrap().produce();
		let _pool = origin.dynamic("pool", Route::default()).unwrap();
		let _exact = origin.publish("pool/job", Route::default()).unwrap();

		let mut follow = origin.consume().follow("pool/job").unwrap();
		assert_eq!(followed(&mut follow).await, ("start", "pool/job".into()));
		quiet(&mut follow).await;
	}

	/// A gap with nothing serving the path ends any request on it, so a covering route that
	/// arrives after it is a start of its own, even under the same epoch.
	#[moq_net_sim::test]
	async fn follow_keeps_a_gap_between_routes_of_one_epoch() {
		let origin = Hop::new(1).unwrap().produce();
		let epoch = Epoch::mint();
		let exact = origin.publish("pool/job", route(&epoch)).unwrap();
		let mut follow = origin.consume().follow("pool/job").unwrap();
		assert_eq!(followed(&mut follow).await, ("start", "pool/job".into()));

		// Both land before the follower reads again, and the cursor delivers the prefix's start
		// ahead of the path's end, but the path went unserved in between.
		drop(exact);
		let _pool = origin.dynamic("pool", route(&epoch)).unwrap();
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
	#[moq_net_sim::test]
	async fn follow_hands_over_to_a_prefix_that_started_before_the_last_change() {
		let origin = Hop::new(1).unwrap().produce();
		let exact = origin.publish("pool/job", route(&Epoch::mint())).unwrap();
		let mut follow = origin.consume().follow("pool/job").unwrap();
		assert_eq!(followed(&mut follow).await, ("start", "pool/job".into()));

		let epoch = Epoch::mint();
		let _pool = origin.dynamic("pool", route(&epoch)).unwrap();
		exact.announce(route(&epoch)).unwrap();
		assert_eq!(followed(&mut follow).await, ("restart", "pool/job".into()));

		drop(exact);
		assert_eq!(followed(&mut follow).await, ("update", "pool".into()));
	}

	/// Without an epoch nothing says two routes serve the same bytes.
	#[moq_net_sim::test]
	async fn follow_restarts_onto_a_more_specific_route_without_an_epoch() {
		let origin = Hop::new(1).unwrap().produce();
		let mut follow = origin.consume().follow("pool/job").unwrap();

		let _pool = origin.dynamic("pool", Route::default()).unwrap();
		assert_eq!(followed(&mut follow).await, ("start", "pool".into()));

		let _exact = origin.publish("pool/job", Route::default()).unwrap();
		assert_eq!(followed(&mut follow).await, ("restart", "pool/job".into()));
	}

	/// A route whose claim covers only paths beneath the followed one never serves it, so it
	/// cannot mask the route that does, even when it would win the prefix on cost.
	#[moq_net_sim::test]
	async fn follow_ignores_a_route_scoped_beneath_the_path() {
		let origin = Hop::new(1).unwrap().produce();
		let chat = Patterns::from("room/*/chat".parse::<Pattern>().unwrap());
		let _chat = origin
			.scope("", &chat)
			.unwrap()
			.dynamic("room/alice", Route::default())
			.unwrap();
		let mut follow = origin.consume().follow("room/alice").unwrap();

		let first = origin.publish("room/alice", Route::default().with_cost(5)).unwrap();
		let event = moq_net_sim::timeout(Duration::from_secs(1), follow.next())
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
		let event = moq_net_sim::timeout(Duration::from_secs(1), follow.next())
			.await
			.unwrap()
			.unwrap();
		assert!(
			matches!(&event, AnnounceEvent::Restart(announce) if announce.route.cost == Cost::new(5)),
			"{event:?}"
		);
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
