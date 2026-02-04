use crate::{Catalog, Result};

/// A catalog consumer, used to receive catalog updates and discover tracks.
///
/// This wraps a `moq_lite::TrackConsumer` and automatically deserializes JSON
/// catalog data to discover available audio and video tracks in a broadcast.
#[derive(Clone)]
pub struct CatalogConsumer {
	/// Access to the underlying track consumer.
	pub track: moq_lite::TrackConsumer,
	group: Option<moq_lite::GroupConsumer>,
}

impl CatalogConsumer {
	/// Create a new catalog consumer from a MoQ track consumer.
	pub fn new(track: moq_lite::TrackConsumer) -> Self {
		Self { track, group: None }
	}

	/// Get the next catalog update.
	///
	/// This method waits for the next catalog publication and returns the
	/// catalog data. If there are no more updates, `None` is returned.
	pub async fn next(&mut self) -> Result<Option<Catalog>> {
		loop {
			tokio::select! {
				res = self.track.next_group() => {
					match res? {
						Some(group) => {
							// Use the new group.
							self.group = Some(group);
						}
						// The track has ended, so we should return None.
						None => return Ok(None),
					}
				},
				Some(frame) = async { self.group.as_mut()?.read_frame().await.transpose() } => {
					self.group.take(); // We don't support deltas yet
					let catalog = Catalog::from_slice(&frame?)?;
					return Ok(Some(catalog));
				}
			}
		}
	}

	/// Wait until the catalog track is closed.
	pub async fn closed(&self) -> Result<()> {
		Ok(self.track.closed().await?)
	}
}

impl From<moq_lite::TrackConsumer> for CatalogConsumer {
	fn from(inner: moq_lite::TrackConsumer) -> Self {
		Self::new(inner)
	}
}
