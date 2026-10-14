use super::{AudioConfig, BinaryConfig, JsonConfig, TextConfig, VideoConfig};

/// Where a catalog entry's track lives: the broadcast that publishes it and its name there.
///
/// An entry's key in the catalog is its ID. The track it names is `track` when set and the ID
/// otherwise, in `broadcast` when set and the catalog's own broadcast otherwise. Every reader
/// subscribes through [`track`](Self::track) so the two never diverge.
pub trait Reference {
	/// The broadcast publishing the track, relative to the catalog's; `None` is the catalog's own.
	fn broadcast(&self) -> Option<&moq_net::path::RelativeOwned>;

	/// The track's name in its broadcast, given this entry's ID.
	fn track<'a>(&'a self, id: &'a str) -> &'a str;
}

macro_rules! reference {
	($($config:ty),*) => {$(
		impl Reference for $config {
			fn broadcast(&self) -> Option<&moq_net::path::RelativeOwned> {
				self.broadcast.as_ref()
			}

			fn track<'a>(&'a self, id: &'a str) -> &'a str {
				self.track.as_deref().unwrap_or(id)
			}
		}
	)*};
}

reference!(VideoConfig, AudioConfig, TextConfig, JsonConfig, BinaryConfig);
