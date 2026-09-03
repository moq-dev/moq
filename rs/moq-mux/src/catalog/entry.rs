//! A borrowed catalog entry: a track's name paired with the config describing it.

use std::ops::Deref;

/// One entry in a catalog section: the track's name and the config describing it.
///
/// A catalog section is a map, so reading one out hands you a key and a value that have to be kept
/// together: the name says what to subscribe to, the config says how to read it. Pairing them means
/// a caller can't accidentally subscribe to one track with another's config, and it only has to
/// name the track once.
///
/// Get one from [`Catalog`](super::hang::Catalog), by name or by iterating a section. It derefs to
/// the config, so the config's fields are reachable directly (`entry.mode`, `entry.compression`).
///
/// The entry is what a consumer is built from: see
/// [`json::Consumer`](crate::json::Consumer) and [`binary::Consumer`](crate::binary::Consumer),
/// each of which adds a `subscribe` to the entry types it can read.
#[derive(Clone, Copy, Debug)]
pub struct Entry<'a, C> {
	name: &'a str,
	config: &'a C,
}

impl<'a, C> Entry<'a, C> {
	pub(crate) fn new(name: &'a str, config: &'a C) -> Self {
		Self { name, config }
	}

	/// The track name, which is the key this entry is stored under.
	pub fn name(&self) -> &'a str {
		self.name
	}

	/// The config describing how to read the track.
	pub fn config(&self) -> &'a C {
		self.config
	}
}

impl<C> Deref for Entry<'_, C> {
	type Target = C;

	fn deref(&self) -> &C {
		self.config
	}
}
