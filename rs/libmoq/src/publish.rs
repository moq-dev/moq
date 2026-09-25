use std::collections::BTreeMap;

use moq_mux::catalog::hang::Extra;
use moq_mux::import;
use tokio::sync::oneshot;

use crate::ffi::OnStatus;
use crate::{Error, Id, NonZeroSlab, State, moq_demand};

/// A spawned task entry: `close` signals shutdown, `callback` delivers status.
///
/// `close` is an `Option` so `*_close` can drop just the sender without
/// removing the entry. The task delivers one final terminal callback and then
/// removes itself, so `user_data` stays valid until that callback fires.
struct TaskEntry {
	close: Option<oneshot::Sender<()>>,
	callback: OnStatus,
}

/// A subscriber's request for a track the broadcast has not declared, kept with the
/// broadcast it was made on so media can be published onto it under that broadcast's catalog.
struct TrackRequest {
	broadcast: Id,
	request: moq_net::track::Request,
}

/// A request a handler pulled, before it has a handle.
enum Request {
	Track(TrackRequest),
	Group(moq_net::group::Request),
}

/// What a request handler serves.
enum Dynamic {
	/// Track requests on a broadcast, remembered so media can be published onto them.
	Broadcast(moq_net::broadcast::Dynamic, Id),
	/// Group requests (fetches of uncached groups) on a track.
	Track(moq_net::track::Dynamic),
}

/// A published broadcast: its producer, its catalog, and the renditions the caller authored by
/// hand.
///
/// Caller-authored configs are tracked separately so removing one cannot retire an importer's
/// rendition with the same name.
struct Broadcast {
	producer: moq_net::broadcast::Producer,
	catalog: moq_mux::catalog::Producer<Extra>,
	video: BTreeMap<String, hang::catalog::VideoConfig>,
	audio: BTreeMap<String, hang::catalog::AudioConfig>,
}

#[derive(Default)]
pub struct Publish {
	/// Active broadcast producers for publishing.
	broadcasts: NonZeroSlab<Broadcast>,

	/// Single-codec media importers, fed timestamped frames.
	// Boxed because the codec splitters/imports are much larger than the container ones.
	media: NonZeroSlab<Box<import::Track>>,

	/// Container importers, fed whole chunks. A separate space from `media` because a
	/// container publishes several tracks and carries its own timing, so it takes no
	/// per-frame timestamp.
	containers: NonZeroSlab<import::Container<Extra>>,

	/// Raw track producers (no media/container/catalog framing).
	tracks: NonZeroSlab<moq_net::track::Producer>,

	/// Raw group producers, created from a raw track producer.
	groups: NonZeroSlab<moq_net::group::Producer>,

	/// JSON snapshot producers (lossy latest-value tracks), each advertised in its broadcast's
	/// catalog for as long as it lives.
	json_snapshot: NonZeroSlab<moq_mux::json::Snapshot<serde_json::Value, Extra>>,

	/// JSON stream producers (lossless append-log tracks), advertised the same way.
	json_stream: NonZeroSlab<moq_mux::json::Stream<serde_json::Value, Extra>>,

	/// Binary snapshot producers (lossy latest-value tracks of opaque bytes), advertised the same way.
	binary_snapshot: NonZeroSlab<moq_mux::binary::Snapshot<Extra>>,

	/// Binary stream producers (lossless append-log tracks of opaque bytes), advertised the same way.
	binary_stream: NonZeroSlab<moq_mux::binary::Stream<Extra>>,

	/// Demand watchers. Close signals shutdown; the task delivers a final callback, then removes itself.
	demand: NonZeroSlab<Option<TaskEntry>>,

	/// Track and group request handlers. Close drops the handler, so pending requests
	/// are rejected; the task delivers a final callback, then removes itself.
	dynamic: NonZeroSlab<Option<TaskEntry>>,

	/// Track requests delivered to a handler, freed on accept, abort, or free.
	track_request: NonZeroSlab<TrackRequest>,

	/// Group requests delivered to a handler, freed on accept, abort, or free.
	group_request: NonZeroSlab<moq_net::group::Request>,
}

