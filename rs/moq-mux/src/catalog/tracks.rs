use std::collections::BTreeMap;

use super::Producer;
use super::hang::{Catalog, CatalogExt};

/// A catalog extension that owns one kind of custom track rendition.
pub trait CustomTrackExt: CatalogExt {
	/// The custom rendition config stored in the catalog.
	type Config;

	/// The rendition map owned by this extension.
	fn renditions(&mut self) -> &mut BTreeMap<String, Self::Config>;

	/// The optional timeline field to populate when publishing a config.
	fn timeline(_config: &mut Self::Config) -> Option<&mut Option<hang::catalog::Timeline>> {
		None
	}
}

struct Track<E: CatalogExt, C> {
	catalog: Producer<E>,
	name: String,
	renditions: fn(&mut Catalog<E>) -> &mut BTreeMap<String, C>,
	timeline: fn(&mut C) -> Option<&mut Option<hang::catalog::Timeline>>,
	// A lazily-configured importer can hold the handle before publishing a config.
	present: bool,
}

impl<E: CatalogExt, C> Track<E, C> {
	fn new(
		catalog: Producer<E>,
		name: impl Into<String>,
		renditions: fn(&mut Catalog<E>) -> &mut BTreeMap<String, C>,
		timeline: fn(&mut C) -> Option<&mut Option<hang::catalog::Timeline>>,
	) -> Self {
		Self {
			catalog,
			name: name.into(),
			renditions,
			timeline,
			present: false,
		}
	}

	fn name(&self) -> &str {
		&self.name
	}

	fn timestamp(&self, hint: Option<crate::container::Timestamp>) -> crate::Result<crate::container::Timestamp> {
		self.catalog.timestamp(hint)
	}

	fn set(&mut self, mut config: C) {
		if let Some(timeline) = (self.timeline)(&mut config) {
			*timeline = Some(self.catalog.timeline_section(&self.name));
		}
		(self.renditions)(&mut self.catalog.lock()).insert(self.name.clone(), config);
		self.present = true;
	}

	fn update(&mut self, f: impl FnOnce(&mut C)) {
		if !self.present {
			return;
		}
		let mut guard = self.catalog.lock();
		if let Some(config) = (self.renditions)(&mut guard).get_mut(&self.name) {
			f(config);
		}
	}
}

impl<E: CatalogExt, C> Drop for Track<E, C> {
	fn drop(&mut self) {
		if self.present {
			(self.renditions)(&mut self.catalog.lock()).remove(&self.name);
		}
	}
}

/// A single custom track's catalog rendition, retired on drop.
///
/// Made via [`Producer::custom_track`]. [`set`](Self::set) inserts or replaces
/// the config, [`update`](Self::update) refines it in place, and dropping the
/// handle removes it. Every mutation automatically publishes the shared catalog.
pub struct CustomTrack<E: CustomTrackExt> {
	inner: Track<E, E::Config>,
}

impl<E: CustomTrackExt> CustomTrack<E> {
	pub(super) fn new(catalog: Producer<E>, name: impl Into<String>) -> Self {
		Self {
			inner: Track::new(catalog, name, |catalog| catalog.ext.renditions(), E::timeline),
		}
	}

	/// The track name this rendition is keyed by.
	pub fn name(&self) -> &str {
		self.inner.name()
	}

	/// Resolve a timestamp on the broadcast's shared clock (see [`Producer::timestamp`]).
	pub fn timestamp(&self, hint: Option<crate::container::Timestamp>) -> crate::Result<crate::container::Timestamp> {
		self.inner.timestamp(hint)
	}

	/// Insert or replace the rendition, publishing the catalog.
	pub fn set(&mut self, config: E::Config) {
		self.inner.set(config);
	}

	/// Refine the rendition in place, publishing if present.
	pub fn update(&mut self, f: impl FnOnce(&mut E::Config)) {
		self.inner.update(f);
	}
}

/// A single video track's catalog rendition, retired on drop.
pub struct VideoTrack<E: CatalogExt = ()> {
	inner: Track<E, hang::catalog::VideoConfig>,
}

impl<E: CatalogExt> VideoTrack<E> {
	pub(super) fn new(catalog: Producer<E>, name: impl Into<String>) -> Self {
		Self {
			inner: Track::new(
				catalog,
				name,
				|catalog| &mut catalog.video.renditions,
				|config| Some(&mut config.timeline),
			),
		}
	}

	/// The track name this rendition is keyed by.
	pub fn name(&self) -> &str {
		self.inner.name()
	}

