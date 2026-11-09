//! The paths announced under a scope, each present or absent.

use std::collections::HashSet;
use std::task::Poll;

use super::origin_impl::{Announce, RouteEvent, Routes};
use crate::{Error, Path};

/// What an announcement listing reports for a path.
#[derive(Clone, Debug)]
pub enum AnnounceEvent {
	/// A route now covers the path, which was absent. It may arrive unavailable; see
	/// [`RouteInfo::available`](crate::origin::RouteInfo::available).
	Present(Announce),
	/// The present path's route lost every usable path to its origin but may recover
	/// one, so the path stays present while requests for it may fail.
	Unavailable(Announce),
	/// The present path's route recovered a usable path: request it again.
	Available(Announce),
	/// The origin withdrew the path: no route covers it any more. Carries its last
	/// advertised route.
	Absent(Announce),
	/// This origin let go of the path, and no route covers it any more: the session it
	/// arrived on closed, or the network lost every path to it. Its origin may still
	/// serve it. Carries its last advertised route.
	Offline(Announce),
}

/// The paths announced in a scope, each reported as it becomes present or absent.
///
/// A path that another instance takes over stays present, and a route's metadata
/// changing is not reported, except its availability: hold a
/// [`follow`](crate::origin::Consumer::follow) to see the broadcast change. Created by
/// [`Consumer::announced`](crate::origin::Consumer::announced). Drop to unregister.
pub struct AnnounceConsumer {
	routes: Routes,
	/// The present paths last reported unavailable, bounded by the present set.
	unavailable: HashSet<Path>,
}

impl AnnounceConsumer {
	pub(super) fn new(routes: Routes) -> Self {
		Self {
			routes,
			unavailable: HashSet::new(),
		}
	}

	/// What a route change means for the listing, if anything.
	fn list(&mut self, event: RouteEvent) -> Option<AnnounceEvent> {
		match event {
			RouteEvent::Start(announce) => {
				if !announce.route.available() {
					self.unavailable.insert(announce.prefix.clone());
				}
				Some(AnnounceEvent::Present(announce))
			}
			RouteEvent::Update(announce) | RouteEvent::Restart(announce) => {
				let was = !self.unavailable.contains(&announce.prefix);
				match (was, announce.route.available()) {
					(true, false) => {
						self.unavailable.insert(announce.prefix.clone());
						Some(AnnounceEvent::Unavailable(announce))
					}
					(false, true) => {
						self.unavailable.remove(&announce.prefix);
						Some(AnnounceEvent::Available(announce))
					}
					_ => None,
				}
			}
			RouteEvent::End(announce) => {
				self.unavailable.remove(&announce.prefix);
				Some(AnnounceEvent::Absent(announce))
			}
			RouteEvent::Offline(announce) => {
				self.unavailable.remove(&announce.prefix);
				Some(AnnounceEvent::Offline(announce))
			}
		}
	}

	/// Returns the next path that became present or absent, relative to this listing's root.
	///
	/// Returns None once the origin closes. The consumer is also a [`futures::Stream`]
	/// of the same events.
	#[cfg(feature = "async")]
	pub async fn next(&mut self) -> Option<AnnounceEvent> {
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

	/// Poll for the next event, registering `waiter` when there is none yet.
	///
	/// Returns `Poll::Ready(None)` once the origin closes.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Option<AnnounceEvent>> {
		loop {
			let Some(event) = std::task::ready!(self.routes.poll_next(waiter)) else {
				return Poll::Ready(None);
			};
			if let Some(event) = self.list(event) {
				return Poll::Ready(Some(event));
			}
		}
	}

	/// Returns the next event without blocking.
	///
	/// Returns None if there is no event available; NOT because the listing is closed.
	/// Use [`Self::is_closed`] to check if the listing is closed.
	pub fn try_next(&mut self) -> Option<AnnounceEvent> {
		loop {
			let event = self.routes.try_next()?;
			if let Some(event) = self.list(event) {
				return Some(event);
			}
		}
	}

	/// Returns true if the listing is closed (no more events will arrive).
	pub fn is_closed(&self) -> bool {
		self.routes.is_closed()
	}

	/// Wait until this listing has synced with what its sources announced.
	///
	/// Synced means every session feeding this scope has delivered its peer's
	/// announcements as of when it answered, and this listing has read every event
	/// queued before then. A path not present by then is absent, rather than not heard
	/// yet. A session joining the scope later, such as a reconnect, takes this back
	/// until it syncs too; routes published on this origin itself never wait.
	///
	/// Level-triggered: ready whenever synced, and once the listing is closed.
	pub fn poll_synced(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.routes.poll_synced(waiter)
	}

	/// Returns the root that is automatically stripped from reported paths.
	pub fn root(&self) -> &Path {
		self.routes.root()
	}

