use crate::Catalog;
use crate::Error;

use std::{
	ops::{Deref, DerefMut},
	sync::{Arc, Mutex, MutexGuard},
};

use moq_lite as moq;

/// Produces a catalog track that describes the available media tracks.
///
/// Use [Self::lock] to update the catalog, publishing any changes on [CatalogGuard::drop].
#[derive(Debug, Clone)]
pub struct CatalogProducer {
	track: moq_lite::TrackProducer,
	current: Arc<Mutex<Catalog>>,
}

impl CatalogProducer {
	/// Create a new catalog producer for the given broadcast.
	pub fn new(mut broadcast: moq::BroadcastProducer) -> Self {
		let track = broadcast.create_track(Catalog::default_track(), Catalog::default_delivery());
		Self {
			current: Arc::new(Mutex::new(Catalog::default())),
			track,
		}
	}

	/// Get mutable access to the catalog, publishing it after any changes.
	pub fn lock(&mut self) -> CatalogGuard<'_> {
		CatalogGuard {
			catalog: self.current.lock().unwrap(),
			track: &mut self.track,
		}
	}

	/// Finish publishing to this catalog and close the track.
	pub fn close(mut self) -> Result<(), Error> {
		self.track.close()?;
		Ok(())
	}
}

pub struct CatalogGuard<'a> {
	catalog: MutexGuard<'a, Catalog>,
	track: &'a mut moq_lite::TrackProducer,
}

impl<'a> Deref for CatalogGuard<'a> {
	type Target = Catalog;

	fn deref(&self) -> &Self::Target {
		&self.catalog
	}
}

impl<'a> DerefMut for CatalogGuard<'a> {
	fn deref_mut(&mut self) -> &mut Self::Target {
		&mut self.catalog
	}
}

impl Drop for CatalogGuard<'_> {
	fn drop(&mut self) {
		if let Ok(mut group) = self.track.append_group() {
			// TODO decide if this should return an error, or be impossible to fail
			let frame = self.catalog.to_string().expect("invalid catalog");
			group.write_frame(frame, tokio::time::Instant::now().into()).ok();
			group.close().ok();
		}
	}
}

/// Consumes the catalog track, returning the next catalog update.
pub struct CatalogConsumer {
	broadcast: Option<moq::BroadcastConsumer>,
	track: Option<moq::TrackConsumer>,
	group: Option<moq::GroupConsumer>,
}

impl CatalogConsumer {
	/// Create a new catalog consumer from a broadcast.
	pub fn new(broadcast: moq::BroadcastConsumer) -> Self {
		Self {
			broadcast: Some(broadcast),
			track: None,
			group: None,
		}
	}

	/// Get the next catalog update.
	///
	/// This method waits for the next catalog publication and returns the
	/// catalog data. If there are no more updates, `None` is returned.
	pub async fn next(&mut self) -> Result<Option<Catalog>, Error> {
		if let Some(broadcast) = &mut self.broadcast {
			self.track = Some(broadcast.subscribe_track(Catalog::default_track(), Catalog::default_delivery()));
			self.broadcast = None;
		}

		loop {
			tokio::select! {
				biased;
				Some(track) = async { self.track.as_mut()?.next_group().await.transpose() } => {
					self.group = Some(track?);
				}
				Some(frame) = async { self.group.as_mut()?.read_frame().await.transpose() } => {
					let catalog = Catalog::from_slice(&frame?)?;
					return Ok(Some(catalog));
				}
				else => return Ok(None),
			}
		}
	}
}
