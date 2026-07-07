use moq_mux::catalog::hang::Extra;
use moq_mux::import;

use crate::{Error, Id, NonZeroSlab};

/// A media importer fed whole chunks: either a single codec track or a container
/// that may publish several tracks. The format string picks which at creation.
enum Media {
	// Boxed because the codec splitters/imports make this variant much larger
	// than the (already boxed) container one.
	Track(Box<import::Track<Extra>>),
	Container(import::Container<Extra>),
}

#[derive(Default)]
pub struct Publish {
	/// Active broadcast producers for publishing.
	broadcasts: NonZeroSlab<(moq_net::broadcast::Producer, moq_mux::catalog::Producer<Extra>)>,

	/// Active media encoders/decoders for publishing.
	media: NonZeroSlab<Media>,

	/// Raw track producers (no media/container/catalog framing).
	tracks: NonZeroSlab<moq_net::track::Producer>,

	/// Raw group producers, created from a raw track producer.
	groups: NonZeroSlab<moq_net::group::Producer>,
}

impl Publish {
	pub fn create(&mut self) -> Result<Id, Error> {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog =
			moq_mux::catalog::Producer::with_catalog(&mut broadcast, moq_mux::catalog::hang::Catalog::default())?;

		let id = self.broadcasts.insert((broadcast, catalog))?;
		Ok(id)
	}

	pub fn get(&self, id: Id) -> Result<&moq_net::broadcast::Producer, Error> {
		self.broadcasts
			.get(id)
			.ok_or(Error::BroadcastNotFound)
			.map(|(broadcast, _)| broadcast)
	}

	/// Mutable access to both the broadcast and its catalog producer.
	/// Used by sibling modules (e.g. `audio`) that need to attach a new
	/// track to an existing publish.
	pub fn pair_mut(
		&mut self,
		id: Id,
	) -> Result<
		(
			&mut moq_net::broadcast::Producer,
			&mut moq_mux::catalog::Producer<Extra>,
		),
		Error,
	> {
		let (broadcast, catalog) = self.broadcasts.get_mut(id).ok_or(Error::BroadcastNotFound)?;
		Ok((broadcast, catalog))
	}

	pub fn close(&mut self, broadcast: Id) -> Result<(), Error> {
		self.broadcasts.remove(broadcast).ok_or(Error::BroadcastNotFound)?;
		Ok(())
	}

	pub fn media_ordered(&mut self, broadcast: Id, format: &str, init: &[u8]) -> Result<Id, Error> {
		let (broadcast, catalog) = self.broadcasts.get(broadcast).ok_or(Error::BroadcastNotFound)?;

		// A container may publish several tracks; a single codec fills one reserved
		// track. Try the container first so a codec format doesn't reserve a stray
		// track on the way to being recognized.
		let media = match import::Container::new(broadcast.clone(), catalog.reserve(), format, init) {
			Ok(container) => Media::Container(container),
			Err(moq_mux::Error::UnknownFormat(_)) => {
				let mut broadcast = broadcast.clone();
				let name = broadcast.unique_name(&format!(".{format}"));
				let request = broadcast.reserve_track(name)?;
				match import::Track::new(request, catalog.reserve(), format, init) {
					Ok(track) => Media::Track(Box::new(track)),
					Err(moq_mux::Error::UnknownFormat(_)) => return Err(Error::UnknownFormat(format.to_string())),
					Err(err) => return Err(err.into()),
				}
			}
			Err(err) => return Err(err.into()),
		};

		let id = self.media.insert(media)?;
		Ok(id)
	}

	pub fn media_frame(&mut self, media: Id, data: &[u8], timestamp: hang::container::Timestamp) -> Result<(), Error> {
		let media = self.media.get_mut(media).ok_or(Error::MediaNotFound)?;

		match media {
			Media::Track(track) => track.decode(data, Some(timestamp))?,
			Media::Container(container) => container.decode(data)?,
		}

		Ok(())
	}

	pub fn media_close(&mut self, media: Id) -> Result<(), Error> {
		let mut media = self.media.remove(media).ok_or(Error::MediaNotFound)?;
		match &mut media {
			Media::Track(track) => track.finish()?,
			Media::Container(container) => container.finish()?,
		}
		Ok(())
	}

	/// Insert or replace a video rendition in the broadcast's catalog.
	///
	/// The catalog is republished automatically.
	pub fn video_config(&mut self, broadcast: Id, name: &str, config: hang::catalog::VideoConfig) -> Result<(), Error> {
		let (_, catalog) = self.broadcasts.get_mut(broadcast).ok_or(Error::BroadcastNotFound)?;
		catalog.lock().video.insert(name, config).map_err(Error::Hang)?;
		Ok(())
	}

