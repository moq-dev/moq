use hang::catalog::{Audio, Section, Video};
use serde::Serialize;

/// Produces both a hang and MSF catalog track for a broadcast.
///
/// Wraps a [`hang::CatalogWriter`] and publishes updates to both
/// the hang (`catalog.json`) and MSF (`catalog`) tracks when [`flush`](Self::flush) is called.
#[derive(Clone)]
pub struct CatalogProducer {
	/// Access to the underlying hang catalog track producer.
	pub hang_track: moq_lite::TrackProducer,

	/// Access to the underlying MSF catalog track producer.
	pub msf_track: moq_lite::TrackProducer,

	writer: hang::CatalogWriter,
}

impl CatalogProducer {
	/// Create a new catalog producer, inserting both catalog tracks into the broadcast.
	pub fn new(broadcast: &moq_lite::BroadcastProducer) -> Result<Self, moq_lite::Error> {
		let hang_track = broadcast.create_track(hang::catalog::default_track())?;
		let msf_track = broadcast.create_track(moq_lite::Track {
			name: moq_msf::DEFAULT_NAME.to_string(),
		})?;

		Ok(Self {
			hang_track,
			msf_track,
			writer: hang::CatalogWriter::new(),
		})
	}

	/// Set a typed section in the catalog.
	///
	/// This does NOT publish the update. Call [`flush`](Self::flush) to publish.
	pub fn set<T: Serialize>(&self, section: &Section<T>, value: &T) -> Result<(), hang::Error> {
		self.writer.set(section, value)
	}

	/// Remove a section from the catalog by name.
	///
	/// This does NOT publish the update. Call [`flush`](Self::flush) to publish.
	pub fn remove(&self, name: &str) -> Result<(), hang::Error> {
		self.writer.remove(name)
	}

	/// Publish the current catalog state to both the hang and MSF tracks.
	pub fn flush(&mut self) {
		// Publish hang catalog
		let Ok(encoded) = self.writer.encode() else {
			return;
		};
		let Ok(mut group) = self.hang_track.append_group() else {
			return;
		};
		let _ = group.write_frame(encoded);
		let _ = group.finish();

		// Publish MSF catalog
		// Read video and audio sections from the writer state for MSF conversion.
		let state = self.writer.read();
		let video: Option<Video> = state
			.sections
			.get("video")
			.and_then(|v| serde_json::from_value(v.clone()).ok());
		let audio: Option<Audio> = state
			.sections
			.get("audio")
			.and_then(|v| serde_json::from_value(v.clone()).ok());
		drop(state);

		crate::msf::publish(video.as_ref(), audio.as_ref(), &mut self.msf_track);
	}

	/// Get a reference to the underlying [`hang::CatalogWriter`].
	pub fn writer(&self) -> &hang::CatalogWriter {
		&self.writer
	}

	/// Create a consumer for the hang catalog track, receiving updates as they're published.
	pub fn consume(&self) -> hang::CatalogConsumer {
		let track = self.hang_track.consume();
		let subscriber = track.subscribe(moq_lite::Subscription::default()).unwrap();
		hang::CatalogConsumer::new(subscriber)
	}

	/// Finish publishing to this catalog.
	pub fn finish(&mut self) -> Result<(), moq_lite::Error> {
		self.writer.close();
		self.hang_track.finish()?;
		self.msf_track.finish()?;
		Ok(())
	}
}