impl Publish {
	/// Store an origin-created broadcast producer, attaching the catalog track
	/// every libmoq broadcast carries.
	pub fn create(&mut self, mut broadcast: moq_net::broadcast::Producer) -> Result<Id, Error> {
		let config = moq_mux::catalog::Config::default()
			.with_catalog(moq_mux::catalog::hang::Catalog::<moq_mux::catalog::hang::Extra>::default());
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, config)?;

		let id = self.broadcasts.insert(Broadcast {
			producer: broadcast,
			catalog,
			video: BTreeMap::new(),
			audio: BTreeMap::new(),
		})?;
		Ok(id)
	}

	/// Advertise the broadcast's exact path as a route. Announcing again re-prices
	/// in place. Until announced, the broadcast is invisible and unroutable.
	pub fn announce(&mut self, broadcast: Id, route: moq_net::origin::Route) -> Result<(), Error> {
		let broadcast = self.broadcasts.get_mut(broadcast).ok_or(Error::BroadcastNotFound)?;
		broadcast.producer.announce(route)?;
		Ok(())
	}

	/// Retract the broadcast's exact-path advertisement, if any.
	pub fn unannounce(&mut self, broadcast: Id) -> Result<(), Error> {
		let broadcast = self.broadcasts.get_mut(broadcast).ok_or(Error::BroadcastNotFound)?;
		broadcast.producer.unannounce();
		Ok(())
	}

	/// The broadcast's track producer.
	pub(crate) fn producer(&mut self, id: Id) -> Result<&mut moq_net::broadcast::Producer, Error> {
		Ok(&mut self.broadcasts.get_mut(id).ok_or(Error::BroadcastNotFound)?.producer)
	}

	/// The broadcast's catalog producer.
	fn catalog(&mut self, id: Id) -> Result<&mut moq_mux::catalog::Producer<Extra>, Error> {
		Ok(&mut self.broadcasts.get_mut(id).ok_or(Error::BroadcastNotFound)?.catalog)
	}

	/// The broadcast's current catalog, as consumers would receive it next.
	#[cfg(test)]
	pub fn catalog_snapshot(&mut self, id: Id) -> Result<moq_mux::catalog::hang::Catalog<Extra>, Error> {
		Ok(self.catalog(id)?.snapshot())
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

	/// End the broadcast for good and release it, finalizing the catalog stream.
	pub fn close(&mut self, broadcast: Id) -> Result<(), Error> {
		let Broadcast {
			producer,
			mut catalog,
			video,
			audio,
			..
		} = self.broadcasts.remove(broadcast).ok_or(Error::BroadcastNotFound)?;
		// Retire caller-authored entries while the catalog track is still open.
		{
			let mut guard = catalog.modify()?;
			for name in video.keys() {
				guard.video.renditions.remove(name);
			}
			for name in audio.keys() {
				guard.audio.renditions.remove(name);
			}
			guard.commit()?;
		}
		// Close the broadcast first so it ends even if finalizing the catalog fails.
		producer.close();
		catalog.finish()?;
		Ok(())
	}

	pub fn audio(&mut self, broadcast: Id, init: import::AudioInit) -> Result<Id, Error> {
		let Broadcast {
			producer: broadcast,
			catalog,
			..
		} = self.broadcasts.get(broadcast).ok_or(Error::BroadcastNotFound)?;
		let broadcast = broadcast.clone();
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
		let broadcast = broadcast.clone();
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

	/// Record a locally encoded frame's transport handoff. Generic imports remain clock-free
	/// unless their caller explicitly identifies the frame as encoder output.
	pub fn media_flush(&mut self, media: Id, timestamp: hang::container::Timestamp) -> Result<(), Error> {
		let track = self.media.get_mut(media).ok_or(Error::MediaNotFound)?;
		track.flush(timestamp, std::time::Instant::now())?;
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
		if !broadcast.video.contains_key(name) && broadcast.catalog.is_claimed::<hang::catalog::VideoConfig>(name) {
			return Err(Error::Hang(hang::Error::Duplicate(name.to_string())));
		}
		broadcast.video.insert(name.to_string(), config.clone());
		let mut catalog = broadcast.catalog.modify()?;
		catalog.video.renditions.insert(name.to_string(), config);
		catalog.commit()?;
		Ok(())
	}

	/// Insert or replace a caller-authored audio rendition in the broadcast's catalog.
	///
	/// Same rules as [`Self::video_config`].
	pub fn audio_config(&mut self, broadcast: Id, name: &str, config: hang::catalog::AudioConfig) -> Result<(), Error> {
		let broadcast = self.broadcasts.get_mut(broadcast).ok_or(Error::BroadcastNotFound)?;
		if !broadcast.audio.contains_key(name) && broadcast.catalog.is_claimed::<hang::catalog::AudioConfig>(name) {
			return Err(Error::Hang(hang::Error::Duplicate(name.to_string())));
		}
		broadcast.audio.insert(name.to_string(), config.clone());
		let mut catalog = broadcast.catalog.modify()?;
		catalog.audio.renditions.insert(name.to_string(), config);
		catalog.commit()?;
		Ok(())
	}

	/// Remove a caller-authored video rendition from the broadcast's catalog by name.
	///
	/// A no-op for any name the caller didn't author, including one a media importer owns: dropping
	/// the handle is what retires the entry, and the importer holds its own. The catalog is
	/// republished automatically.
	pub fn video_remove(&mut self, broadcast: Id, name: &str) -> Result<(), Error> {
		let broadcast = self.broadcasts.get_mut(broadcast).ok_or(Error::BroadcastNotFound)?;
		if broadcast.video.remove(name).is_some() {
			let mut catalog = broadcast.catalog.modify()?;
			catalog.video.renditions.remove(name);
			catalog.commit()?;
		}
		Ok(())
	}

	/// Remove a caller-authored audio rendition from the broadcast's catalog by name.
	///
	/// Same rules as [`Self::video_remove`].
	pub fn audio_remove(&mut self, broadcast: Id, name: &str) -> Result<(), Error> {
		let broadcast = self.broadcasts.get_mut(broadcast).ok_or(Error::BroadcastNotFound)?;
		if broadcast.audio.remove(name).is_some() {
			let mut catalog = broadcast.catalog.modify()?;
			catalog.audio.renditions.remove(name);
			catalog.commit()?;
		}
		Ok(())
	}

	/// Replace the properties shared by every video rendition as one catalog update.
	pub fn video_properties(&mut self, broadcast: Id, properties: hang::catalog::VideoProperties) -> Result<(), Error> {
		let catalog = self.catalog(broadcast)?;
		let mut catalog = catalog.modify()?;
		catalog.video.set_properties(properties)?;
		catalog.commit()?;
		Ok(())
	}

	/// Insert or replace a top-level application catalog section by name.
	///
	/// `value` is any JSON document. Errors if `name` is a HANG root (`video`, `audio`, `text`,
	/// `archive`, `clock`, `json`, `binary`, or retired `timeline`) or an MSF root (`version`,
	/// `generatedAt`, `isComplete`, `tracks`, or `initDataList`). The catalog is republished
	/// automatically.
	pub fn catalog_section_set(&mut self, broadcast: Id, name: &str, value: serde_json::Value) -> Result<(), Error> {
		let catalog = self.catalog(broadcast)?;
		let mut guard = catalog.modify()?;
		guard.set_section(name.to_string(), value)?;
		guard.commit()?;
		Ok(())
	}

	/// Remove a top-level application catalog section by name.
	///
	/// A no-op if no section with that name exists. Republishes the catalog if it did.
	pub fn catalog_section_remove(&mut self, broadcast: Id, name: &str) -> Result<(), Error> {
		let catalog = self.catalog(broadcast)?;
		let mut guard = catalog.modify()?;
		guard.remove_section(name);
		guard.commit()?;
		Ok(())
	}

	/// A watch-only handle to a raw track's subscriber demand.
	pub fn track_demand(&self, track: Id) -> Result<moq_net::track::Demand, Error> {
		Ok(self.tracks.get(track).ok_or(Error::TrackNotFound)?.demand())
	}

	/// A watch-only handle to a media importer's subscriber demand.
	///
	/// A container publishes several tracks and so has no single demand; its handle lives in
	/// another slab and is refused here, as moq-ffi refuses it.
	pub fn media_demand(&self, media: Id) -> Result<moq_net::track::Demand, Error> {
		Ok(self.media.get(media).ok_or(Error::MediaNotFound)?.demand())
	}

	/// Watch a track's subscriber demand, reporting the current state and every change.
	///
	/// `on_demand` fires with a [`moq_demand`] value immediately and again on each change, then
	/// once with a terminal code: `0` when the track ends or the watcher is closed, negative when
	/// the track aborts. Seeding with the current state is what makes a late registration safe: a
	/// track that went unused before the watcher existed still reports it.
	pub fn demand(&mut self, demand: moq_net::track::Demand, on_demand: OnStatus) -> Result<Id, Error> {
		let channel = oneshot::channel();
		let id = self.demand.insert(Some(TaskEntry {
			close: Some(channel.0),
			callback: on_demand,
		}))?;

		tokio::spawn(async move {
			let res = Self::run_demand(on_demand, demand, channel.1).await;

			// Deliver one final terminal callback (code <= 0), then drop the entry.
			// Pull it out from under the lock so the callback never runs while held.
			let entry = State::lock().publish.demand.remove(id).flatten();
			if let Some(entry) = entry {
				entry.callback.call(res);
			}
		});

		Ok(id)
	}

	pub(crate) async fn run_demand(
		callback: OnStatus,
		demand: moq_net::track::Demand,
		mut close: oneshot::Receiver<()>,
	) -> Result<(), Error> {
		// Neither handle exposes the current state, only the level-triggered waits, and exactly
		// one of them is ready at any instant: racing them is the read. Close is ignored until
		// that seed is delivered, so a watcher closed before its first poll still reports USED
		// or UNUSED before the terminal. A dropped track is its end, not a failure; the watcher
		// does not keep it alive and reports the close as clean.
		let mut used = tokio::select! {
			res = demand.used() => match res {
				Ok(()) => true,
				Err(moq_net::Error::Dropped) => return Ok(()),
				Err(err) => return Err(err.into()),
			},
			res = demand.unused() => match res {
				Ok(()) => false,
				Err(moq_net::Error::Dropped) => return Ok(()),
				Err(err) => return Err(err.into()),
			},
		};

		loop {
			let state = if used {
				moq_demand::MOQ_DEMAND_USED
			} else {
				moq_demand::MOQ_DEMAND_UNUSED
			};
			callback.call(state as i32);

			// A flip between the report and this wait resolves it immediately, so no edge is
			// lost; a double flip collapses into nothing, which is what a level signal means.
			let flipped = async {
				if used {
					demand.unused().await
				} else {
					demand.used().await
				}
			};
			tokio::select! {
				biased;
				_ = &mut close => return Ok(()),
				res = flipped => match res {
					Ok(()) => used = !used,
					Err(moq_net::Error::Dropped) => return Ok(()),
					Err(err) => return Err(err.into()),
				},
			}
		}
	}

	/// Stop a demand watcher. The task still delivers its terminal callback.
	pub fn demand_close(&mut self, watcher: Id) -> Result<(), Error> {
		self.demand
			.get_mut(watcher)
			.and_then(|entry| entry.as_mut())
			.ok_or(Error::NotFound)?
			.close
			.take()
			.ok_or(Error::NotFound)?;
		Ok(())
	}

	/// Serve subscriber requests for tracks the broadcast has not declared, delivering each as
	/// a track-request handle via `on_request`.
	///
	/// Without a live handler an unknown track name is refused, as before.
	pub fn dynamic(&mut self, broadcast: Id, on_request: OnStatus) -> Result<Id, Error> {
		let dynamic = self.producer(broadcast)?.dynamic();
		self.spawn_dynamic(Dynamic::Broadcast(dynamic, broadcast), on_request)
	}

	/// Serve fetches of uncached groups on a raw track, delivering each as a group-request
	/// handle via `on_group`.
	pub fn track_dynamic(&mut self, track: Id, on_group: OnStatus) -> Result<Id, Error> {
		let dynamic = self.tracks.get(track).ok_or(Error::TrackNotFound)?.dynamic();
		self.spawn_dynamic(Dynamic::Track(dynamic), on_group)
	}

	/// Serve fetches of uncached groups on a track that has not been accepted yet.
	///
	/// A track requested by a fetch has a group request pending from birth; a handler obtained
	/// before [`Self::track_request_accept`] keeps it serviceable across the transition.
	pub fn track_request_dynamic(&mut self, request: Id, on_group: OnStatus) -> Result<Id, Error> {
		let dynamic = self
			.track_request
			.get(request)
			.ok_or(Error::NotFound)?
			.request
			.dynamic();
		self.spawn_dynamic(Dynamic::Track(dynamic), on_group)
	}

	fn spawn_dynamic(&mut self, dynamic: Dynamic, callback: OnStatus) -> Result<Id, Error> {
		let channel = oneshot::channel();
		let id = self.dynamic.insert(Some(TaskEntry {
			close: Some(channel.0),
			callback,
		}))?;

		tokio::spawn(async move {
			let res = Self::run_dynamic(callback, dynamic, channel.1).await;

			// Deliver one final terminal callback (code <= 0), then drop the entry.
			// Pull it out from under the lock so the callback never runs while held.
			let entry = State::lock().publish.dynamic.remove(id).flatten();
			if let Some(entry) = entry {
				entry.callback.call(res);
			}
		});

		Ok(id)
	}

	async fn run_dynamic(
		callback: OnStatus,
		mut dynamic: Dynamic,
		mut close: oneshot::Receiver<()>,
	) -> Result<(), Error> {
		loop {
			// The handler is owned here, so returning drops it and rejects whatever is pending.
			// `biased` so a pending close always wins over a ready request.
			let res = tokio::select! {
				biased;
				_ = &mut close => return Ok(()),
				res = kio::wait(|waiter| match &mut dynamic {
					Dynamic::Broadcast(dynamic, broadcast) => dynamic
						.poll_requested_track(waiter)
						.map_ok(|request| Request::Track(TrackRequest { broadcast: *broadcast, request })),
					Dynamic::Track(dynamic) => dynamic.poll_requested_group(waiter).map_ok(Request::Group),
				}) => res,
			};

			// A finished broadcast or track is the end of the requests, not a failure.
			let request = match res {
				Ok(request) => request,
				Err(moq_net::Error::Closed | moq_net::Error::Dropped) => return Ok(()),
				Err(err) => return Err(err.into()),
			};

			// Hold the lock only to buffer the request; release it before the callback.
			let mut state = State::lock();
			let id = match request {
				Request::Track(request) => state.publish.track_request.insert(request)?,
				Request::Group(request) => state.publish.group_request.insert(request)?,
			};
			drop(state);
			callback.call(id);
		}
	}

	/// Stop a track or group request handler. Pending requests are rejected, and the task
	/// still delivers its terminal callback.
	pub fn dynamic_close(&mut self, dynamic: Id) -> Result<(), Error> {
		self.dynamic
			.get_mut(dynamic)
			.and_then(|entry| entry.as_mut())
			.ok_or(Error::NotFound)?
			.close
			.take()
			.ok_or(Error::NotFound)?;
		Ok(())
	}

	/// The name of a requested track, borrowed from the request's storage.
	pub fn track_request_name(&self, request: Id, dst: &mut crate::moq_string) -> Result<(), Error> {
		let name = self.track_request.get(request).ok_or(Error::NotFound)?.request.name();
		*dst = crate::moq_string {
			data: name.as_ptr().cast::<std::ffi::c_char>(),
			len: name.len(),
		};
		Ok(())
	}

	/// Accept a track request as a raw track, returning a track handle like [`Self::track`].
	pub fn track_request_accept(&mut self, request: Id, info: moq_net::track::Info) -> Result<Id, Error> {
		let request = self.track_request.remove(request).ok_or(Error::NotFound)?;
		self.tracks.insert(request.request.accept(info))
	}

	/// Accept a track request as an audio track, returning a media handle like [`Self::audio`].
	pub fn track_request_audio(&mut self, request: Id, init: import::AudioInit) -> Result<Id, Error> {
		let TrackRequest { broadcast, request } = self.track_request.remove(request).ok_or(Error::NotFound)?;
		let catalog = self.catalog(broadcast)?;
		let track = import::Track::audio(request, catalog.reserve(), init)?;
		self.media.insert(Box::new(track))
	}

	/// Accept a track request as a video track, returning a media handle like [`Self::video`].
	pub fn track_request_video(&mut self, request: Id, init: import::VideoInit) -> Result<Id, Error> {
		let TrackRequest { broadcast, request } = self.track_request.remove(request).ok_or(Error::NotFound)?;
		let catalog = self.catalog(broadcast)?;
		let track = import::Track::video(request, catalog.reserve(), init)?;
		self.media.insert(Box::new(track))
	}

	/// Reject a track request, failing every subscriber waiting on it with `error_code`.
	pub fn track_request_abort(&mut self, request: Id, error_code: u16) -> Result<(), Error> {
		let request = self.track_request.remove(request).ok_or(Error::NotFound)?;
		request.request.reject(moq_net::Error::App(error_code));
		Ok(())
	}

	/// Drop a track request, which rejects it.
	pub fn track_request_free(&mut self, request: Id) -> Result<(), Error> {
		self.track_request.remove(request).ok_or(Error::NotFound)?;
		Ok(())
	}

	/// The sequence, priority, and first frame of a requested group.
	pub fn group_request_info(&self, request: Id) -> Result<(u64, u8, u64), Error> {
		let request = self.group_request.get(request).ok_or(Error::NotFound)?;
		Ok((request.sequence(), request.priority(), request.frame_start()))
	}

	/// Accept a group request, returning a group handle like [`Self::track_group`].
	///
	/// The producer is positioned at the request's `frame_start` so frames keep the
	/// indices they have in the group rather than restarting at 0.
	pub fn group_request_accept(&mut self, request: Id) -> Result<Id, Error> {
		let request = self.group_request.remove(request).ok_or(Error::NotFound)?;
		let frame_start = request.frame_start();
		let mut group = request.accept(None)?;
		if let Err(err) = group.start_at(frame_start) {
			let _ = group.abort(err.clone());
			return Err(err.into());
		}
		self.groups.insert(group)
	}

	/// Reject a group request, failing every fetch waiting on it with `error_code`.
	pub fn group_request_abort(&mut self, request: Id, error_code: u16) -> Result<(), Error> {
		let request = self.group_request.remove(request).ok_or(Error::NotFound)?;
		request.reject(moq_net::Error::App(error_code));
		Ok(())
	}

	/// Drop a group request, which rejects it.
	pub fn group_request_free(&mut self, request: Id) -> Result<(), Error> {
		self.group_request.remove(request).ok_or(Error::NotFound)?;
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
		let track = self.tracks.remove(track).ok_or(Error::TrackNotFound)?;
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

	/// Create a track on a broadcast and hand it, with the broadcast's catalog, to `publish`, which
	/// wraps it in a data producer that advertises the track in that catalog.
	fn data_track<T>(
		&mut self,
		broadcast: Id,
		name: &str,
		publish: impl FnOnce(&moq_mux::catalog::Producer<Extra>, moq_net::track::Producer) -> moq_mux::Result<T>,
	) -> Result<T, Error> {
		let broadcast = self.broadcasts.get_mut(broadcast).ok_or(Error::BroadcastNotFound)?;
		let track = broadcast.producer.create_track(name, None)?;
		Ok(publish(&broadcast.catalog, track)?)
	}

	/// Create a JSON snapshot track (lossy latest-value) on a broadcast, advertised in its catalog.
	///
	/// Values published via [`Self::json_snapshot_update`] reach subscribers as a single latest
	/// state; a late joiner only sees the newest value. The catalog entry (`json.tracks.<name>`,
	/// `mode: snapshot`) is written now and retired when the track finishes or fails.
	pub fn json_snapshot(&mut self, broadcast: Id, name: &str, config: moq_mux::json::Config) -> Result<Id, Error> {
		let producer = self.data_track(broadcast, name, |catalog, track| catalog.json_snapshot(track, config))?;
		self.json_snapshot.insert(producer)
	}

	/// Publish a new value to a JSON snapshot track. A no-op if unchanged.
	pub fn json_snapshot_update(&mut self, json: Id, value: serde_json::Value) -> Result<(), Error> {
		let producer = self.json_snapshot.get_mut(json).ok_or(Error::TrackNotFound)?;
		producer.update(&value)?;
		Ok(())
	}

	/// Finish a JSON snapshot track and retire its catalog entry. No more values can be published.
	pub fn json_snapshot_finish(&mut self, json: Id) -> Result<(), Error> {
		let producer = self.json_snapshot.remove(json).ok_or(Error::TrackNotFound)?;
		producer.finish()?;
		Ok(())
	}

	/// Create a JSON stream track (lossless append-log) on a broadcast, advertised in its catalog.
	///
	/// Every record appended via [`Self::json_stream_append`] is preserved and delivered in order.
	/// The catalog entry (`json.tracks.<name>`, `mode: stream`) lives as long as the track.
	pub fn json_stream(&mut self, broadcast: Id, name: &str, config: moq_mux::json::Config) -> Result<Id, Error> {
		let producer = self.data_track(broadcast, name, |catalog, track| catalog.json_stream(track, config))?;
		self.json_stream.insert(producer)
	}

	/// Append one record to a JSON stream track.
	pub fn json_stream_append(&mut self, stream: Id, value: serde_json::Value) -> Result<(), Error> {
		let producer = self.json_stream.get_mut(stream).ok_or(Error::TrackNotFound)?;
		producer.append(&value)?;
		Ok(())
	}

	/// Finish a JSON stream track and retire its catalog entry. No more records can be appended.
	pub fn json_stream_finish(&mut self, stream: Id) -> Result<(), Error> {
		let producer = self.json_stream.remove(stream).ok_or(Error::TrackNotFound)?;
		producer.finish()?;
		Ok(())
	}

	/// Create a binary snapshot track (lossy latest-value) on a broadcast, advertised in its catalog
	/// as `binary.tracks.<name>`, `mode: snapshot`.
	pub fn binary_snapshot(&mut self, broadcast: Id, name: &str, config: moq_mux::binary::Config) -> Result<Id, Error> {
		let producer = self.data_track(broadcast, name, |catalog, track| catalog.binary_snapshot(track, config))?;
		self.binary_snapshot.insert(producer)
	}

	/// Publish a new payload to a binary snapshot track, superseding the last.
	pub fn binary_snapshot_update(&mut self, binary: Id, payload: &[u8]) -> Result<(), Error> {
		let producer = self.binary_snapshot.get_mut(binary).ok_or(Error::TrackNotFound)?;
		producer.update(bytes::Bytes::copy_from_slice(payload))?;
		Ok(())
	}

	/// Finish a binary snapshot track and retire its catalog entry.
	pub fn binary_snapshot_finish(&mut self, binary: Id) -> Result<(), Error> {
		let producer = self.binary_snapshot.remove(binary).ok_or(Error::TrackNotFound)?;
		producer.finish()?;
		Ok(())
	}

	/// Create a binary stream track (lossless append-log) on a broadcast, advertised in its catalog
	/// as `binary.tracks.<name>`, `mode: stream`.
	pub fn binary_stream(&mut self, broadcast: Id, name: &str, config: moq_mux::binary::Config) -> Result<Id, Error> {
		let producer = self.data_track(broadcast, name, |catalog, track| catalog.binary_stream(track, config))?;
		self.binary_stream.insert(producer)
	}

	/// Append one payload to a binary stream track.
	pub fn binary_stream_append(&mut self, stream: Id, payload: &[u8]) -> Result<(), Error> {
		let producer = self.binary_stream.get_mut(stream).ok_or(Error::TrackNotFound)?;
		producer.append(bytes::Bytes::copy_from_slice(payload))?;
		Ok(())
	}

	/// Finish a binary stream track and retire its catalog entry.
	pub fn binary_stream_finish(&mut self, stream: Id) -> Result<(), Error> {
		let producer = self.binary_stream.remove(stream).ok_or(Error::TrackNotFound)?;
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
		let group = self.groups.remove(group).ok_or(Error::GroupNotFound)?;
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
