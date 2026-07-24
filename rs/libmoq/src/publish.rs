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

	/// JSON snapshot producers (lossy latest-value tracks).
	json_snapshot: NonZeroSlab<moq_json::snapshot::Producer<serde_json::Value>>,

	/// JSON stream producers (lossless append-log tracks).
	json_stream: NonZeroSlab<moq_json::stream::Producer<serde_json::Value>>,
}

impl Publish {
	/// Store an origin-created broadcast producer, attaching the catalog track every
	/// libmoq broadcast carries.
	pub fn create(&mut self, mut broadcast: moq_net::broadcast::Producer) -> Result<Id, Error> {
		let catalog =
			moq_mux::catalog::Producer::with_catalog(&mut broadcast, moq_mux::catalog::hang::Catalog::default())?;

		let id = self.broadcasts.insert((broadcast, catalog))?;
		Ok(id)
	}

	/// Set whether the broadcast is announced (announced by its origin), keeping the rest
	/// of its route (hops, cost).
	pub fn set_announce(&mut self, broadcast: Id, announce: bool) -> Result<(), Error> {
		let (broadcast, _) = self.broadcasts.get_mut(broadcast).ok_or(Error::BroadcastNotFound)?;
		let route = broadcast.consume().route();
		broadcast.set_route(route.with_announce(announce))?;
		Ok(())
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

	/// Cleanly finish the broadcast and finalize the catalog stream, so subscribers
	/// see a normal end rather than [`moq_net::Error::Dropped`].
	pub fn finish(&mut self, broadcast: Id) -> Result<(), Error> {
		let (mut broadcast, mut catalog) = self.broadcasts.remove(broadcast).ok_or(Error::BroadcastNotFound)?;
		// Finish the broadcast first so the clean end reaches subscribers even if
		// finalizing the catalog fails.
		broadcast.finish();
		catalog.finish()?;
		Ok(())
	}

	pub fn media(&mut self, broadcast: Id, format: &str, init: &[u8]) -> Result<Id, Error> {
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
				match import::Track::new(request, catalog.reserve(), import::Init::new(format, init.to_vec())) {
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

	pub fn media_finish(&mut self, media: Id) -> Result<(), Error> {
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

	/// Replace the video presentation metadata as one catalog update.
	pub fn video_presentation(
		&mut self,
		broadcast: Id,
		presentation: hang::catalog::VideoPresentation,
	) -> Result<(), Error> {
		let (_, catalog) = self.broadcasts.get_mut(broadcast).ok_or(Error::BroadcastNotFound)?;
		let mut catalog = catalog.lock();
		catalog.video.set_presentation(presentation)?;
		catalog.commit()?;
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
	pub fn track(&mut self, broadcast: Id, name: &str, info: Option<moq_net::track::Info>) -> Result<Id, Error> {
		let (broadcast, _) = self.broadcasts.get_mut(broadcast).ok_or(Error::BroadcastNotFound)?;
		let track = broadcast.create_track(name, info)?;
		self.tracks.insert(track)
	}

	/// Append a new group to a raw track, returning a group producer.
	pub fn track_group(&mut self, track: Id) -> Result<Id, Error> {
		let track = self.tracks.get_mut(track).ok_or(Error::TrackNotFound)?;
		let group = track.append_group()?;
		self.groups.insert(group)
	}

	/// Create a raw group with an explicit sequence number.
	pub fn track_group_at(&mut self, track: Id, sequence: u64) -> Result<Id, Error> {
		let track = self.tracks.get_mut(track).ok_or(Error::TrackNotFound)?;
		let group = track.create_group(moq_net::group::Info { sequence })?;
		self.groups.insert(group)
	}

	/// Write a single-frame group to a raw track with an explicit timestamp.
	pub fn track_frame(&mut self, track: Id, timestamp: moq_net::Timestamp, payload: &[u8]) -> Result<(), Error> {
		let track = self.tracks.get_mut(track).ok_or(Error::TrackNotFound)?;
		track.write_frame(timestamp, bytes::Bytes::copy_from_slice(payload))?;
		Ok(())
	}

	/// Send a best-effort datagram on a raw track, returning its per-track sequence.
	///
	/// The payload must be at most [`moq_net::MAX_DATAGRAM_PAYLOAD`] bytes. Datagrams are
	/// delivered only on transports and wire versions with a datagram channel; there is no
	/// group fallback.
	pub fn track_datagram(&mut self, track: Id, timestamp_us: u64, payload: &[u8]) -> Result<u64, Error> {
		let track = self.tracks.get_mut(track).ok_or(Error::TrackNotFound)?;
		let timestamp = moq_net::Timestamp::from_micros(timestamp_us)?;
		Ok(track.append_datagram(timestamp, bytes::Bytes::copy_from_slice(payload))?)
	}

	/// Finish a raw track. No more groups or frames can be written.
	///
	/// [`Self::track_finish_at`] declares the boundary ahead of time, so this keeps that
	/// boundary and only releases the handle.
	pub fn track_finish(&mut self, track: Id) -> Result<(), Error> {
		let mut track = self.tracks.remove(track).ok_or(Error::TrackNotFound)?;
		if track.final_sequence().is_none() {
			track.finish()?;
		}
		Ok(())
	}

	/// Declare a raw track's exclusive final group sequence.
	pub fn track_finish_at(&mut self, track: Id, final_sequence: u64) -> Result<(), Error> {
		let track = self.tracks.get_mut(track).ok_or(Error::TrackNotFound)?;
		track.finish_at(final_sequence)?;
		Ok(())
	}

	/// Abort a raw track with an application error code.
	pub fn track_abort(&mut self, track: Id, error_code: u16) -> Result<(), Error> {
		let track = self.tracks.remove(track).ok_or(Error::TrackNotFound)?;
		track.abort(moq_net::Error::App(error_code))?;
		Ok(())
	}

	/// Create a JSON snapshot track (lossy latest-value) on a broadcast.
	///
	/// Values published via [`Self::json_snapshot_update`] reach subscribers as a single latest
	/// state; a late joiner only sees the newest value. Advertise the track in the catalog with
	/// [`Self::catalog_section_set`] if consumers should discover it.
	pub fn json_snapshot(
		&mut self,
		broadcast: Id,
		name: &str,
		config: moq_json::snapshot::ProducerConfig,
	) -> Result<Id, Error> {
		let (broadcast, _) = self.broadcasts.get_mut(broadcast).ok_or(Error::BroadcastNotFound)?;
		let track = broadcast.create_track(name, None)?;
		let producer = moq_json::snapshot::Producer::new(track, config);
		self.json_snapshot.insert(producer)
	}

	/// Publish a new value to a JSON snapshot track. A no-op if unchanged.
	pub fn json_snapshot_update(&mut self, json: Id, value: serde_json::Value) -> Result<(), Error> {
		let producer = self.json_snapshot.get_mut(json).ok_or(Error::TrackNotFound)?;
		producer.update(&value)?;
		Ok(())
	}

	/// Finish a JSON snapshot track. No more values can be published.
	pub fn json_snapshot_finish(&mut self, json: Id) -> Result<(), Error> {
		let mut producer = self.json_snapshot.remove(json).ok_or(Error::TrackNotFound)?;
		producer.finish()?;
		Ok(())
	}

	/// Create a JSON stream track (lossless append-log) on a broadcast.
	///
	/// Every record appended via [`Self::json_stream_append`] is preserved and delivered in order.
	pub fn json_stream(
		&mut self,
		broadcast: Id,
		name: &str,
		config: moq_json::stream::ProducerConfig,
	) -> Result<Id, Error> {
		let (broadcast, _) = self.broadcasts.get_mut(broadcast).ok_or(Error::BroadcastNotFound)?;
		let track = broadcast.create_track(name, None)?;
		let producer = moq_json::stream::Producer::new(track, config);
		self.json_stream.insert(producer)
	}

	/// Append one record to a JSON stream track.
	pub fn json_stream_append(&mut self, stream: Id, value: serde_json::Value) -> Result<(), Error> {
		let producer = self.json_stream.get_mut(stream).ok_or(Error::TrackNotFound)?;
		producer.append(&value)?;
		Ok(())
	}

	/// Finish a JSON stream track. No more records can be appended.
	pub fn json_stream_finish(&mut self, stream: Id) -> Result<(), Error> {
		let mut producer = self.json_stream.remove(stream).ok_or(Error::TrackNotFound)?;
		producer.finish()?;
		Ok(())
	}

	/// Write a frame into a raw group with an explicit timestamp.
	pub fn group_frame(&mut self, group: Id, timestamp: moq_net::Timestamp, payload: &[u8]) -> Result<(), Error> {
		let group = self.groups.get_mut(group).ok_or(Error::GroupNotFound)?;
		group.write_frame(timestamp, bytes::Bytes::copy_from_slice(payload))?;
		Ok(())
	}

	/// Finish a raw group. No more frames can be written.
	pub fn group_finish(&mut self, group: Id) -> Result<(), Error> {
		let mut group = self.groups.remove(group).ok_or(Error::GroupNotFound)?;
		group.finish()?;
		Ok(())
	}

	/// Abort a raw group with an application error code.
	pub fn group_abort(&mut self, group: Id, error_code: u16) -> Result<(), Error> {
		let group = self.groups.remove(group).ok_or(Error::GroupNotFound)?;
		group.abort(moq_net::Error::App(error_code))?;
		Ok(())
	}
}
