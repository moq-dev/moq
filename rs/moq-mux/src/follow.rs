//! Following a catalog rendition's broadcast across the publisher identities that serve it.
//!
//! An announcement is the online and offline signal, so a reader that restarts on each identity
//! follows [`Identities`] rather than pinning one broadcast as a [`Binding`](crate::Binding) does.

use std::task::{Poll, ready};

/// The publisher identities serving a rendition's broadcast, as each comes online, takes over,
/// and goes. Created by [`Source::follow`](crate::Source::follow).
///
/// A sibling broadcast is followed through [`Following`](moq_net::origin::Following); the
/// catalog's own broadcast is handed out once and its end never reported, since a reader
/// follows its catalog's end itself.
pub struct Identities(Feed);

enum Feed {
	/// Every identity announced at a path.
	Following {
		following: Box<moq_net::origin::Following>,
		/// An identity was handed out and its end not reported yet.
		live: bool,
	},
	/// One identity, handed out once.
	Fixed(Option<moq_net::broadcast::Consumer>),
}

impl Identities {
	/// Follow every identity announced at a path.
	pub(crate) fn following(following: moq_net::origin::Following) -> Self {
		Self(Feed::Following {
			following: Box::new(following),
			live: false,
		})
	}

	pub(crate) fn fixed(broadcast: moq_net::broadcast::Consumer) -> Self {
		Self(Feed::Fixed(Some(broadcast)))
	}

	/// The next change.
	///
	/// Returns each identity as it comes online or takes over, and `None` once the path goes
	/// offline; see
	/// [`Following::available`](moq_net::origin::Following::available). Fails once the origin
	/// closes, and with any resolution failure, such as a handler refusing the path; the call
	/// after a refusal waits for the serving route to change.
	#[cfg_attr(feature = "async", kio::wrapper)]
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<crate::Result<Option<moq_net::broadcast::Consumer>>> {
		let (following, live) = match &mut self.0 {
			Feed::Following { following, live } => (following, live),
			Feed::Fixed(broadcast) => {
				return match broadcast.take() {
					Some(broadcast) => Poll::Ready(Ok(Some(broadcast))),
					None => Poll::Pending,
				};
			}
		};
		loop {
			if let Some(broadcast) = ready!(following.poll_available(waiter))? {
				*live = true;
				return Poll::Ready(Ok(Some(broadcast)));
			}
			// Report the end of what was handed out once.
			if std::mem::take(live) {
				return Poll::Ready(Ok(None));
			}
		}
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

	fn route(identity: &moq_net::Identity) -> moq_net::origin::RouteOptions {
		moq_net::origin::RouteOptions::default().with_identity(identity.clone(), 0)
	}

	/// The next change a follower reports, which must arrive within the origin's update hold.
	async fn followed(follow: &mut Identities) -> Option<moq_net::broadcast::Consumer> {
		kio::clock::timeout(std::time::Duration::from_secs(1), follow.next())
			.await
			.expect("no change")
			.expect("the follow failed")
	}

	/// The follower stays quiet past the origin's update hold.
	async fn quiet(follow: &mut Identities) {
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
		let _catalog = origin.route("a/pub", Default::default()).unwrap();
		settle().await;
		let source = resolved(&origin, "a/pub").await;
		let mut follow = source.follow(Some(&Relative::new("./source").unwrap())).unwrap();
		quiet(&mut follow).await;

		let first = origin.route("a/source", Default::default()).unwrap();
		let started = followed(&mut follow).await.expect("a start");
		assert_eq!(started.info().path.as_str(), "a/source");

		drop(first);
		assert!(followed(&mut follow).await.is_none(), "an end");
		quiet(&mut follow).await;

		let _second = origin.route("a/source", Default::default()).unwrap();
		let restarted = followed(&mut follow).await.expect("a start");
		assert!(!restarted.is_clone(&started));
	}

	/// A replacement restarts the follow, and its end hands the old identity out again.
	#[tokio::test(start_paused = true)]
	async fn a_follow_restarts_on_a_replacement() {
		let origin = produce_origin();
		let (old_identity, new_identity) = (moq_net::Identity::random(), moq_net::Identity::random());
		let _catalog = origin.route("a/pub", Default::default()).unwrap();
		let _old = origin.route("a/source", route(&old_identity)).unwrap();
		settle().await;
		let source = resolved(&origin, "a/pub").await;
		let rel = Relative::new("./source").unwrap();
		let mut follow = source.follow(Some(&rel)).unwrap();
		let old = followed(&mut follow).await.unwrap();
		assert_eq!(old.info().identity.as_ref(), Some(&old_identity));

		let new = origin.route("a/source", route(&new_identity)).unwrap();
		let replaced = followed(&mut follow).await.unwrap();
		assert_eq!(replaced.info().identity.as_ref(), Some(&new_identity));
		// The old identity still serves the subscriptions already on it.
		assert!(!old.is_closed());

		// The replacement goes and the old identity wins again.
		drop(new);
		let back = followed(&mut follow).await.unwrap();
		assert_eq!(back.info().identity.as_ref(), Some(&old_identity));
	}

	/// A self-reference reads only the catalog identity, whatever replaces it at the path.
	#[tokio::test(start_paused = true)]
	async fn a_follow_of_the_catalog_keeps_its_identity() {
		let origin = produce_origin();
		let old = origin.route("a/pub", Default::default()).unwrap();
		settle().await;
		let source = resolved(&origin, "a/pub").await;
		let mut follow = source.follow(Some(&Relative::new("./pub").unwrap())).unwrap();
		assert!(followed(&mut follow).await.unwrap().is_clone(&source.broadcast()));

		let _new = origin.route("a/pub", Default::default()).unwrap();
		quiet(&mut follow).await;
		// Its end is the catalog's, which the catalog's reader sees.
		drop(old);
		quiet(&mut follow).await;
	}
}
