use std::task::Poll;

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
	covering: Vec<Announce>,
}

impl Follow {
	pub(super) fn new(announced: AnnounceConsumer, path: PathOwned) -> Self {
		Self {
			announced,
			path,
			covering: Vec::new(),
		}
	}

	/// The next change to the route serving the path, or `None` once the origin closes.
	pub async fn next(&mut self) -> Option<AnnounceEvent> {
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

	/// Poll for the next change, registering `waiter` when there is none yet.
	///
	/// Every announcement already on hand is folded into one change, so the replay a late
	/// follower starts with (a covering prefix, then the exact path beneath it) is one start on
	/// the route serving the path rather than a start and a restart.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Option<AnnounceEvent>> {
		let before = self.serving().cloned();
		// Whether any announcement on hand changed the serving route, and whether one changed
		// which instance it is.
		let (mut changed, mut replaced) = (false, false);
		let ended = loop {
			match self.announced.poll_next(waiter) {
				Poll::Ready(Some(event)) => match self.fold(event) {
					None => {}
					Some(AnnounceEvent::Update(_)) => changed = true,
					Some(_) => (changed, replaced) = (true, true),
				},
				Poll::Ready(None) => break true,
				Poll::Pending => break false,
			}
		};

		let change = match (before, self.serving().cloned()) {
			(None, None) => None,
			(None, Some(after)) => Some(AnnounceEvent::Start(after)),
			(Some(before), None) => Some(AnnounceEvent::End(before)),
			(Some(before), Some(after)) => {
				// Routes with one epoch serve the same bytes, whatever happened in between.
				let same = before.route.epoch.is_some() && before.route.epoch == after.route.epoch;
				match (replaced && !same, changed) {
					(true, _) => Some(AnnounceEvent::Restart(after)),
					(false, true) => Some(AnnounceEvent::Update(after)),
					(false, false) => None,
				}
			}
		};
		match (change, ended) {
			(Some(change), _) => Poll::Ready(Some(change)),
			(None, true) => Poll::Ready(None),
			(None, false) => Poll::Pending,
		}
	}

	/// The most specific route covering the path, which is the one a request resolves.
	fn serving(&self) -> Option<&Announce> {
		self.covering
			.iter()
			.max_by_key(|announce| announce.prefix.as_str().len())
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
		let before = self.serving().cloned();

		self.covering.retain(|standing| standing.prefix != prefix);
		let restart = match event {
			AnnounceEvent::End(_) => false,
			AnnounceEvent::Start(announce) | AnnounceEvent::Update(announce) => {
				self.covering.push(announce);
				false
			}
			AnnounceEvent::Restart(announce) => {
				self.covering.push(announce);
				true
			}
		};

		let after = self.serving().cloned();
		match (before, after) {
			(None, None) => None,
			(None, Some(after)) => Some(AnnounceEvent::Start(after)),
			(Some(before), None) => Some(AnnounceEvent::End(before)),
			(Some(before), Some(after)) if before.prefix != after.prefix => {
				let same = before.route.epoch.is_some() && before.route.epoch == after.route.epoch;
				Some(match same {
					true => AnnounceEvent::Update(after),
					false => AnnounceEvent::Restart(after),
				})
			}
			// A route less specific than the serving one changed, which no request sees.
			(Some(_), Some(after)) if after.prefix != prefix => None,
			(Some(_), Some(after)) if restart => Some(AnnounceEvent::Restart(after)),
			(Some(_), Some(after)) => Some(AnnounceEvent::Update(after)),
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