	/// Resolve a timestamp on the broadcast's shared clock (see [`Producer::timestamp`]).
	pub fn timestamp(&self, hint: Option<crate::container::Timestamp>) -> crate::Result<crate::container::Timestamp> {
		self.inner.timestamp(hint)
	}

	/// Insert or replace the rendition, publishing the catalog and advertising its timeline.
	pub fn set(&mut self, config: hang::catalog::VideoConfig) {
		self.inner.set(config);
	}

	/// Refine the rendition in place, publishing if present.
	pub fn update(&mut self, f: impl FnOnce(&mut hang::catalog::VideoConfig)) {
		self.inner.update(f);
	}
}

/// A single audio track's catalog rendition, retired on drop.
pub struct AudioTrack<E: CatalogExt = ()> {
	inner: Track<E, hang::catalog::AudioConfig>,
}

impl<E: CatalogExt> AudioTrack<E> {
	pub(super) fn new(catalog: Producer<E>, name: impl Into<String>) -> Self {
		Self {
			inner: Track::new(
				catalog,
				name,
				|catalog| &mut catalog.audio.renditions,
				|config| Some(&mut config.timeline),
			),
		}
	}

	/// The track name this rendition is keyed by.
	pub fn name(&self) -> &str {
		self.inner.name()
	}

	/// Resolve a timestamp on the broadcast's shared clock (see [`Producer::timestamp`]).
	pub fn timestamp(&self, hint: Option<crate::container::Timestamp>) -> crate::Result<crate::container::Timestamp> {
		self.inner.timestamp(hint)
	}

	/// Insert or replace the rendition, publishing the catalog and advertising its timeline.
	pub fn set(&mut self, config: hang::catalog::AudioConfig) {
		self.inner.set(config);
	}

	/// Refine the rendition in place, publishing if present.
	pub fn update(&mut self, f: impl FnOnce(&mut hang::catalog::AudioConfig)) {
		self.inner.update(f);
	}
}

#[cfg(test)]
mod test {
	use std::task::Poll;

	use serde::{Deserialize, Serialize};

	use super::*;

	#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
	struct TestExt {
		data: TestSection,
	}

	impl CatalogExt for TestExt {}

	impl CustomTrackExt for TestExt {
		type Config = TestConfig;

		fn renditions(&mut self) -> &mut BTreeMap<String, Self::Config> {
			&mut self.data.renditions
		}

		fn timeline(config: &mut Self::Config) -> Option<&mut Option<hang::catalog::Timeline>> {
			Some(&mut config.timeline)
		}
	}

	#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
	struct TestSection {
		renditions: BTreeMap<String, TestConfig>,
	}

	#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
	struct TestConfig {
		value: u64,
		#[serde(skip_serializing_if = "Option::is_none")]
		timeline: Option<hang::catalog::Timeline>,
	}

	#[test]
	fn custom_track_manages_rendition() {
		let mut broadcast = moq_net::Broadcast::new().produce();
		let catalog = Producer::with_catalog(&mut broadcast, Catalog::<TestExt>::default()).unwrap();
		let mut consumer = catalog.consume().unwrap();
		let waiter = kio::Waiter::noop();

		let mut track = catalog.custom_track("data0");
		track.update(|config| config.value = 1);
		assert!(catalog.snapshot().data.renditions.is_empty());
		assert!(matches!(consumer.poll_next(&waiter), Poll::Pending));

		track.set(TestConfig {
			value: 2,
			timeline: None,
		});
		let snapshot = match consumer.poll_next(&waiter) {
			Poll::Ready(Ok(Some(catalog))) => catalog,
			other => panic!("expected custom rendition catalog, got {other:?}"),
		};
		let config = snapshot.data.renditions.get("data0").unwrap();
		assert_eq!(config.value, 2);
		assert_eq!(config.timeline.as_ref().unwrap().track, "data0.timeline.z");

		track.update(|config| config.value = 3);
		let snapshot = match consumer.poll_next(&waiter) {
			Poll::Ready(Ok(Some(catalog))) => catalog,
			other => panic!("expected updated custom rendition catalog, got {other:?}"),
		};
		assert_eq!(snapshot.data.renditions["data0"].value, 3);

		drop(track);
		let snapshot = match consumer.poll_next(&waiter) {
			Poll::Ready(Ok(Some(catalog))) => catalog,
			other => panic!("expected retired custom rendition catalog, got {other:?}"),
		};
		assert!(snapshot.data.renditions.is_empty());
	}
}
