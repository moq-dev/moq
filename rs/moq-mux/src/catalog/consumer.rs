use std::task::Poll;

use hang::Catalog;

use crate::Result;

/// A catalog consumer, used to receive catalog updates and discover tracks.
///
/// This wraps a `moq_lite::TrackConsumer` and automatically deserializes JSON
/// catalog data to discover available audio and video tracks in a broadcast.
#[derive(Clone)]
pub struct Consumer {
	/// Access to the underlying track consumer.
	pub track: moq_lite::TrackConsumer,
	group: Option<moq_lite::GroupConsumer>,
}

impl Consumer {
	/// Create a new catalog consumer from a MoQ track consumer.
	pub fn new(track: moq_lite::TrackConsumer) -> Self {
		Self { track, group: None }
	}

	/// Poll for the next catalog update.
	pub fn poll_next(&mut self, waiter: &conducer::Waiter) -> Poll<Result<Option<Catalog>>> {
		// Get the newest group from the track.
		while let Poll::Ready(group) = self.track.poll_next_group(waiter)? {
			self.group = group;

			// We got a None, meaning the track is done.
			if self.group.is_none() {
				return Poll::Ready(Ok(None));
			}
		}

		// If there's no current group, return pending.
		let Some(group) = &mut self.group else {
			return Poll::Pending;
		};

		// Poll for frame from current group.
		if let Poll::Ready(Some(frame)) = group.poll_read_frame(waiter)? {
			self.group.take(); // We don't support deltas yet

			let catalog = Catalog::from_slice(&frame)?;
			Poll::Ready(Ok(Some(catalog)))
		} else {
			self.group.take();
			Poll::Pending
		}
	}

	/// Get the next catalog update.
	///
	/// This method waits for the next catalog publication and returns the
	/// catalog data. If there are no more updates, `None` is returned.
	pub async fn next(&mut self) -> Result<Option<Catalog>> {
		conducer::wait(|waiter| self.poll_next(waiter)).await
	}
}

impl From<moq_lite::TrackConsumer> for Consumer {
	fn from(inner: moq_lite::TrackConsumer) -> Self {
		Self::new(inner)
	}
}