	/// Insert or replace an audio rendition in the broadcast's catalog.
	///
	/// The catalog is republished automatically.
	pub fn audio_config(&mut self, broadcast: Id, name: &str, config: hang::catalog::AudioConfig) -> Result<(), Error> {
		let (_, catalog) = self.broadcasts.get_mut(broadcast).ok_or(Error::BroadcastNotFound)?;
		catalog.lock().audio.insert(name, config).map_err(Error::Hang)?;
		Ok(())
	}

	/// Remove a video rendition from the broadcast's catalog by name.
	///
	/// The catalog is republished automatically.
	pub fn video_remove(&mut self, broadcast: Id, name: &str) -> Result<(), Error> {
		let (_, catalog) = self.broadcasts.get_mut(broadcast).ok_or(Error::BroadcastNotFound)?;
		catalog.lock().video.remove(name);
		Ok(())
	}

	/// Remove an audio rendition from the broadcast's catalog by name.
	///
	/// The catalog is republished automatically.
	pub fn audio_remove(&mut self, broadcast: Id, name: &str) -> Result<(), Error> {
		let (_, catalog) = self.broadcasts.get_mut(broadcast).ok_or(Error::BroadcastNotFound)?;
		catalog.lock().audio.remove(name);
		Ok(())
	}

	/// Insert or replace a top-level application catalog section by name.
	///
	/// `value` is any JSON document. Errors if `name` is reserved (`video`/`audio`).
	/// The catalog is republished automatically.
	pub fn catalog_section_set(&mut self, broadcast: Id, name: &str, value: serde_json::Value) -> Result<(), Error> {
		let (_, catalog) = self.broadcasts.get_mut(broadcast).ok_or(Error::BroadcastNotFound)?;
		catalog.lock().set_section(name.to_string(), value)?;
		Ok(())
	}

	/// Remove a top-level application catalog section by name.
	///
	/// A no-op if no section with that name exists. Republishes the catalog if it did.
	pub fn catalog_section_remove(&mut self, broadcast: Id, name: &str) -> Result<(), Error> {
		let (_, catalog) = self.broadcasts.get_mut(broadcast).ok_or(Error::BroadcastNotFound)?;
		catalog.lock().remove_section(name);
		Ok(())
	}

	/// Create a raw track on a broadcast for arbitrary byte payloads.
	///
	/// No codec, container, or catalog framing. This is the moq-net primitive
	/// for non-media tracks. Pair it with [`Self::video_config`] / [`Self::audio_config`]
	/// if you want to describe the track in the catalog as well.
	pub fn track(&mut self, broadcast: Id, name: &str) -> Result<Id, Error> {
		let (broadcast, _) = self.broadcasts.get_mut(broadcast).ok_or(Error::BroadcastNotFound)?;
		let track = broadcast.create_track(name, None)?;
		self.tracks.insert(track)
	}

	/// Append a new group to a raw track, returning a group producer.
	pub fn track_group(&mut self, track: Id) -> Result<Id, Error> {
		let track = self.tracks.get_mut(track).ok_or(Error::TrackNotFound)?;
		let group = track.append_group()?;
		self.groups.insert(group)
	}

	/// Write a single-frame group to a raw track. Convenience for the common
	/// one-frame-per-group pattern (e.g. status/command tracks).
	pub fn track_frame(&mut self, track: Id, payload: &[u8]) -> Result<(), Error> {
		let track = self.tracks.get_mut(track).ok_or(Error::TrackNotFound)?;
		track.write_frame_now(bytes::Bytes::copy_from_slice(payload))?;
		Ok(())
	}

	/// Finish a raw track. No more groups or frames can be written.
	pub fn track_finish(&mut self, track: Id) -> Result<(), Error> {
		let mut track = self.tracks.remove(track).ok_or(Error::TrackNotFound)?;
		track.finish()?;
		Ok(())
	}

	/// Write a frame into a raw group.
	pub fn group_frame(&mut self, group: Id, payload: &[u8]) -> Result<(), Error> {
		let group = self.groups.get_mut(group).ok_or(Error::GroupNotFound)?;
		group.write_frame_now(bytes::Bytes::copy_from_slice(payload))?;
		Ok(())
	}

	/// Finish a raw group. No more frames can be written.
	pub fn group_finish(&mut self, group: Id) -> Result<(), Error> {
		let mut group = self.groups.remove(group).ok_or(Error::GroupNotFound)?;
		group.finish()?;
		Ok(())
	}
}
