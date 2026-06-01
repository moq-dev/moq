use std::task::Poll;

use hang::Catalog;

use crate::Result;

/// A catalog consumer, used to receive catalog updates and discover tracks.
///
/// This wraps a `moq_net::TrackSubscriber` and automatically deserializes JSON
/// catalog data to discover available audio and video tracks in a broadcast.
#[derive(Clone)]
pub struct Consumer {
	/// Access to the underlying track consumer.
	pub track: moq_net::TrackSubscriber,
	group: Option<moq_net::GroupConsumer>,
}

impl Consumer {
	/// Create a new catalog consumer from a MoQ track consumer.
	pub fn new(track: moq_net::TrackSubscriber) -> Self {
		Self { track, group: None }
	}

	/// Poll for the next catalog update.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Catalog>>> {
		// Drain pending groups, keeping only the newest. Remember whether the track is done
		// so we can distinguish "more groups may arrive" from "no more groups, ever".
		let track_finished = loop {
			match self.track.poll_next_group(waiter)? {
				Poll::Ready(Some(group)) => self.group = Some(group),
				Poll::Ready(None) => break true,
				Poll::Pending => break false,
			}
		};

		if let Some(group) = &mut self.group {
			match group.poll_read_frame(waiter)? {
				Poll::Ready(Some(frame)) => {
					self.group = None;
					return Poll::Ready(Ok(Some(Catalog::from_slice(&frame)?)));
				}
				Poll::Ready(None) => self.group = None,
				Poll::Pending => return Poll::Pending,
			}
		}

		if track_finished {
			Poll::Ready(Ok(None))
		} else {
			Poll::Pending
		}
	}

	/// Get the next catalog update.
	///
	/// This method waits for the next catalog publication and returns the
	/// catalog data. If there are no more updates, `None` is returned.
	pub async fn next(&mut self) -> Result<Option<Catalog>> {
		kio::wait(|waiter| self.poll_next(waiter)).await
	}
}

impl From<moq_net::TrackSubscriber> for Consumer {
	fn from(inner: moq_net::TrackSubscriber) -> Self {
		Self::new(inner)
	}
}

#[cfg(test)]
mod test {
	use std::task::Poll;

	use super::*;

	fn catalog_payload(name: &str) -> (Catalog, String) {
		let catalog = Catalog {
			user: Some(hang::catalog::User {
				name: Some(name.to_string()),
				..Default::default()
			}),
			..Default::default()
		};
		let payload = catalog.to_string().expect("catalog should serialize");
		(catalog, payload)
	}

	fn expect_catalog(result: Poll<Result<Option<Catalog>>>) -> Catalog {
		match result {
			Poll::Ready(Ok(Some(decoded))) => decoded,
			other => panic!("expected catalog payload, got {other:?}"),
		}
	}

	#[test]
	fn waits_for_pending_catalog_group_payload() {
		let mut track = Catalog::default_track().produce();
		let mut consumer = Consumer::new(track.subscribe_default());
		let mut group = track.append_group().expect("catalog group should append");

		let waiter = kio::Waiter::noop();
		assert!(matches!(consumer.poll_next(&waiter), Poll::Pending));

		let (catalog, payload) = catalog_payload("pending");
		group.write_frame(payload).expect("catalog frame should write");
		group.finish().expect("catalog group should finish");

		assert_eq!(expect_catalog(consumer.poll_next(&waiter)), catalog);
	}

	#[test]
	fn waits_for_pending_catalog_group_payload_after_track_finish() {
		let mut track = Catalog::default_track().produce();
		let mut consumer = Consumer::new(track.subscribe_default());
		let mut group = track.append_group().expect("catalog group should append");

		track.finish().expect("catalog track should finish");

		let waiter = kio::Waiter::noop();
		assert!(matches!(consumer.poll_next(&waiter), Poll::Pending));

		let (catalog, payload) = catalog_payload("finished");
		group.write_frame(payload).expect("catalog frame should write");
		group.finish().expect("catalog group should finish");

		assert_eq!(expect_catalog(consumer.poll_next(&waiter)), catalog);
	}

	#[test]
	fn returns_latest_complete_catalog_group() {
		let mut track = Catalog::default_track().produce();
		let mut consumer = Consumer::new(track.subscribe_default());
		let waiter = kio::Waiter::noop();

		let (_old, old_payload) = catalog_payload("old");
		let (latest, latest_payload) = catalog_payload("latest");

		let mut old_group = track.append_group().expect("old catalog group should append");
		old_group
			.write_frame(old_payload)
			.expect("old catalog frame should write");
		old_group.finish().expect("old catalog group should finish");

		let mut latest_group = track.append_group().expect("latest catalog group should append");
		latest_group
			.write_frame(latest_payload)
			.expect("latest catalog frame should write");
		latest_group.finish().expect("latest catalog group should finish");
		track.finish().expect("catalog track should finish");

		assert_eq!(expect_catalog(consumer.poll_next(&waiter)), latest);
		assert!(matches!(consumer.poll_next(&waiter), Poll::Ready(Ok(None))));
	}

	#[test]
	fn waits_for_newer_pending_group_instead_of_returning_older_ready_group() {
		let mut track = Catalog::default_track().produce();
		let mut consumer = Consumer::new(track.subscribe_default());
		let waiter = kio::Waiter::noop();

		let (_old, old_payload) = catalog_payload("old");
		let (latest, latest_payload) = catalog_payload("latest");

		let mut old_group = track.append_group().expect("old catalog group should append");
		old_group
			.write_frame(old_payload)
			.expect("old catalog frame should write");
		old_group.finish().expect("old catalog group should finish");

		let mut latest_group = track.append_group().expect("latest catalog group should append");

		assert!(matches!(consumer.poll_next(&waiter), Poll::Pending));

		latest_group
			.write_frame(latest_payload)
			.expect("latest catalog frame should write");
		latest_group.finish().expect("latest catalog group should finish");

		assert_eq!(expect_catalog(consumer.poll_next(&waiter)), latest);
	}

	#[test]
	fn retained_pending_group_is_superseded_by_newer_group() {
		let mut track = Catalog::default_track().produce();
		let mut consumer = Consumer::new(track.subscribe_default());
		let waiter = kio::Waiter::noop();

		let (_old, old_payload) = catalog_payload("old");
		let (latest, latest_payload) = catalog_payload("latest");

		let mut old_group = track.append_group().expect("old catalog group should append");

		assert!(matches!(consumer.poll_next(&waiter), Poll::Pending));

		let mut latest_group = track.append_group().expect("latest catalog group should append");
		latest_group
			.write_frame(latest_payload)
			.expect("latest catalog frame should write");
		latest_group.finish().expect("latest catalog group should finish");
		track.finish().expect("catalog track should finish");

		assert_eq!(expect_catalog(consumer.poll_next(&waiter)), latest);

		old_group
			.write_frame(old_payload)
			.expect("old catalog frame should write");
		old_group.finish().expect("old catalog group should finish");

		assert!(matches!(consumer.poll_next(&waiter), Poll::Ready(Ok(None))));
	}

	#[test]
	fn returns_none_when_empty_track_finishes() {
		let mut track = Catalog::default_track().produce();
		let mut consumer = Consumer::new(track.subscribe_default());
		let waiter = kio::Waiter::noop();

		track.finish().expect("catalog track should finish");

		assert!(matches!(consumer.poll_next(&waiter), Poll::Ready(Ok(None))));
	}
}
