//! The paths announced under a scope, each present or absent.

use std::task::Poll;

use super::origin_impl::{Announce, RouteEvent, RouteKind, Routes};
use crate::{Error, Path};

/// What an announcement listing reports for a path.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum AnnounceEvent {
	/// A route now covers the path, which was absent. It may arrive unavailable; see
	/// [`RouteInfo::available`](crate::origin::RouteInfo::available).
	Present(Announce),
	/// No route covers the path any more, whether its origin withdrew it or this side let
	/// go of it. Carries its last advertised route.
	Absent(Announce),
}

/// The paths announced in a scope, each reported as it becomes present or absent.
///
/// A path that another identity takes over stays present, and a route's metadata,
/// availability included, changing is not reported: hold a
/// [`follow`](crate::origin::Consumer::follow) to see the broadcast change. Created by
/// [`Consumer::announced`](crate::origin::Consumer::announced). Drop to unregister.
pub struct AnnounceConsumer {
	routes: Routes,
}

impl AnnounceConsumer {
	pub(super) fn new(routes: Routes) -> Self {
		Self { routes }
	}

	/// What a route change means for the listing, if anything.
	fn list(event: RouteEvent) -> Option<AnnounceEvent> {
		let RouteEvent { kind, announce } = event;
		match kind {
			RouteKind::Start => Some(AnnounceEvent::Present(announce)),
			RouteKind::Update | RouteKind::Restart => None,
			RouteKind::End | RouteKind::Offline => Some(AnnounceEvent::Absent(announce)),
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
			if let Some(event) = Self::list(event) {
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
			if let Some(event) = Self::list(event) {
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
			if let Some(event) = Self::list(event) {
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
		Hop, Identity,
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
		}
	}

	/// The listing stays quiet past the origin's update hold.
	async fn quiet(announced: &mut AnnounceConsumer) {
		if let Ok(event) = kio::clock::timeout(Duration::from_secs(1), announced.next()).await {
			panic!("expected nothing, got {event:?}");
		}
	}

	fn route(identity: &Identity) -> RouteOptions {
		RouteOptions::default().with_identity(identity.clone(), 0)
	}

	/// A path stays present while another identity takes it over or its route changes, and is
	/// absent only once nothing covers it.
	#[kio::test]
	async fn a_path_stays_present_across_identities() {
		let origin = Hop::new(1).unwrap().produce();
		let mut announced = origin.consume().announced();
		let mut routes = origin.consume().routes();

		let first = origin.route("live", route(&Identity::random())).unwrap();
		assert_eq!(listed(&mut announced).await, "present live");
		assert!(matches!(
			routes.next().await,
			Some(RouteEvent {
				kind: RouteKind::Start,
				..
			})
		));

		first
			.announce_info(
				RouteInfo::new()
					.with_identity(Identity::random(), 0)
					.with_cost(Cost::new(5)),
			)
			.unwrap();
		quiet(&mut announced).await;
		// The relay-level stream still reports the replacement.
		assert!(matches!(
			routes.next().await,
			Some(RouteEvent {
				kind: RouteKind::Restart,
				..
			})
		));

		let second = origin.route("live", route(&Identity::random())).unwrap();
		drop(first);
		quiet(&mut announced).await;

		drop(second);
		assert_eq!(listed(&mut announced).await, "absent live");
	}

	/// A held route keeps its path present and quiet as its availability flips; a session
	/// letting go of a route and a peer withdrawing it both leave the path absent.
	#[kio::test]
	async fn availability_is_quiet_and_offline_is_absent() {
		let origin = Hop::new(1).unwrap().produce();
		let mut announced = origin.consume().announced();
		let identity = Identity::random();
		let held = RouteInfo::new()
			.with_identity(identity.clone(), 0)
			.with_available(false);

		let lost = origin.forward("lost", held.clone()).unwrap();
		assert_eq!(listed(&mut announced).await, "present lost");
		lost.update(held.clone().with_cost(Cost::new(3))).unwrap();
		quiet(&mut announced).await;
		lost.update(held.clone().with_available(true)).unwrap();
		quiet(&mut announced).await;
		lost.update(held.clone()).unwrap();
		quiet(&mut announced).await;
		drop(lost);
		assert_eq!(listed(&mut announced).await, "absent lost");

		let ended = origin.forward("ended", held.with_available(true)).unwrap();
		assert_eq!(listed(&mut announced).await, "present ended");
		ended.withdrawn();
		assert_eq!(listed(&mut announced).await, "absent ended");

		// A route published here ends with its handle.
		let local = origin.route("local", route(&identity)).unwrap();
		assert_eq!(listed(&mut announced).await, "present local");
		drop(local);
		assert_eq!(listed(&mut announced).await, "absent local");
	}
}
