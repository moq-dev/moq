use std::task::Poll;

use crate::{Catalog, Result};

/// A catalog consumer, used to receive catalog updates and discover tracks.
///
/// This wraps a `moq_lite::TrackSubscriber` and automatically deserializes JSON
/// catalog data to discover available audio and video tracks in a broadcast.
pub struct CatalogConsumer {
	/// Access to the underlying track subscriber.
	pub track: moq_lite::TrackSubscriber,
	group: Option<moq_lite::GroupConsumer>,
}

impl CatalogConsumer {
	/// Create a new catalog consumer from a MoQ track subscriber.
	pub fn new(track: moq_lite::TrackSubscriber) -> Self {
		Self { track, group: None }
	}

	/// Poll for the next catalog update.
	pub fn poll_next(&mut self, waiter: &moq_lite::conducer::Waiter) -> Poll<Result<Option<Catalog>>> {
		// Get the newest group from the track.
		while let Poll::Ready(group) = self.track.poll_next_group(waiter)? {
			self.group = group;
			if self.group.is_none() {
				return Poll::Ready(Ok(None));
			}
		}

		let Some(group) = &mut self.group else {
			return Poll::Pending;
		};

		// Poll for frame from current group.
		match group.poll_read_frame(waiter)? {
			Poll::Ready(Some(frame)) => {
				self.group.take(); // We don't support deltas yet
				let catalog = Catalog::from_slice(&frame)?;
				Poll::Ready(Ok(Some(catalog)))
			}
			_ => Poll::Pending,
		}
	}

	/// Get the next catalog update.
	///
	/// This method waits for the next catalog publication and returns the
	/// catalog data. If there are no more updates, `None` is returned.
	pub async fn next(&mut self) -> Result<Option<Catalog>> {
		moq_lite::conducer::wait(|waiter| self.poll_next(waiter)).await
	}
}

impl From<moq_lite::TrackSubscriber> for CatalogConsumer {
	fn from(inner: moq_lite::TrackSubscriber) -> Self {
		Self::new(inner)
	}
}
