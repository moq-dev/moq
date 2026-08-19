use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

use moq_mux::catalog::hang::Extra;
use moq_mux::catalog::{Rendition, RenditionConfig};
use moq_mux::import;

use crate::{Error, Id, NonZeroSlab};

/// A published broadcast: its producer, its catalog, and the renditions the caller authored by
/// hand.
///
/// The renditions are held rather than written and forgotten because the handle is what owns the
/// catalog entry: it publishes on [`set`](Rendition::set) and retires the entry on drop. A media
/// importer ([`import::Track`] or [`import::Container`]) holds its own for the tracks it publishes,
/// which is what keeps the two from writing over each other.
struct Broadcast {
	producer: moq_net::broadcast::Producer,
	catalog: moq_mux::catalog::Producer<Extra>,
	video: BTreeMap<String, Rendition<Extra, hang::catalog::VideoConfig>>,
	audio: BTreeMap<String, Rendition<Extra, hang::catalog::AudioConfig>>,
}

/// The caller's rendition under `name`, reserved on first use.
///
/// A second write to a name the caller already owns re-sets that rendition, so a config can be
/// refined in place. A name a media importer owns is refused, since it writes and removes its own.
fn rendition<'a, C: RenditionConfig<Extra>>(
	owned: &'a mut BTreeMap<String, Rendition<Extra, C>>,
	catalog: &moq_mux::catalog::Producer<Extra>,
	name: &str,
) -> Result<&'a mut Rendition<Extra, C>, Error> {
	match owned.entry(name.to_string()) {
		Entry::Occupied(entry) => Ok(entry.into_mut()),
		// A duplicate is a hang error either way, so report it under hang's own code rather than
		// the generic mux one.
		Entry::Vacant(entry) => match catalog.reserve().init::<C>(name) {
			Ok(rendition) => Ok(entry.insert(rendition)),
			Err(moq_mux::Error::Hang(err)) => Err(Error::Hang(err)),
			Err(err) => Err(err.into()),
		},
	}
}

#[derive(Default)]
pub struct Publish {
	/// Active broadcast producers for publishing.
	broadcasts: NonZeroSlab<Broadcast>,

	/// Single-codec media importers, fed timestamped frames.
	// Boxed because the codec splitters/imports are much larger than the container ones.
	media: NonZeroSlab<Box<import::Track<Extra>>>,

	/// Container importers, fed whole chunks. A separate space from `media` because a
	/// container publishes several tracks and carries its own timing, so it takes no
	/// per-frame timestamp.
	containers: NonZeroSlab<import::Container<Extra>>,

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

