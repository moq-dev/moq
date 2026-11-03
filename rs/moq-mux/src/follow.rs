//! Following a broadcast across the publisher instances that serve it.
//!
//! An announcement is the online and offline signal, so a reader that restarts on each instance
//! follows [`Instances`] rather than pinning one broadcast as a [`Binding`](crate::Binding) does.

use std::task::Poll;

/// The publisher instances serving a broadcast, as each comes online, takes over, and goes.
///
/// Built on [`follow`](moq_net::origin::Consumer::follow). Create one
/// with [`Source::follow`](crate::Source::follow) for a catalog reference, or [`Instances::new`]
/// for a path.
pub struct Instances {
	feed: Feed,
	/// The instance a reference named, if any.
	instance: Option<moq_net::Instance>,
	/// The instance handed out last, until it stops serving.
	current: Option<moq_net::broadcast::Consumer>,
}

enum Feed {
	/// Every instance announced at a path.
	Announced(Box<moq_net::origin::Following>),
	/// One instance, handed out once.
	Fixed(Option<moq_net::broadcast::Consumer>),
}

/// A change to the instance [`Instances`] serves.
pub enum Event {
	/// Start on this instance, dropping whatever was read from the one before.
	Start(moq_net::broadcast::Consumer),
	/// The instance handed out last stopped serving: idle until the next start.
	End,
}

impl Instances {
	/// Follow the broadcast at `path` through `origin`, handing out only `instance` when given.
	///
	/// Unlike [`Source::follow`](crate::Source::follow), this needs no catalog broadcast, so a
	/// reader can follow the broadcast whose catalog it reads and build a
	/// [`Source`](crate::Source) for each instance.
	pub fn new(
		origin: &moq_net::origin::Consumer,
		path: impl TryInto<moq_net::Path, Error: Into<moq_net::Error>>,
		instance: Option<&moq_net::Instance>,
	) -> crate::Result<Self> {
		let announced = origin.follow(path, None)?;
		Ok(Self::with(Feed::Announced(Box::new(announced)), instance))
	}

	/// Hand out `broadcast` once and never report its end, which its owner follows.
	pub(crate) fn fixed(broadcast: moq_net::broadcast::Consumer) -> Self {
		Self::with(Feed::Fixed(Some(broadcast)), None)
	}

	fn with(feed: Feed, instance: Option<&moq_net::Instance>) -> Self {
		Self {
			feed,
			instance: instance.cloned(),
			current: None,
		}
	}

	/// Poll for the next change, registering `waiter` when there is none yet.
	///
	/// Each announcement that brings an instance hands it out, so the same one returns after an
	/// end. Fails once the origin closes, and with any resolution failure, such as a handler
	/// refusing the path.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<crate::Result<Event>> {
		loop {
			let next = match &mut self.feed {
				// Followed unpinned and checked here, so another instance taking over ends the
				// rendition rather than waiting for the named one to come back.
				Feed::Announced(announced) => announced.poll_available(waiter).map_err(crate::Error::from),
				Feed::Fixed(broadcast) => match broadcast.take() {
					Some(broadcast) => Poll::Ready(Ok(broadcast)),
					None => Poll::Pending,
				},
			};
			if let Poll::Ready(next) = next {
				let broadcast = next?;
				if crate::source::check_instance(self.instance.as_ref(), &broadcast).is_err() {
					tracing::debug!(path = %broadcast.info().path, "skipping a broadcast of another instance");
					match self.current.take() {
						Some(_) => return Poll::Ready(Ok(Event::End)),
						None => continue,
					}
				}
				// A fixed instance ends with its owner, which follows that itself.
				if let Feed::Announced(_) = self.feed {
					self.current = Some(broadcast.clone());
				}
				return Poll::Ready(Ok(Event::Start(broadcast)));
			}

			if let Some(current) = &self.current
				&& current.poll_closed(waiter).is_ready()
			{
				self.current = None;
				return Poll::Ready(Ok(Event::End));
			}
			return Poll::Pending;
		}
	}

	/// The next change; see [`Self::poll_next`].
	pub async fn next(&mut self) -> crate::Result<Event> {
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
		let next = kio::clock::timeout(std::time::Duration::from_secs(1), follow.next())
			.await
			.expect("no change")
			.expect("the follow failed");
		match next {
			Event::Start(broadcast) => Some(broadcast),
			Event::End => None,
		}
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
