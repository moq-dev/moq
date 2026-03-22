use crate::Result;

use super::CatalogReader;

/// The default name for the catalog track.
pub const DEFAULT_TRACK_NAME: &str = "catalog.json";

/// The default priority for the catalog track.
pub const DEFAULT_TRACK_PRIORITY: u8 = 100;

/// Returns the default track descriptor for the catalog.
pub fn default_track() -> moq_lite::Track {
	moq_lite::Track {
		name: DEFAULT_TRACK_NAME.to_string(),
		priority: DEFAULT_TRACK_PRIORITY,
	}
}

/// A catalog consumer that reads JSON frames from a MoQ track and
/// feeds them into a `CatalogReader` for per-section change notification.
pub struct CatalogConsumer {
	/// Access to the underlying track consumer.
	pub track: moq_lite::TrackConsumer,

	/// The reader that distributes per-section updates.
	reader: CatalogReader,

	group: Option<moq_lite::GroupConsumer>,
}

impl CatalogConsumer {
	/// Create a new catalog consumer from a MoQ track consumer.
	pub fn new(track: moq_lite::TrackConsumer) -> Self {
		Self {
			track,
			reader: CatalogReader::new(),
			group: None,
		}
	}

	/// Get a reference to the reader for registering section interest.
	pub fn reader(&self) -> &CatalogReader {
		&self.reader
	}

	/// Run the background loop that reads frames and dispatches to sections.
	///
	/// This method blocks until the track is closed.
	pub async fn run(&mut self) -> Result<()> {
		loop {
			tokio::select! {
				res = self.track.recv_group() => {
					match res? {
						Some(group) => {
							self.group = Some(group);
						}
						None => {
							self.reader.close();
							return Ok(());
						}
					}
				},
				Some(frame) = async { self.group.as_mut()?.read_frame().await.transpose() } => {
					self.group.take(); // We don't support deltas yet

					let json: serde_json::Map<String, serde_json::Value> =
						serde_json::from_slice(&frame?)?;
					self.reader.update(json);
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