	/// Converts a reported path back to one rooted at the origin.
	pub fn absolute(&self, prefix: impl TryInto<Path, Error: Into<Error>>) -> Result<Path, Error> {
		self.routes.absolute(prefix)
	}
}

#[cfg(feature = "async")]
impl futures::Stream for AnnounceConsumer {
	type Item = AnnounceEvent;

	fn poll_next(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Option<Self::Item>> {
		let this = self.get_mut();
		loop {
			let Some(event) = std::task::ready!(futures::Stream::poll_next(std::pin::Pin::new(&mut this.routes), cx))
			else {
				return Poll::Ready(None);
			};
			if let Some(event) = this.list(event) {
				return Poll::Ready(Some(event));
			}
		}
	}
}

#[cfg(all(test, feature = "async"))]
mod tests {
	use std::time::Duration;

	use super::super::ProduceTest;
	use super::*;
	use crate::{
		Hop, Instance,
		origin::{Cost, RouteInfo, RouteOptions},
	};

	/// The listing's next event as `present path` or `absent path`, within the update hold.
	async fn listed(announced: &mut AnnounceConsumer) -> String {
		let event = kio::clock::timeout(Duration::from_secs(1), announced.next())
			.await
			.expect("no event")
			.expect("the origin closed");
		match event {
			AnnounceEvent::Present(announce) => format!("present {}", announce.prefix),
			AnnounceEvent::Absent(announce) => format!("absent {}", announce.prefix),
			AnnounceEvent::Offline(announce) => format!("offline {}", announce.prefix),
			AnnounceEvent::Available(announce) => format!("available {}", announce.prefix),
			AnnounceEvent::Unavailable(announce) => format!("unavailable {}", announce.prefix),
		}
	}

	/// The listing stays quiet past the origin's update hold.
	async fn quiet(announced: &mut AnnounceConsumer) {
		if let Ok(event) = kio::clock::timeout(Duration::from_secs(1), announced.next()).await {
			panic!("expected nothing, got {event:?}");
		}
	}

	fn route(instance: &Instance) -> RouteOptions {
		RouteOptions::default().with_instance(instance.clone(), 0)
	}

	/// A path stays present while another instance takes it over or its route changes, and is
	/// absent only once nothing covers it.
	#[kio::test]
	async fn a_path_stays_present_across_instances() {
		let origin = Hop::new(1).unwrap().produce();
		let mut announced = origin.consume().announced();
		let mut routes = origin.consume().routes();

		let first = origin.route("live", route(&Instance::random())).unwrap();
		assert_eq!(listed(&mut announced).await, "present live");
		assert!(matches!(routes.next().await, Some(RouteEvent::Start(_))));

		first
			.announce_info(
				RouteInfo::new()
					.with_instance(Instance::random(), 0)
					.with_cost(Cost::new(5)),
			)
			.unwrap();
		quiet(&mut announced).await;
		// The relay-level stream still reports the replacement.
		assert!(matches!(routes.next().await, Some(RouteEvent::Restart(_))));

		let second = origin.route("live", route(&Instance::random())).unwrap();
		drop(first);
		quiet(&mut announced).await;

		drop(second);
		assert_eq!(listed(&mut announced).await, "absent live");
	}

	/// A held route keeps its path present, reported unavailable and available as it flips; a
	/// session losing a route takes its path offline, while a peer's withdrawal ends it.
	#[kio::test]
	async fn availability_flips_and_offline_is_not_absent() {
		let origin = Hop::new(1).unwrap().produce();
		let mut announced = origin.consume().announced();
		let instance = Instance::random();
		let held = RouteInfo::new().with_instance(instance.clone(), 0).with_available(false);

		let lost = origin.forward("lost", held.clone()).unwrap();
		assert_eq!(listed(&mut announced).await, "present lost");
		lost.update(held.clone().with_cost(Cost::new(3))).unwrap();
		quiet(&mut announced).await;
		lost.update(held.clone().with_available(true)).unwrap();
		assert_eq!(listed(&mut announced).await, "available lost");
		lost.update(held.clone()).unwrap();
		assert_eq!(listed(&mut announced).await, "unavailable lost");
		drop(lost);
		assert_eq!(listed(&mut announced).await, "offline lost");

		let ended = origin.forward("ended", held.with_available(true)).unwrap();
		assert_eq!(listed(&mut announced).await, "present ended");
		ended.withdrawn();
		assert_eq!(listed(&mut announced).await, "absent ended");

		// A route published here ends with its handle.
		let local = origin.route("local", route(&instance)).unwrap();
		assert_eq!(listed(&mut announced).await, "present local");
		drop(local);
		assert_eq!(listed(&mut announced).await, "absent local");
	}
}