		let id = self.broadcasts.insert(Broadcast {
			producer: broadcast,
			catalog,
			video: BTreeMap::new(),
			audio: BTreeMap::new(),
		})?;
		Ok(id)
	}

	/// Set whether the broadcast is announced (announced by its origin), keeping the rest
	/// of its route (hops, cost).
	pub fn set_announce(&mut self, broadcast: Id, announce: bool) -> Result<(), Error> {
		let broadcast = self.broadcasts.get_mut(broadcast).ok_or(Error::BroadcastNotFound)?;
		let route = broadcast.producer.consume().route();
		broadcast.producer.set_route(route.with_announce(announce))?;
		Ok(())
	}

	/// The broadcast's track producer.
	fn producer(&mut self, id: Id) -> Result<&mut moq_net::broadcast::Producer, Error> {
		Ok(&mut self.broadcasts.get_mut(id).ok_or(Error::BroadcastNotFound)?.producer)
	}

	/// The broadcast's catalog producer.
	fn catalog(&mut self, id: Id) -> Result<&mut moq_mux::catalog::Producer<Extra>, Error> {
		Ok(&mut self.broadcasts.get_mut(id).ok_or(Error::BroadcastNotFound)?.catalog)
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
		let broadcast = self.broadcasts.get_mut(id).ok_or(Error::BroadcastNotFound)?;
		Ok((&mut broadcast.producer, &mut broadcast.catalog))
	}

	/// Cleanly finish the broadcast and finalize the catalog stream, so subscribers
	/// see a normal end rather than [`moq_net::Error::Dropped`].
	pub fn finish(&mut self, broadcast: Id) -> Result<(), Error> {
		let Broadcast {
			mut producer,
			mut catalog,
			video,
			audio,
		} = self.broadcasts.remove(broadcast).ok_or(Error::BroadcastNotFound)?;
		// Retire the caller's renditions while the catalog track is still open, so their removal is
		// published rather than warned about once `finish` has closed it.
		drop(video);
		drop(audio);
		// Finish the broadcast first so the clean end reaches subscribers even if
		// finalizing the catalog fails.
		producer.finish();
		catalog.finish()?;
		Ok(())
	}

	pub fn audio(&mut self, broadcast: Id, init: import::AudioInit) -> Result<Id, Error> {
		let Broadcast {
			producer: broadcast,
			catalog,
			..
		} = self.broadcasts.get(broadcast).ok_or(Error::BroadcastNotFound)?;
		let mut broadcast = broadcast.clone();
		let name = broadcast.unique_name(&format!(".{}", init.format));
		let request = broadcast.reserve_track(name)?;

		let track = import::Track::audio(request, catalog.reserve(), init)?;
		let id = self.media.insert(Box::new(track))?;
		Ok(id)
	}

	pub fn video(&mut self, broadcast: Id, init: import::VideoInit) -> Result<Id, Error> {
		let Broadcast {
			producer: broadcast,
			catalog,
			..
		} = self.broadcasts.get(broadcast).ok_or(Error::BroadcastNotFound)?;
		let mut broadcast = broadcast.clone();
		let name = broadcast.unique_name(&format!(".{}", init.format));
		let request = broadcast.reserve_track(name)?;

		let track = import::Track::video(request, catalog.reserve(), init)?;
		let id = self.media.insert(Box::new(track))?;
		Ok(id)
	}

	pub fn container(&mut self, broadcast: Id, init: import::ContainerInit) -> Result<Id, Error> {
		let Broadcast {
			producer: broadcast,
			catalog,
			..
		} = self.broadcasts.get(broadcast).ok_or(Error::BroadcastNotFound)?;
		let container = import::Container::new(broadcast.clone(), catalog.reserve(), &init)?;
		let id = self.containers.insert(container)?;
		Ok(id)
	}

	pub fn media_frame(&mut self, media: Id, data: &[u8], timestamp: hang::container::Timestamp) -> Result<(), Error> {
		let track = self.media.get_mut(media).ok_or(Error::MediaNotFound)?;
		track.decode(data, Some(timestamp))?;
		Ok(())
	}

	/// Draw a group boundary on this media importer.
	///
	/// This ends the open group; the next frame starts a new one. Audio has no boundary of its own
	/// (every frame is independently decodable), so this is the only thing that gives it groups:
	/// call it per frame for one group (one QUIC stream) forwarded without waiting, or at a segment
	/// cadence to align with video.
	pub fn media_cut(&mut self, media: Id) -> Result<(), Error> {
		let track = self.media.get_mut(media).ok_or(Error::MediaNotFound)?;
		track.cut(None)?;
		Ok(())
	}

	/// Draw a group boundary and number the next group `sequence`.
	///
	/// [`media_cut`](Self::media_cut) with an explicit sequence, for a caller whose group numbers
	/// have to be deterministic: two encoders publishing the same content align per GOP so a
	/// consumer can fail over between them.
	pub fn media_seek(&mut self, media: Id, sequence: u64) -> Result<(), Error> {
		let track = self.media.get_mut(media).ok_or(Error::MediaNotFound)?;
		track.seek(sequence)?;
		Ok(())
	}

	pub fn media_finish(&mut self, media: Id) -> Result<(), Error> {
		let mut track = self.media.remove(media).ok_or(Error::MediaNotFound)?;
		track.finish()?;
		Ok(())
	}

	/// Write a whole chunk of container bytes.
	///
	/// No timestamp: a container carries its tracks' timing itself.
	pub fn container_write(&mut self, container: Id, data: &[u8]) -> Result<(), Error> {
		let container = self.containers.get_mut(container).ok_or(Error::MediaNotFound)?;
		container.decode(data)?;
		Ok(())
	}

	/// Declare that the next chunk starts a new segment, rolling a group on every track.
	pub fn container_cut(&mut self, container: Id) -> Result<(), Error> {
		let container = self.containers.get_mut(container).ok_or(Error::MediaNotFound)?;
		container.cut();
		Ok(())
	}

	/// Start a new segment and number its groups `sequence`.
	pub fn container_seek(&mut self, container: Id, sequence: u64) -> Result<(), Error> {
		let container = self.containers.get_mut(container).ok_or(Error::MediaNotFound)?;
		container.seek(sequence)?;
		Ok(())
	}

	pub fn container_finish(&mut self, container: Id) -> Result<(), Error> {
		let mut container = self.containers.remove(container).ok_or(Error::MediaNotFound)?;
		container.finish()?;
		Ok(())
	}

	/// Insert or replace a caller-authored video rendition in the broadcast's catalog.
	///
	/// Errors if a media importer owns the name, since it publishes and retires its own rendition.
	/// The catalog is republished automatically.
	pub fn video_config(&mut self, broadcast: Id, name: &str, config: hang::catalog::VideoConfig) -> Result<(), Error> {
		let broadcast = self.broadcasts.get_mut(broadcast).ok_or(Error::BroadcastNotFound)?;
		rendition(&mut broadcast.video, &broadcast.catalog, name)?.set(config);
		Ok(())
	}

	/// Insert or replace a caller-authored audio rendition in the broadcast's catalog.
	///
	/// Same rules as [`Self::video_config`].
	pub fn audio_config(&mut self, broadcast: Id, name: &str, config: hang::catalog::AudioConfig) -> Result<(), Error> {
		let broadcast = self.broadcasts.get_mut(broadcast).ok_or(Error::BroadcastNotFound)?;
		rendition(&mut broadcast.audio, &broadcast.catalog, name)?.set(config);
		Ok(())
	}

	/// Remove a caller-authored video rendition from the broadcast's catalog by name.
	///
	/// A no-op for any name the caller didn't author, including one a media importer owns: dropping
	/// the handle is what retires the entry, and the importer holds its own. The catalog is
	/// republished automatically.
	pub fn video_remove(&mut self, broadcast: Id, name: &str) -> Result<(), Error> {
		let broadcast = self.broadcasts.get_mut(broadcast).ok_or(Error::BroadcastNotFound)?;
		broadcast.video.remove(name);
		Ok(())
	}

	/// Remove a caller-authored audio rendition from the broadcast's catalog by name.
	///
	/// Same rules as [`Self::video_remove`].
	pub fn audio_remove(&mut self, broadcast: Id, name: &str) -> Result<(), Error> {
		let broadcast = self.broadcasts.get_mut(broadcast).ok_or(Error::BroadcastNotFound)?;
		broadcast.audio.remove(name);
		Ok(())
	}

	/// Replace the properties shared by every video rendition as one catalog update.
	pub fn video_properties(&mut self, broadcast: Id, properties: hang::catalog::VideoProperties) -> Result<(), Error> {
		let catalog = self.catalog(broadcast)?;
		let mut catalog = catalog.lock();
		catalog.video.set_properties(properties)?;
		catalog.commit()?;
		Ok(())
	}

	/// Insert or replace a top-level application catalog section by name.
	///
	/// `value` is any JSON document. Errors if `name` is reserved (`video`/`audio`).
	/// The catalog is republished automatically.
	pub fn catalog_section_set(&mut self, broadcast: Id, name: &str, value: serde_json::Value) -> Result<(), Error> {
		let catalog = self.catalog(broadcast)?;
		catalog.lock().set_section(name.to_string(), value)?;
		Ok(())
	}

	/// Remove a top-level application catalog section by name.
	///
	/// A no-op if no section with that name exists. Republishes the catalog if it did.
	pub fn catalog_section_remove(&mut self, broadcast: Id, name: &str) -> Result<(), Error> {
		let catalog = self.catalog(broadcast)?;
		catalog.lock().remove_section(name);
		Ok(())
	}

	/// Create a raw track on a broadcast for arbitrary byte payloads.
	///
	/// No codec, container, or catalog framing. This is the moq-net primitive
	/// for non-media tracks. Pair it with [`Self::video_config`] / [`Self::audio_config`]
	/// if you want to describe the track in the catalog as well.
	pub fn track(&mut self, broadcast: Id, name: &str, info: Option<moq_net::track::Info>) -> Result<Id, Error> {
		let broadcast = self.producer(broadcast)?;
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
		let broadcast = self.producer(broadcast)?;
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
		let broadcast = self.producer(broadcast)?;
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
