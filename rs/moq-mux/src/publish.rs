//! Bridge from the request-based single-track importers back to a broadcast catalog.
//!
//! A single-track importer (in [`crate::codec`]) produces frames on one track and
//! exposes the catalog renditions it publishes via [`Renditions`]. Most callers,
//! though, work with a whole [`moq_net::BroadcastProducer`] plus a shared
//! [`catalog::Producer`](crate::catalog::Producer). [`Published`] is the adapter:
//! it merges an importer's renditions into that catalog and removes them on drop.
//!
//! For the broadcast-push case, mint a track with
//! [`BroadcastProducer::unique_track`](moq_net::BroadcastProducer::unique_track) and
//! build the importer `from_track`. A [`moq_net::TrackRequest`] (from
//! [`BroadcastDynamic::requested_track`](moq_net::BroadcastDynamic::requested_track))
//! is instead the on-demand path, fed directly to the importer's `new`.

use std::ops::{Deref, DerefMut};

/// A single-track importer that exposes the catalog renditions it publishes.
///
/// Implemented by the per-codec importers so [`Published`] can merge their
/// renditions into a broadcast catalog generically.
pub trait Renditions {
	/// The standalone media catalog (video/audio renditions) this importer publishes.
	fn renditions(&self) -> &hang::Catalog;
}

/// Mint a fresh unique track for a legacy single-codec importer.
///
/// Picks a unique name from `suffix` and sets the microsecond
/// [`hang::container::TIMESCALE`] that the legacy importers stamp their frames
/// with, so the relay gets timing without parsing the payload. Hand the result
/// to the importer's `from_track`.
pub fn unique_track(broadcast: &mut moq_net::BroadcastProducer, suffix: &str) -> crate::Result<moq_net::TrackProducer> {
	let info = moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE);
	Ok(broadcast.unique_track(suffix, info)?)
}

/// A single-track importer attached to a broadcast catalog.
///
/// Merges the importer's [`Renditions`] into a [`catalog::Producer`](crate::catalog::Producer)
/// on creation and removes them on drop. Derefs to the inner importer, so all of
/// its methods (`decode`, `finish`, `seek`, ...) are available directly.
pub struct Published<I: Renditions> {
	inner: I,
	catalog: crate::catalog::Producer,
	video: Vec<String>,
	audio: Vec<String>,
}

impl<I: Renditions> Published<I> {
	/// Merge `inner`'s renditions into `catalog`, publishing the update.
	pub fn new(mut catalog: crate::catalog::Producer, inner: I) -> Self {
		let media = inner.renditions();
		let video: Vec<String> = media.video.renditions.keys().cloned().collect();
		let audio: Vec<String> = media.audio.renditions.keys().cloned().collect();

		{
			let mut guard = catalog.lock();
			for (name, config) in &media.video.renditions {
				guard.video.renditions.insert(name.clone(), config.clone());
			}
			for (name, config) in &media.audio.renditions {
				guard.audio.renditions.insert(name.clone(), config.clone());
			}
		}

		Self {
			inner,
			catalog,
			video,
			audio,
		}
	}
}

impl<I: Renditions> Deref for Published<I> {
	type Target = I;

	fn deref(&self) -> &I {
		&self.inner
	}
}

impl<I: Renditions> DerefMut for Published<I> {
	fn deref_mut(&mut self) -> &mut I {
		&mut self.inner
	}
}

impl<I: Renditions> Drop for Published<I> {
	fn drop(&mut self) {
		let mut guard = self.catalog.lock();
		for name in &self.video {
			guard.video.renditions.remove(name);
		}
		for name in &self.audio {
			guard.audio.renditions.remove(name);
		}
	}
}
