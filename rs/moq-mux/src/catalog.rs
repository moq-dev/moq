use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex, MutexGuard};

/// Produces both a hang and MSF catalog track for a broadcast.
///
/// The JSON catalog is updated when tracks are added/removed but is *not* automatically published.
/// You'll have to call [`lock`](Self::lock) to update and publish the catalog.
/// Both the hang (`catalog.json`) and MSF (`catalog`) tracks are published on drop of the guard.
#[derive(Clone)]
pub struct CatalogProducer {
	/// Access to the underlying hang catalog track producer.
	pub hang_track: moq_lite::TrackProducer,

	/// Access to the underlying MSF catalog track producer.
	pub msf_track: moq_lite::TrackProducer,

	current: Arc<Mutex<hang::Catalog>>,
}

impl CatalogProducer {
	/// Create a new catalog producer, inserting both catalog tracks into the broadcast.
	pub fn new(broadcast: &mut moq_lite::BroadcastProducer) -> Result<Self, moq_lite::Error> {
		Self::with_catalog(broadcast, hang::Catalog::default())
	}

	/// Create a new catalog producer with the given initial catalog.
	pub fn with_catalog(
		broadcast: &mut moq_lite::BroadcastProducer,
		catalog: hang::Catalog,
	) -> Result<Self, moq_lite::Error> {
		let hang_track = broadcast.create_track(hang::Catalog::default_track())?;
		let msf_track = broadcast.create_track(moq_lite::Track::new(moq_msf::DEFAULT_NAME))?;

		Ok(Self {
			hang_track,
			msf_track,
			current: Arc::new(Mutex::new(catalog)),
		})
	}

	/// Get mutable access to the catalog, publishing it after any changes.
	pub fn lock(&mut self) -> CatalogGuard<'_> {
		CatalogGuard {
			catalog: self.current.lock().unwrap(),
			hang_track: &mut self.hang_track,
			msf_track: &mut self.msf_track,
			updated: false,
		}
	}

	/// Create a consumer for this catalog, receiving updates as they're published.
	pub fn consume(&self) -> hang::CatalogConsumer {
		let subscriber = self
			.hang_track
			.consume()
			.subscribe(hang::Catalog::SUBSCRIPTION)
			.expect("hang_track producer is alive");
		hang::CatalogConsumer::new(subscriber)
	}

	/// Finish publishing to this catalog.
	pub fn finish(&mut self) -> Result<(), moq_lite::Error> {
		self.hang_track.finish()?;
		self.msf_track.finish()?;
		Ok(())
	}
}

/// RAII guard for modifying a catalog with automatic publishing on drop.
///
/// Obtained via [`CatalogProducer::lock`].
///
/// On drop, both the hang and MSF catalog tracks are updated if the catalog was mutated.
pub struct CatalogGuard<'a> {
	catalog: MutexGuard<'a, hang::Catalog>,
	hang_track: &'a mut moq_lite::TrackProducer,
	msf_track: &'a mut moq_lite::TrackProducer,
	updated: bool,
}

impl<'a> Deref for CatalogGuard<'a> {
	type Target = hang::Catalog;

	fn deref(&self) -> &Self::Target {
		&self.catalog
	}
}

impl<'a> DerefMut for CatalogGuard<'a> {
	fn deref_mut(&mut self) -> &mut Self::Target {
		self.updated = true;
		&mut self.catalog
	}
}

impl Drop for CatalogGuard<'_> {
	fn drop(&mut self) {
		if !self.updated {
			return;
		}

		// Publish hang catalog
		let Ok(mut group) = self.hang_track.append_group() else {
			return;
		};
		let frame = self.catalog.to_string().expect("invalid catalog");
		let _ = group.write_frame(frame);
		let _ = group.finish();

		// Publish MSF catalog
		crate::msf::publish(&self.catalog, self.msf_track);
	}
}
