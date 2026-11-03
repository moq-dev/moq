//! Following a catalog rendition's broadcast across the publisher instances that serve it.
//!
//! An announcement is the online and offline signal, so a reader that restarts on each instance
//! follows [`Instances`] rather than pinning one broadcast as a [`Binding`](crate::Binding) does.

use std::task::Poll;

/// The publisher instances serving a rendition's broadcast, as each comes online, takes over,
/// and goes. Created by [`Source::follow`](crate::Source::follow).
///
/// A sibling broadcast is followed through [`Following`](moq_net::origin::Following); the
/// catalog's own broadcast is handed out once and its end never reported, since a reader
/// follows its catalog's end itself.
pub struct Instances(Feed);

enum Feed {
	/// Every instance announced at a path.
	Following(Box<moq_net::origin::Following>),
	/// One instance, handed out once.
	Fixed(Option<moq_net::broadcast::Consumer>),
}

impl Instances {
	pub(crate) fn following(following: moq_net::origin::Following) -> Self {
		Self(Feed::Following(Box::new(following)))
	}

	pub(crate) fn fixed(broadcast: moq_net::broadcast::Consumer) -> Self {
		Self(Feed::Fixed(Some(broadcast)))
	}

	/// Poll for the next change, registering `waiter` when there is none yet.
	///
	/// Returns each instance as it comes online or takes over, and `None` once the path goes
	/// offline; see [`Following::available`](moq_net::origin::Following::available). Fails
	/// once the origin closes, and with any resolution failure, such as a handler refusing the
	/// path.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<crate::Result<Option<moq_net::broadcast::Consumer>>> {
		match &mut self.0 {
			Feed::Following(following) => following.poll_available(waiter).map_err(crate::Error::from),
			Feed::Fixed(broadcast) => match broadcast.take() {
				Some(broadcast) => Poll::Ready(Ok(Some(broadcast))),
				None => Poll::Pending,
			},
		}
	}

	/// The next change; see [`Self::poll_next`].
	pub async fn next(&mut self) -> crate::Result<Option<moq_net::broadcast::Consumer>> {
		kio::wait(|waiter| self.poll_next(waiter)).await
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::source::{produce_origin, resolved};
	use moq_net::path::Relative;

	/// Let the origin's driver run the fronts that announcements started.
	async fn settle() {
		for _ in 0..10 {
			tokio::task::yield_now().await;
		}
	}

	fn route(instance: &moq_net::Instance) -> moq_net::origin::Route {
		moq_net::origin::Route::default().with_instance(instance.clone(), 0)
	}

	/// The next change a follower reports, which must arrive within the origin's update hold.
	async fn followed(follow: &mut Instances) -> Option<moq_net::broadcast::Consumer> {
		kio::clock::timeout(std::time::Duration::from_secs(1), follow.next())
			.await
			.expect("no change")
			.expect("the follow failed")
	}

	/// The follower stays quiet past the origin's update hold.
	async fn quiet(follow: &mut Instances) {
		if kio::clock::timeout(std::time::Duration::from_secs(1), follow.next())
			.await
			.is_ok()
		{
			panic!("expected no change");
		}
	}

	/// A sibling announced after the follow starts it, its end idles it, and its return starts
	/// it again.
	#[tokio::test(start_paused = true)]
	async fn a_follow_starts_late_and_survives_an_end() {
		let origin = produce_origin();
		let _catalog = origin.publish("a/pub", Default::default()).unwrap();
		settle().await;
		let source = resolved(&origin, "a/pub").await;
		let mut follow = source.follow(Some(&Relative::new("./source").unwrap()), None).unwrap();
		quiet(&mut follow).await;

		let first = origin.publish("a/source", Default::default()).unwrap();
		let started = followed(&mut follow).await.expect("a start");
		assert_eq!(started.info().path.as_str(), "a/source");

		drop(first);
		assert!(followed(&mut follow).await.is_none(), "an end");
		quiet(&mut follow).await;

		let _second = origin.publish("a/source", Default::default()).unwrap();
		let restarted = followed(&mut follow).await.expect("a start");
		assert!(!restarted.is_clone(&started));
	}

	/// A replacement restarts a reference that names no instance, and ends one that names the
	/// old instance until that stream serves again.
	#[tokio::test(start_paused = true)]
	async fn a_follow_restarts_only_without_an_instance() {
		let origin = produce_origin();
		let (old_instance, new_instance) = (moq_net::Instance::random(), moq_net::Instance::random());
		let _catalog = origin.publish("a/pub", Default::default()).unwrap();
		let _old = origin.publish("a/source", route(&old_instance)).unwrap();
		settle().await;
		let source = resolved(&origin, "a/pub").await;
		let rel = Relative::new("./source").unwrap();
		let mut any = source.follow(Some(&rel), None).unwrap();
		let mut named = source.follow(Some(&rel), Some(&old_instance)).unwrap();
		let old = followed(&mut any).await.unwrap();
		assert!(followed(&mut named).await.unwrap().is_clone(&old));

		let new = origin.publish("a/source", route(&new_instance)).unwrap();
		let replaced = followed(&mut any).await.unwrap();
		assert_eq!(replaced.info().instance.as_ref(), Some(&new_instance));
		assert!(followed(&mut named).await.is_none(), "another instance ends it");
		quiet(&mut named).await;
		// The old instance still serves the subscriptions already on it.
		assert!(!old.is_closed());

		// The replacement goes and the old instance wins again.
		drop(new);
		let back = followed(&mut named).await.unwrap();
		assert_eq!(back.info().instance.as_ref(), Some(&old_instance));
		let back = followed(&mut any).await.unwrap();
		assert_eq!(back.info().instance.as_ref(), Some(&old_instance));
	}

	/// A self-reference reads only the catalog instance, whatever replaces it at the path.
	#[tokio::test(start_paused = true)]
	async fn a_follow_of_the_catalog_keeps_its_instance() {
		let origin = produce_origin();
		let old = origin.publish("a/pub", Default::default()).unwrap();
		settle().await;
		let source = resolved(&origin, "a/pub").await;
		let mut follow = source.follow(Some(&Relative::new("./pub").unwrap()), None).unwrap();
		assert!(followed(&mut follow).await.unwrap().is_clone(&source.broadcast()));

		let _new = origin.publish("a/pub", Default::default()).unwrap();
		quiet(&mut follow).await;
		// Its end is the catalog's, which the catalog's reader sees.
		drop(old);
		quiet(&mut follow).await;
	}
}
