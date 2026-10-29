use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use moq_mux::catalog::hang::Extra;
use moq_mux::import;
use tokio::sync::oneshot;

use crate::ffi::OnStatus;
use crate::task::Task;
use crate::{Error, Id, NonZeroSlab, State, moq_demand};

/// A demand watcher's task, plus a handle to peek the wanted group from its callback.
struct DemandEntry {
	task: Task,
	demand: moq_net::track::Demand,
}

/// A subscriber's request for a track the broadcast has not declared, kept with the
/// broadcast it was made on so media can be published onto it under that broadcast's catalog.
struct TrackRequest {
	broadcast: Id,
	// Carried, not looked up, so a raw accept learns its broadcast closed even after the id is gone.
	children: Children,
	request: moq_net::track::Request,
}

/// A request a handler pulled, before it has a handle. A track request is boxed: it is
/// several times the size of a fill.
enum Request {
	Track(Box<TrackRequest>),
	Fill(moq_net::track::Fill),
}

/// One group of a fill, handed to C as a group request: the frozen ABI answers fills a
/// group at a time.
struct GroupRequest {
	fill: Arc<moq_net::track::Fill>,
	sequence: u64,
	/// Dropped once the request is answered or freed, so its fill's next group follows.
	_pending: oneshot::Sender<()>,
}

/// What a request handler serves.
enum Dynamic {
	/// Track requests on a broadcast, remembered so media can be published onto them.
	Broadcast(moq_net::broadcast::Requests, Id, Children),
	/// Group requests (fetches of uncached groups) on a track.
	Track(moq_net::track::Requests),
}

/// A published broadcast: its producer, its catalog, and the renditions the caller authored by
/// hand.
///
/// Caller-authored configs are tracked separately so removing one cannot retire an importer's
/// rendition with the same name.
struct Broadcast {
	driver: crate::ffi::Background,
	producer: moq_net::broadcast::Producer,
	catalog: moq_mux::catalog::Producer<Extra>,
	video: BTreeMap<String, hang::catalog::VideoConfig>,
	audio: BTreeMap<String, hang::catalog::AudioConfig>,
	children: Children,
	/// Set by [`Publish::catalog_finish`]: the catalog is final, so a close leaves it alone.
	ended: bool,
}

/// A handle publishing tracks on a broadcast, by the table that holds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Child {
	Media(Id),
	Container(Id),
	Track(Id),
	Audio(Id),
	Video(Id),
	JsonSnapshot(Id),
	JsonStream(Id),
}

/// A child's handle, which ends what it still publishes when its broadcast closes.
pub(crate) trait Abort {
	/// End the handle's tracks with `err`. A track that already finished ends cleanly instead.
	fn abort(self, err: moq_net::Error);
}

/// What a broadcast holds of its children.
enum Family {
	Open {
		/// Handles that may still publish, ended on close.
		open: HashSet<Child>,
		/// Finished producers, kept so a late subscriber still finds their tracks.
		kept: Vec<Box<dyn Send>>,
	},
	/// The broadcast closed: its children ended and new ones are refused.
	Closed,
}

/// The track-publishing handles of one broadcast.
///
/// Closing the broadcast ends every child still open with `Unroutable`, the way a reader sees a
/// publisher leave, and frees its handle. A finished track stops being served once its last
/// producer drops, and a `*_finish` call frees its handle, so the broadcast keeps the finished
/// producer here instead: a subscriber arriving after the end still finds the track until
/// [`State::publish_close`].
#[derive(Clone)]
pub struct Children(Arc<std::sync::Mutex<Family>>);

impl Default for Children {
	fn default() -> Self {
		Self(Arc::new(std::sync::Mutex::new(Family::Open {
			open: HashSet::new(),
			kept: Vec::new(),
		})))
	}
}

impl Children {
	fn lock(&self) -> std::sync::MutexGuard<'_, Family> {
		self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
	}

	/// Insert a new child into `slab`, so the broadcast's close ends it. Once the broadcast
	/// closed, the child is aborted and refused.
	pub(crate) fn insert<T: Abort>(
		&self,
		slab: &mut NonZeroSlab<T>,
		value: T,
		child: fn(Id) -> Child,
	) -> Result<Id, Error> {
		let id = slab.insert(value)?;
		match &mut *self.lock() {
			Family::Open { open, .. } => {
				open.insert(child(id));
				return Ok(id);
			}
			Family::Closed => {}
		}
		if let Some(value) = slab.remove(id) {
			value.abort(moq_net::Error::Unroutable);
		}
		Err(Error::BroadcastNotFound)
	}

	/// Keep `child`'s finished producer until the broadcast ends, or drop it now if it already has.
	pub(crate) fn keep(&self, child: Child, producer: impl Send + 'static) {
		let producer: Box<dyn Send> = Box::new(producer);
		if let Family::Open { open, kept } = &mut *self.lock() {
			open.remove(&child);
			kept.push(producer);
		}
	}

	/// Stop tracking `child`, which ended on its own.
	pub(crate) fn forget(&self, child: Child) {
		if let Family::Open { open, .. } = &mut *self.lock() {
			open.remove(&child);
		}
	}

	/// Refuse new children, handing back the open ones and letting go of the finished ones.
	fn close(&self) -> HashSet<Child> {
		let family = std::mem::replace(&mut *self.lock(), Family::Closed);
		match family {
			Family::Open { open, kept } => {
				drop(kept);
				open
			}
			Family::Closed => HashSet::new(),
		}
	}
}

/// What a producer publishing on a broadcast needs from it.
pub struct Target<'a> {
	pub producer: &'a mut moq_net::broadcast::Producer,
	pub catalog: &'a mut moq_mux::catalog::Producer<Extra>,
	/// The broadcast's children, which the producer joins.
	pub children: &'a Children,
}

/// A producer handle, with the broadcast that keeps it once finished.
struct Kept<T> {
	inner: T,
	children: Children,
}

impl<T: Send + 'static> Kept<T> {
	fn keep(self, child: Child) {
		self.children.keep(child, self.inner);
	}
}

impl Abort for Kept<Box<import::Track>> {
	fn abort(self, err: moq_net::Error) {
		self.inner.abort(err);
	}
}

impl Abort for Kept<moq_net::track::Producer> {
	fn abort(self, err: moq_net::Error) {
		// A track with a declared end ends there, as when its last producer drops.
		if self.inner.finished().is_none() {
			let _ = self.inner.abort(err);
		}
	}
}

impl Abort for Container {
	fn abort(self, err: moq_net::Error) {
		self.inner.abort(err);
	}
}

impl Abort for JsonSnapshot {
	fn abort(self, err: moq_net::Error) {
		let _ = self.data.producer.abort(err);
	}
}

impl Abort for Data<moq_mux::json::Stream<serde_json::Value>> {
	fn abort(self, err: moq_net::Error) {
		let _ = self.producer.abort(err);
	}
}

/// The cause a failed import ends its tracks with, so a reader sees the publisher fail rather than
/// `Dropped`. Only a moq-net error has a wire code; any other reaches readers as `Internal`.
fn cause(err: &moq_mux::Error) -> moq_net::Error {
	match err {
		moq_mux::Error::Moq(err) => err.clone(),
		_ => moq_net::StreamError::Internal.into(),
	}
}

impl<T> std::ops::Deref for Kept<T> {
	type Target = T;

	fn deref(&self) -> &T {
		&self.inner
	}
}

impl<T> std::ops::DerefMut for Kept<T> {
	fn deref_mut(&mut self) -> &mut T {
		&mut self.inner
	}
}

// Legacy C data methods stamp writes on one fixed broadcast clock without retaining
// another catalog handle after a failed stream retires its listing.
struct Data<T> {
	producer: T,
	clock: moq_mux::Clock,
	children: Children,
}

/// A JSON snapshot producer, kept beside the signal that stops its refresh task.
struct JsonSnapshot {
	data: Data<moq_mux::json::Snapshot<serde_json::Value>>,

	/// Closed when the entry is removed, which wakes the refresh task to stop.
	_alive: kio::Producer<()>,
}

struct Container {
	inner: import::Container<Extra>,
	driver: crate::ffi::Background,
	children: Children,
}

#[derive(Default)]
pub struct Publish {
	/// Active broadcast producers for publishing.
	broadcasts: NonZeroSlab<Broadcast>,

	/// Single-codec media importers, fed timestamped frames.
	// Boxed because the codec splitters/imports are much larger than the container ones.
	media: NonZeroSlab<Kept<Box<import::Track>>>,

	/// Container importers, fed whole chunks. A separate space from `media` because a
	/// container publishes several tracks and carries its own timing, so it takes no
	/// per-frame timestamp.
	containers: NonZeroSlab<Container>,

	/// Raw track producers (no media/container/catalog framing).
	tracks: NonZeroSlab<Kept<moq_net::track::Producer>>,

	/// Raw group producers, created from a raw track producer.
	groups: NonZeroSlab<moq_net::group::Producer>,

	/// JSON snapshot producers (lossy latest-value tracks), each advertised in its broadcast's
	/// catalog for as long as it lives.
	json_snapshot: NonZeroSlab<JsonSnapshot>,

	/// JSON stream producers (lossless append-log tracks), advertised the same way.
	json_stream: NonZeroSlab<Data<moq_mux::json::Stream<serde_json::Value>>>,

	/// Demand watchers. Close signals shutdown; the task delivers a final callback, then removes itself.
	demand: NonZeroSlab<DemandEntry>,

	/// Track and group request handlers. Close drops the handler, so pending requests
	/// are rejected; the task delivers a final callback, then removes itself.
	dynamic: NonZeroSlab<Task>,

	/// Track requests delivered to a handler, freed on accept, abort, or free.
	track_request: NonZeroSlab<TrackRequest>,

	/// Group requests delivered to a handler, freed on accept, abort, or free.
	group_request: NonZeroSlab<GroupRequest>,

	/// Announcement handles for origin broadcasts, freed by the caller.
	publications: NonZeroSlab<Publication>,
}

/// A broadcast's announcement capability, beside the fill handle it was published with.
struct Publication {
	driver: crate::ffi::Background,
	/// The fill handle, which leaves the registry once the broadcast closes.
	broadcast: Id,
	inner: moq_net::origin::Publication,
}

impl Publish {
	fn broadcast(&self, id: Id) -> Result<&Broadcast, Error> {
		let broadcast = self.broadcasts.get(id).ok_or(Error::BroadcastNotFound)?;
		broadcast.driver.check()?;
		Ok(broadcast)
	}

	fn broadcast_mut(&mut self, id: Id) -> Result<&mut Broadcast, Error> {
		let broadcast = self.broadcasts.get_mut(id).ok_or(Error::BroadcastNotFound)?;
		broadcast.driver.check()?;
		Ok(broadcast)
	}

	/// Store an origin-created broadcast producer, attaching the catalog track
	/// every moq-c broadcast carries.
	pub fn create(
		&mut self,
		(mut broadcast, driver): (moq_net::broadcast::Producer, crate::ffi::Background),
	) -> Result<Id, Error> {
		let config = moq_mux::catalog::Config::default()
			.with_catalog(moq_mux::catalog::hang::Catalog::<moq_mux::catalog::hang::Extra>::default());
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, config)?;

		let id = self.broadcasts.insert(Broadcast {
			driver,
			producer: broadcast,
			catalog,
			video: BTreeMap::new(),
			audio: BTreeMap::new(),
			children: Children::default(),
			ended: false,
		})?;
		Ok(id)
	}

	/// Store an origin broadcast's fill handle and its publication, returning both handles.
	pub fn create_publication(
		&mut self,
		(publication, driver): (moq_net::origin::Publication, crate::ffi::Background),
	) -> Result<(Id, Id), Error> {
		let broadcast = self.create((publication.broadcast().clone(), driver.clone()))?;
		let id = self.publications.insert(Publication {
			driver,
			broadcast,
			inner: publication,
		});
		match id {
			Ok(id) => Ok((id, broadcast)),
			Err(err) => {
				self.broadcasts.remove(broadcast);
				Err(err)
			}
		}
	}

	/// Publish a prepared answer filled through `id`, returning its publication handle.
	pub fn admit(
		&mut self,
		id: Id,
		prepared: moq_net::origin::Prepared,
		driver: crate::ffi::Background,
	) -> Result<Id, Error> {
		if let Some(broadcast) = self.broadcasts.get(id) {
			broadcast.driver.check()?;
			if !broadcast.producer.is_clone(prepared.broadcast()) {
				return Err(Error::BroadcastNotFound);
			}
		}
		// A closed fill left this registry, and native admission refuses it.
		let publication = prepared.publish()?;
		self.publications.insert(Publication {
			driver,
			broadcast: id,
			inner: publication,
		})
	}

	/// The publication, refused with `Closed` once its broadcast closed.
	fn publication(&self, id: Id) -> Result<&Publication, Error> {
		let publication = self.publications.get(id).ok_or(Error::NotFound)?;
		publication.driver.check()?;
		if self.broadcasts.get(publication.broadcast).is_none() {
			return Err(Error::Moq(moq_net::Error::Closed));
		}
		Ok(publication)
	}

	/// Advertise the broadcast's exact path as a route. Announcing again re-prices
	/// in place. Until announced, the broadcast is invisible and unroutable.
	pub fn announce(&mut self, publication: Id, route: moq_net::origin::Route) -> Result<(), Error> {
		let publication = self.publication(publication)?;
		let result = publication.inner.announce(route);
		publication.driver.check()?;
		Ok(result?)
	}

	/// Retract the broadcast's exact-path advertisement, if any.
	pub fn unannounce(&mut self, publication: Id) -> Result<(), Error> {
		self.publication(publication)?.inner.unannounce();
		Ok(())
	}

	/// Release a publication handle; the broadcast and its route stay as they are.
	pub fn publication_free(&mut self, publication: Id) -> Result<(), Error> {
		self.publications.remove(publication).ok_or(Error::NotFound)?;
		Ok(())
	}

	/// The broadcast's track producer.
	pub(crate) fn producer(&mut self, id: Id) -> Result<&mut moq_net::broadcast::Producer, Error> {
		let broadcast = self.broadcast_mut(id)?;
		Ok(&mut broadcast.producer)
	}

	/// The broadcast's catalog producer.
	fn catalog(&mut self, id: Id) -> Result<&mut moq_mux::catalog::Producer<Extra>, Error> {
		let broadcast = self.broadcast_mut(id)?;
		Ok(&mut broadcast.catalog)
	}

	/// The broadcast's current catalog, as consumers would receive it next.
	#[cfg(test)]
	pub fn catalog_snapshot(&mut self, id: Id) -> Result<moq_mux::catalog::hang::Catalog<Extra>, Error> {
		Ok(self.catalog(id)?.snapshot())
	}

	/// What a producer publishing on the broadcast needs from it.
	/// Used by sibling modules (e.g. `audio`) that need to attach a new
	/// track to an existing publish.
	pub fn target(&mut self, id: Id) -> Result<Target<'_>, Error> {
		let broadcast = self.broadcast_mut(id)?;
		Ok(Target {
			producer: &mut broadcast.producer,
			catalog: &mut broadcast.catalog,
			children: &broadcast.children,
		})
	}

	/// End the broadcast for good, finalizing the catalog stream. Its children already ended.
	fn close(mut broadcast: Broadcast) -> Result<(), Error> {
		if broadcast.ended {
			broadcast.producer.close();
			return broadcast.driver.check();
		}
		let result = (|| {
			// Retire caller-authored entries while the catalog track is still open.
			{
				let mut guard = broadcast.catalog.modify()?;
				for name in broadcast.video.keys() {
					guard.video.renditions.remove(name);
				}
				for name in broadcast.audio.keys() {
					guard.audio.renditions.remove(name);
				}
				guard.commit()?;
			}
			// Close the broadcast first so it ends even if finalizing the catalog fails.
			broadcast.producer.close();
			broadcast.catalog.finish()?;
			Ok(())
		})();
		broadcast.driver.check()?;
		result
	}

	/// End the broadcast's content by finishing its catalog, with its entries still listed, so
	/// a late subscriber finds the finished tracks until [`State::publish_close`].
	pub fn catalog_finish(&mut self, broadcast: Id) -> Result<(), Error> {
		let broadcast = self.broadcast_mut(broadcast)?;
		let result = broadcast.catalog.finish();
		broadcast.ended |= result.is_ok();
		broadcast.driver.check()?;
		Ok(result?)
	}

	/// The children of a broadcast.
	fn children(&self, broadcast: Id) -> Result<Children, Error> {
		Ok(self.broadcast(broadcast)?.children.clone())
	}

	/// Insert a media importer as a child of the broadcast `children` belongs to.
	fn insert_media(&mut self, track: import::Track, children: Children) -> Result<Id, Error> {
		let media = Kept {
			inner: Box::new(track),
			children: children.clone(),
		};
		children.insert(&mut self.media, media, Child::Media)
	}

	pub fn audio(&mut self, broadcast: Id, init: import::AudioInit) -> Result<Id, Error> {
		let Broadcast {
			producer: broadcast,
			catalog,
			children,
			..
		} = self.broadcast(broadcast)?;
		let broadcast = broadcast.clone();
		let name = broadcast.unique_name(&format!(".{}", init.format));
		let request = broadcast.reserve_track(name)?;

		let track = import::Track::audio(request, catalog.reserve(), init)?;
		let children = children.clone();
		self.insert_media(track, children)
	}

	pub fn video(&mut self, broadcast: Id, init: import::VideoInit) -> Result<Id, Error> {
		let Broadcast {
			producer: broadcast,
			catalog,
			children,
			..
		} = self.broadcast(broadcast)?;
		let broadcast = broadcast.clone();
		let name = broadcast.unique_name(&format!(".{}", init.format));
		let request = broadcast.reserve_track(name)?;

		let track = import::Track::video(request, catalog.reserve(), init)?;
		let children = children.clone();
		self.insert_media(track, children)
	}

	pub fn container(&mut self, broadcast: Id, init: import::ContainerInit) -> Result<Id, Error> {
		let Broadcast {
			producer: broadcast,
			catalog,
			driver,
			children,
			..
		} = self.broadcast(broadcast)?;
		let container = import::Container::new(broadcast.clone(), catalog.reserve(), &init)?;
		let children = children.clone();
		let container = Container {
			inner: container,
			driver: driver.clone(),
			children: children.clone(),
		};
		children.insert(&mut self.containers, container, Child::Container)
	}

	pub fn media_frame(&mut self, media: Id, data: &[u8], timestamp: hang::container::Timestamp) -> Result<(), Error> {
		let track = self.media.get_mut(media).ok_or(Error::MediaNotFound)?;
		track.decode(data, Some(timestamp))?;
		Ok(())
	}

	/// Mark a timeline break on this media importer.
	pub fn media_discontinuity(&mut self, media: Id) -> Result<(), Error> {
		let track = self.media.get_mut(media).ok_or(Error::MediaNotFound)?;
		track.discontinuity()?;
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
		if let Err(err) = track.finish() {
			track.children.forget(Child::Media(media));
			let cause = cause(&err);
			track.abort(cause);
			return Err(err.into());
		}
		track.keep(Child::Media(media));
		Ok(())
	}

	/// Write a whole chunk of container bytes.
	///
	/// No timestamp: a container carries its tracks' timing itself.
	pub fn container_write(&mut self, container: Id, data: &[u8]) -> Result<(), Error> {
		let container = self.containers.get_mut(container).ok_or(Error::MediaNotFound)?;
		container.driver.check()?;
		let result = container.inner.decode(data);
		container.driver.check()?;
		Ok(result?)
	}

	/// Declare that the next chunk starts a new segment, rolling a group on every track.
	pub fn container_cut(&mut self, container: Id) -> Result<(), Error> {
		let container = self.containers.get_mut(container).ok_or(Error::MediaNotFound)?;
		container.driver.check()?;
		let result = container.inner.cut();
		container.driver.check()?;
		Ok(result?)
	}

	/// Start a new segment and number its groups `sequence`.
	pub fn container_seek(&mut self, container: Id, sequence: u64) -> Result<(), Error> {
		let container = self.containers.get_mut(container).ok_or(Error::MediaNotFound)?;
		container.driver.check()?;
		let result = container.inner.seek(sequence);
		container.driver.check()?;
		Ok(result?)
	}

	pub fn container_finish(&mut self, container: Id) -> Result<(), Error> {
		let id = container;
		let mut container = self.containers.remove(id).ok_or(Error::MediaNotFound)?;
		if let Err(err) = container.inner.finish() {
			container.children.forget(Child::Container(id));
			let driver = container.driver.clone();
			container.abort(cause(&err));
			driver.check()?;
			return Err(err.into());
		}
		container.driver.check()?;
		container.children.keep(Child::Container(id), container.inner);
		Ok(())
	}

	/// Insert or replace a caller-authored video rendition in the broadcast's catalog.
	///
	/// Errors if a media importer owns the name, since it publishes and retires its own rendition,
	/// or if the config changes the rendition's track identity, which takes a new name. The catalog
	/// is republished automatically.
	pub fn video_config(&mut self, broadcast: Id, name: &str, config: hang::catalog::VideoConfig) -> Result<(), Error> {
		let broadcast = self.broadcast_mut(broadcast)?;
		if !broadcast.video.contains_key(name) && broadcast.catalog.is_claimed::<hang::catalog::VideoConfig>(name) {
			return Err(Error::Hang(hang::Error::Duplicate(name.to_string())));
		}
		if let Some(previous) = broadcast.video.get(name)
			&& !previous.same_identity(&config)
		{
			return Err(moq_mux::Error::IdentityChanged(name.to_string()).into());
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
		let broadcast = self.broadcast_mut(broadcast)?;
		if !broadcast.audio.contains_key(name) && broadcast.catalog.is_claimed::<hang::catalog::AudioConfig>(name) {
			return Err(Error::Hang(hang::Error::Duplicate(name.to_string())));
		}
		if let Some(previous) = broadcast.audio.get(name)
			&& !previous.same_identity(&config)
		{
			return Err(moq_mux::Error::IdentityChanged(name.to_string()).into());
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
		let broadcast = self.broadcast_mut(broadcast)?;
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
		let broadcast = self.broadcast_mut(broadcast)?;
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

	/// The raw track producer behind a handle, for tests that publish what the C API
	/// can't express (an untimed frame).
	#[cfg(test)]
	pub(crate) fn track_producer(&mut self, track: Id) -> Option<&mut moq_net::track::Producer> {
		self.tracks.get_mut(track).map(|track| &mut track.inner)
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
	/// `on_demand` fires with a [`moq_demand`] value immediately and again on each change of the
	/// wanted group, then once with a terminal code: `0` when the track ends or the watcher is
	/// closed, negative when the track aborts. Seeding with the current state is what makes a late
	/// registration safe: a track that went unused before the watcher existed still reports it.
	pub fn demand(&mut self, demand: moq_net::track::Demand, on_demand: OnStatus) -> Result<Id, Error> {
		let (task, close) = Task::new(on_demand);
		let id = self.demand.insert(DemandEntry {
			task,
			demand: demand.clone(),
		})?;

		tokio::spawn(async move {
			let res = crate::ffi::run(Self::run_demand(on_demand, demand, close)).await;

			// Deliver one final terminal callback (code <= 0), then drop the entry.
			// Pull it out from under the lock so the callback never runs while held.
			let entry = State::lock().publish.demand.remove(id);
			if let Some(entry) = entry {
				entry.task.callback.call(res);
			}
		});

		Ok(id)
	}

	pub(crate) async fn run_demand(
		callback: OnStatus,
		mut demand: moq_net::track::Demand,
		mut close: oneshot::Receiver<()>,
	) -> Result<(), Error> {
		// The first change is the current state. Close is ignored until it is delivered, so a
		// watcher closed before its first poll still reports USED or UNUSED before the terminal.
		// An application dropping its producer ends the watcher; it does not keep the track
		// alive and reports the close as clean.
		let mut first = true;
		loop {
			let next = if first {
				first = false;
				demand.wanted_changed().await
			} else {
				tokio::select! {
					biased;
					_ = &mut close => return Ok(()),
					res = demand.wanted_changed() => res,
				}
			};
			let state = match next {
				Ok(Some(_)) => moq_demand::MOQ_DEMAND_USED,
				Ok(None) => moq_demand::MOQ_DEMAND_UNUSED,
				Err(moq_net::Error::Dropped) => return Ok(()),
				Err(err) => return Err(err.into()),
			};
			callback.call(state as i32);
		}
	}

	/// The group a demand watcher's subscribers want right now, until its terminal callback.
	pub fn demand_wanted(&self, watcher: Id) -> Result<Option<u64>, Error> {
		Ok(self.demand.get(watcher).ok_or(Error::NotFound)?.demand.wanted())
	}

	/// Stop a demand watcher. The task still delivers its terminal callback.
	pub fn demand_close(&mut self, watcher: Id) -> Result<(), Error> {
		self.demand
			.get_mut(watcher)
			.ok_or(Error::NotFound)?
			.task
			.close(Error::NotFound)
	}

	/// Serve subscriber requests for tracks the broadcast has not declared, delivering each as
	/// a track-request handle via `on_request`.
	///
	/// Without a live handler an unknown track name is refused, as before.
	pub fn dynamic(&mut self, broadcast: Id, on_request: OnStatus) -> Result<Id, Error> {
		let dynamic = self.producer(broadcast)?.requests();
		let Broadcast { driver, children, .. } = self.broadcast(broadcast)?;
		let dynamic = Dynamic::Broadcast(dynamic, broadcast, children.clone());
		self.spawn_dynamic(dynamic, on_request, driver.clone())
	}

	/// Serve fetches of uncached groups on a raw track, delivering each as a group-request
	/// handle via `on_group`.
	pub fn track_dynamic(&mut self, track: Id, on_group: OnStatus) -> Result<Id, Error> {
		let dynamic = self.tracks.get(track).ok_or(Error::TrackNotFound)?.requests();
		self.spawn_dynamic(Dynamic::Track(dynamic), on_group, Default::default())
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
			.requests();
		self.spawn_dynamic(Dynamic::Track(dynamic), on_group, Default::default())
	}

	fn spawn_dynamic(
		&mut self,
		dynamic: Dynamic,
		callback: OnStatus,
		driver: crate::ffi::Background,
	) -> Result<Id, Error> {
		self.dynamic.spawn(
			callback,
			|state| &mut state.publish.dynamic,
			|close| async move { driver.watch(Self::run_dynamic(callback, dynamic, close)).await },
		)
	}

	async fn run_dynamic(
		callback: OnStatus,
		mut dynamic: Dynamic,
		mut close: oneshot::Receiver<()>,
	) -> Result<(), Error> {
		// Each fill hands out its groups one at a time, alongside the other fills.
		let mut fills = kio::Tasks::new();
		loop {
			// The handler is owned here, so returning drops it and rejects whatever is pending.
			// `biased` so a pending close always wins over a ready request.
			let res = tokio::select! {
				biased;
				_ = &mut close => return Ok(()),
				res = kio::wait(|waiter| {
					let _ = fills.poll(waiter);
					match &mut dynamic {
						Dynamic::Broadcast(dynamic, broadcast, children) => {
							dynamic.poll_requested_track(waiter).map_ok(|request| {
								Request::Track(Box::new(TrackRequest {
									broadcast: *broadcast,
									children: children.clone(),
									request,
								}))
							})
						}
						Dynamic::Track(dynamic) => dynamic.poll_requested_fill(waiter).map_ok(Request::Fill),
					}
				}) => res,
			};

			// A finished broadcast or track, or its application dropping the producer, ends requests.
			let request = match res {
				Ok(request) => request,
				Err(moq_net::Error::Closed | moq_net::Error::Dropped) => return Ok(()),
				Err(err) => return Err(err.into()),
			};

			// Hold the lock only to buffer the request; release it before the callback.
			let id = match request {
				Request::Track(request) => State::lock().publish.track_request.insert(*request)?,
				Request::Fill(fill) => {
					let mut serve = Box::pin(Self::serve_fill(callback, Arc::new(fill)));
					fills.push(move |waiter: &kio::Waiter| waiter.poll_future(serve.as_mut()));
					continue;
				}
			};
			callback.call(id);
		}
	}

	/// Hand `fill` to C a group at a time, each once the one before it is answered or nobody
	/// reads it any more, until every group is answered or nobody waits on the fill.
	async fn serve_fill(callback: OnStatus, fill: Arc<moq_net::track::Fill>) {
		let mut last = None;
		while fill.is_used()
			&& let Some(sequence) = fill.next()
			&& last != Some(sequence)
		{
			last = Some(sequence);
			let (pending, answered) = oneshot::channel();
			let request = GroupRequest {
				fill: fill.clone(),
				sequence,
				_pending: pending,
			};
			let Ok(id) = State::lock().publish.group_request.insert(request) else {
				return;
			};
			callback.call(id);
			let unwanted = kio::wait(|waiter| fill.poll_unwanted(sequence, waiter));
			tokio::select! {
				_ = answered => {}
				// Withdrawn, so answering it fails as not found, and the fill moves on.
				_ = unwanted => {
					State::lock().publish.group_request.remove(id);
				}
			}
		}
	}

	/// Stop a track or group request handler. Pending requests are rejected, and the task
	/// still delivers its terminal callback.
	pub fn dynamic_close(&mut self, dynamic: Id) -> Result<(), Error> {
		self.dynamic.close(dynamic, Error::NotFound)
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
		let TrackRequest { children, request, .. } = self.track_request.remove(request).ok_or(Error::NotFound)?;
		let track = Kept {
			inner: request.accept(info),
			children: children.clone(),
		};
		children.insert(&mut self.tracks, track, Child::Track)
	}

	/// Accept a track request as an audio track, returning a media handle like [`Self::audio`].
	pub fn track_request_audio(&mut self, request: Id, init: import::AudioInit) -> Result<Id, Error> {
		let TrackRequest {
			broadcast,
			children,
			request,
		} = self.track_request.remove(request).ok_or(Error::NotFound)?;
		let catalog = self.catalog(broadcast)?;
		let track = import::Track::audio(request, catalog.reserve(), init)?;
		self.insert_media(track, children)
	}

	/// Accept a track request as a video track, returning a media handle like [`Self::video`].
	pub fn track_request_video(&mut self, request: Id, init: import::VideoInit) -> Result<Id, Error> {
		let TrackRequest {
			broadcast,
			children,
			request,
		} = self.track_request.remove(request).ok_or(Error::NotFound)?;
		let catalog = self.catalog(broadcast)?;
		let track = import::Track::video(request, catalog.reserve(), init)?;
		self.insert_media(track, children)
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
	pub fn group_request_info(&self, request: Id) -> Result<(u64, i8, u64), Error> {
		let request = self.group_request.get(request).ok_or(Error::NotFound)?;
		Ok((
			request.sequence,
			request.fill.priority(),
			request.fill.frame_start(request.sequence),
		))
	}

	/// Accept a group request, returning a group handle like [`Self::track_group`].
	///
	/// The producer is positioned at the request's `frame_start` so frames keep the
	/// indices they have in the group rather than restarting at 0.
	pub fn group_request_accept(&mut self, request: Id) -> Result<Id, Error> {
		let request = self.group_request.remove(request).ok_or(Error::NotFound)?;
		let group = request.fill.accept(request.sequence, None)?;
		self.groups.insert(group)
	}

	/// Reject a group request like [`Self::group_request_free`]; `error_code` is not delivered.
	pub fn group_request_abort(&mut self, request: Id, _error_code: u16) -> Result<(), Error> {
		self.group_request_free(request)
	}

	/// Drop a group request, failing the readers waiting on it as not found. The group is
	/// refused rather than dropped, so a later reader asks again.
	pub fn group_request_free(&mut self, request: Id) -> Result<(), Error> {
		let request = self.group_request.remove(request).ok_or(Error::NotFound)?;
		request
			.fill
			.refuse_groups(request.sequence..request.sequence + 1, moq_net::Error::NotFound);
		Ok(())
	}

	/// Create a raw track on a broadcast for arbitrary byte payloads.
	///
	/// No codec, container, or catalog framing. This is the moq-net primitive
	/// for non-media tracks. Pair it with [`Self::video_config`] / [`Self::audio_config`]
	/// if you want to describe the track in the catalog as well.
	pub fn track(&mut self, broadcast: Id, name: &str, info: Option<moq_net::track::Info>) -> Result<Id, Error> {
		let children = self.children(broadcast)?;
		let broadcast = self.producer(broadcast)?;
		let track = broadcast.create_track(name, info)?;
		let track = Kept {
			inner: track,
			children: children.clone(),
		};
		children.insert(&mut self.tracks, track, Child::Track)
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
	/// boundary and only releases the handle; the broadcast keeps the track.
	pub fn track_finish(&mut self, track: Id) -> Result<(), Error> {
		let id = track;
		let track = self.tracks.remove(id).ok_or(Error::TrackNotFound)?;
		if track.finished().is_none() {
			track.finish()?;
		}
		track.keep(Child::Track(id));
		Ok(())
	}

	/// Declare a raw track's exclusive final group sequence.
	pub fn track_finish_at(&mut self, track: Id, final_sequence: u64) -> Result<(), Error> {
		let track = self.tracks.get_mut(track).ok_or(Error::TrackNotFound)?;
		track.finish_at(moq_net::track::Position::group(final_sequence))?;
		Ok(())
	}

	/// Abort a raw track with an application error code.
	pub fn track_abort(&mut self, track: Id, error_code: u16) -> Result<(), Error> {
		let id = track;
		let track = self.tracks.remove(id).ok_or(Error::TrackNotFound)?;
		track.children.forget(Child::Track(id));
		track.inner.abort(moq_net::Error::App(error_code))?;
		Ok(())
	}

	/// Create a track on a broadcast and hand it, with the broadcast's catalog, to `publish`, which
	/// wraps it in a data producer that advertises the track in that catalog.
	///
	/// `refresh` sets the track's [`refresh`](moq_net::track::Info::refresh) ratio.
	fn data_track<T>(
		&mut self,
		broadcast: Id,
		name: &str,
		refresh: Option<u32>,
		publish: impl FnOnce(&moq_mux::catalog::Producer<Extra>, moq_net::track::Producer) -> moq_mux::Result<T>,
	) -> Result<Data<T>, Error> {
		let broadcast = self.broadcast_mut(broadcast)?;
		// Compressed: JSON is small and repetitive.
		let mut info = moq_net::track::Info::default()
			.with_timescale(moq_net::Timescale::MILLI)
			.with_compress(true);
		info.refresh = refresh;
		let track = broadcast.producer.create_track(name, info)?;
		let producer = publish(&broadcast.catalog, track)?;
		Ok(Data {
			producer,
			clock: broadcast.catalog.clock(),
			children: broadcast.children.clone(),
		})
	}

	/// Create a JSON snapshot track (lossy latest-value) on a broadcast, advertised in its catalog.
	///
	/// Values published via [`Self::json_snapshot_update`] reach subscribers as a single latest
	/// state; a late joiner only sees the newest value. The catalog entry (`json.tracks.<name>`,
	/// `mode: snapshot`) is written now and retired when the track finishes or fails.
	///
	/// With deltas on, the track carries a refresh ratio of 8: a join after more than eight
	/// snapshots' worth of deltas asks for the value restated in a fresh group, which a task
	/// answers until the track is finished.
	pub fn json_snapshot(
		&mut self,
		broadcast: Id,
		name: &str,
		config: moq_mux::json::Config,
		options: moq_mux::json::Options,
	) -> Result<Id, Error> {
		let refresh = options.deltas.then_some(8);
		let data = self.data_track(broadcast, name, refresh, |catalog, track| {
			catalog.json_snapshot(track, config, options)
		})?;
		let alive = kio::Producer::new(());
		let closed = alive.consume();
		let children = data.children.clone();
		let snapshot = JsonSnapshot { data, _alive: alive };
		let id = children.insert(&mut self.json_snapshot, snapshot, Child::JsonSnapshot)?;
		crate::ffi::Background::new(async move {
			Self::json_snapshot_refresh(id, closed).await;
			Ok(())
		});
		Ok(id)
	}

	/// Answer joiners of the JSON snapshot `id` until it is removed, or a refresh fails.
	///
	/// A failed restatement only stops the refreshes: the error that caused it (a closed track, a
	/// rejected write) reaches the caller through its next update.
	async fn json_snapshot_refresh(id: Id, closed: kio::Consumer<()>) {
		loop {
			let stop = kio::wait(|waiter| {
				// Check under the state lock that removes the entry, so a removal never slips between
				// the check and the lookup.
				let state = State::lock();
				if closed.poll_closed(waiter).is_ready() {
					return std::task::Poll::Ready(true);
				}
				match state.publish.json_snapshot.get(id) {
					Some(json) => json.data.producer.poll_refresh(waiter).map(|res| res.is_err()),
					None => std::task::Poll::Ready(true),
				}
			})
			.await;
			if stop {
				return;
			}
		}
	}

	/// Publish a new value to a JSON snapshot track. A no-op if unchanged.
	pub fn json_snapshot_update(&mut self, json: Id, value: serde_json::Value) -> Result<(), Error> {
		let json = &mut self.json_snapshot.get_mut(json).ok_or(Error::TrackNotFound)?.data;
		json.producer
			.update(moq_net::Timed::from(&value).at(json.clock.now()))?;
		Ok(())
	}

	/// Finish a JSON snapshot track and retire its catalog entry. No more values can be published.
	pub fn json_snapshot_finish(&mut self, json: Id) -> Result<(), Error> {
		let id = json;
		let json = self.json_snapshot.remove(id).ok_or(Error::TrackNotFound)?;
		json.data.children.forget(Child::JsonSnapshot(id));
		json.data.producer.finish()?;
		Ok(())
	}

	/// Create a JSON stream track (lossless append-log) on a broadcast, advertised in its catalog.
	///
	/// Every record appended via [`Self::json_stream_append`] is preserved and delivered in order.
	/// The catalog entry (`json.tracks.<name>`, `mode: stream`) lives as long as the track.
	pub fn json_stream(&mut self, broadcast: Id, name: &str, config: moq_mux::json::Config) -> Result<Id, Error> {
		let producer = self.data_track(broadcast, name, None, |catalog, track| {
			catalog.json_stream(track, config)
		})?;
		let children = producer.children.clone();
		children.insert(&mut self.json_stream, producer, Child::JsonStream)
	}

	/// Append one record to a JSON stream track.
	pub fn json_stream_append(&mut self, stream: Id, value: serde_json::Value) -> Result<(), Error> {
		let producer = self.json_stream.get_mut(stream).ok_or(Error::TrackNotFound)?;
		producer
			.producer
			.append(moq_net::Timed::from(&value).at(producer.clock.now()))?;
		Ok(())
	}

	/// Finish a JSON stream track and retire its catalog entry. No more records can be appended.
	pub fn json_stream_finish(&mut self, stream: Id) -> Result<(), Error> {
		let producer = self.json_stream.remove(stream).ok_or(Error::TrackNotFound)?;
		producer.children.forget(Child::JsonStream(stream));
		producer.producer.finish()?;
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

impl State {
	/// Release a broadcast and every child still open on it, returning what is left to end it.
	///
	/// The children end first, with `Unroutable` as a reader sees a publisher leave, so none
	/// outlives the broadcast it publishes on.
	pub fn publish_close(&mut self, broadcast: Id) -> Result<Closing, Error> {
		let broadcast = self
			.publish
			.broadcasts
			.remove(broadcast)
			.ok_or(Error::BroadcastNotFound)?;
		let err = moq_net::Error::Unroutable;
		let mut encoders: Vec<Box<dyn FnOnce(moq_net::Error) + Send>> = Vec::new();
		for child in broadcast.children.close() {
			match child {
				Child::Media(id) => self.publish.media.remove(id).map(|media| media.abort(err.clone())),
				Child::Container(id) => self.publish.containers.remove(id).map(|media| media.abort(err.clone())),
				Child::Track(id) => self.publish.tracks.remove(id).map(|track| track.abort(err.clone())),
				Child::Audio(id) => self
					.audio
					.remove(id)
					.ok()
					.map(|audio| encoders.push(Box::new(|err| audio.abort(err)))),
				Child::Video(id) => self
					.video
					.remove(id)
					.ok()
					.map(|video| encoders.push(Box::new(|err| video.abort(err)))),
				Child::JsonSnapshot(id) => self
					.publish
					.json_snapshot
					.remove(id)
					.map(|json| json.abort(err.clone())),
				Child::JsonStream(id) => self.publish.json_stream.remove(id).map(|json| json.abort(err.clone())),
			};
		}
		Ok(Closing { encoders, broadcast })
	}
}

/// A broadcast released by [`State::publish_close`], ended by [`Closing::run`] once the global
/// lock is released.
///
/// An encoder holds its own lock across a codec call, a drain included, so ending one under the
/// global lock would stall every unrelated call behind that codec.
#[must_use = "the broadcast ends only once run"]
pub struct Closing {
	encoders: Vec<Box<dyn FnOnce(moq_net::Error) + Send>>,
	broadcast: Broadcast,
}

impl Closing {
	/// End the released encoders, waiting out any call in flight on them, then the broadcast.
	pub fn run(self) -> Result<(), Error> {
		for abort in self.encoders {
			abort(moq_net::Error::Unroutable);
		}
		Publish::close(self.broadcast)
	}
}

#[cfg(test)]
mod children_tests {
	use super::*;

	const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

	/// A standalone broadcast with one raw track and one media importer, each read by a subscriber.
	async fn children(state: &mut State) -> (Id, Id, Id, [moq_net::track::Subscriber; 2]) {
		let broadcast = state
			.publish
			.create((moq_net::broadcast::Info::new().produce(), Default::default()))
			.unwrap();
		let consumer = state.publish.broadcast(broadcast).unwrap().producer.consume();
		let track = state.publish.track(broadcast, "raw", None).unwrap();
		let mut head = b"OpusHead".to_vec();
		head.extend_from_slice(&[1, 2, 0, 0, 0x80, 0xbb, 0, 0, 0, 0, 0]);
		let media = state
			.publish
			.audio(broadcast, import::AudioInit::new(import::AudioFormat::Opus, head))
			.unwrap();
		let name = state.publish.media_demand(media).unwrap().name().to_string();
		let readers = [
			consumer.track("raw").unwrap().subscribe(None).await.unwrap(),
			consumer.track(&name).unwrap().subscribe(None).await.unwrap(),
		];
		(broadcast, track, media, readers)
	}

	/// Closing a broadcast ends every child still open on it the way a publisher leaving does,
	/// and releases the child handles.
	#[tokio::test(start_paused = true)]
	async fn closing_a_broadcast_ends_its_open_tracks() {
		let mut state = State::new();
		let (broadcast, track, media, readers) = children(&mut state).await;
		state.publish_close(broadcast).unwrap().run().unwrap();
		for mut reader in readers {
			let read = moq_clock::timeout(TIMEOUT, reader.recv_group()).await;
			assert!(matches!(
				read.expect("the track carried on"),
				Err(moq_net::Error::Unroutable)
			));
		}
		assert!(matches!(state.publish.track_finish(track), Err(Error::TrackNotFound)));
		assert!(matches!(state.publish.media_finish(media), Err(Error::MediaNotFound)));
	}

	/// Closing a broadcast ends its data tracks the way it ends media ones, and releases their
	/// handles.
	#[tokio::test(start_paused = true)]
	async fn closing_a_broadcast_ends_its_open_data_tracks() {
		let mut state = State::new();
		let broadcast = state
			.publish
			.create((moq_net::broadcast::Info::new().produce(), Default::default()))
			.unwrap();
		let consumer = state.publish.broadcast(broadcast).unwrap().producer.consume();
		let json = moq_mux::json::Config::default;
		let json_snapshot = state
			.publish
			.json_snapshot(broadcast, "json-snapshot", json(), Default::default())
			.unwrap();
		let json_stream = state.publish.json_stream(broadcast, "json-stream", json()).unwrap();
		let mut readers = Vec::new();
		for name in ["json-snapshot", "json-stream"] {
			readers.push(consumer.track(name).unwrap().subscribe(None).await.unwrap());
		}

		state.publish_close(broadcast).unwrap().run().unwrap();
		for mut reader in readers {
			let read = moq_clock::timeout(TIMEOUT, reader.recv_group()).await;
			assert!(matches!(
				read.expect("the track carried on"),
				Err(moq_net::Error::Unroutable)
			));
		}
		let json = serde_json::Value::Null;
		assert!(matches!(
			state.publish.json_snapshot_update(json_snapshot, json.clone()),
			Err(Error::TrackNotFound)
		));
		assert!(matches!(
			state.publish.json_stream_append(json_stream, json),
			Err(Error::TrackNotFound)
		));
	}

	/// A track finished before its broadcast closes ends cleanly.
	#[tokio::test(start_paused = true)]
	async fn finishing_before_close_ends_tracks_cleanly() {
		let mut state = State::new();
		let (broadcast, track, media, readers) = children(&mut state).await;
		state.publish.track_finish(track).unwrap();
		state.publish.media_finish(media).unwrap();
		state.publish_close(broadcast).unwrap().run().unwrap();
		for mut reader in readers {
			let read = moq_clock::timeout(TIMEOUT, reader.recv_group()).await;
			assert!(matches!(read.expect("the track carried on"), Ok(None)));
		}
	}

	/// A child made after its broadcast closed, such as a request accepted late, is refused and
	/// ends the way a publisher leaving does.
	#[tokio::test(start_paused = true)]
	async fn a_child_of_a_closed_broadcast_is_refused() {
		let children = Children::default();
		children.close();
		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("late", None)
			.unwrap();
		let mut reader = track.subscribe(None);
		let mut tracks = NonZeroSlab::default();
		let kept = Kept {
			inner: track,
			children: children.clone(),
		};
		assert!(matches!(
			children.insert(&mut tracks, kept, Child::Track),
			Err(Error::BroadcastNotFound)
		));
		assert!(matches!(reader.recv_group().await, Err(moq_net::Error::Unroutable)));
	}
}

#[cfg(test)]
mod supervision_tests {
	use super::*;

	#[tokio::test(start_paused = true)]
	async fn admission_must_match_the_broadcast_it_filled() {
		let origin = moq_tokio::origin::spawn();
		let claim = origin.requests("room").unwrap();
		let pending = origin.consume().request("room/first", None);
		let prepared = claim
			.requested_broadcast()
			.await
			.unwrap()
			.prepare(moq_net::Epoch::mint())
			.unwrap();
		let mut publish = Publish::default();
		let first = publish
			.create((prepared.broadcast().clone(), Default::default()))
			.unwrap();
		let alias = {
			let original = publish.broadcast(first).unwrap();
			Broadcast {
				driver: original.driver.clone(),
				producer: original.producer.clone(),
				catalog: original.catalog.clone(),
				video: Default::default(),
				audio: Default::default(),
				children: Default::default(),
				ended: false,
			}
		};
		let clone = publish.broadcasts.insert(alias).unwrap();
		assert_ne!(first, clone);
		let publication = publish.admit(clone, prepared, Default::default()).unwrap();
		pending.await.unwrap();
		publish.unannounce(publication).unwrap();

		let pending = origin.consume().request("room/second", None);
		let prepared = claim
			.requested_broadcast()
			.await
			.unwrap()
			.prepare(moq_net::Epoch::mint())
			.unwrap();
		publish
			.create((prepared.broadcast().clone(), Default::default()))
			.unwrap();
		assert!(matches!(
			publish.admit(first, prepared, Default::default()),
			Err(Error::BroadcastNotFound)
		));
		assert!(pending.await.is_err());
	}

	#[tokio::test]
	async fn a_container_importer_keeps_the_origin_failure_after_other_owners_are_released() {
		let (origin, completion, trigger) = crate::ffi::panicking_origin();
		let mut publish = Publish::default();
		let (_publication, broadcast) = publish
			.create_publication((origin.create_broadcast("retained").unwrap(), completion.clone()))
			.unwrap();
		let container = publish
			.container(
				broadcast,
				import::ContainerInit::new(import::ContainerFormat::Ts, bytes::Bytes::new()),
			)
			.unwrap();
		publish.broadcasts.remove(broadcast).unwrap();
		drop(origin);
		*trigger
			.write()
			.unwrap_or_else(|_| unreachable!("test keeps its panic trigger open")) = true;
		assert!(matches!(completion.finished().await, Err(Error::Panic)));
		drop(completion);
		assert!(matches!(publish.container_cut(container), Err(Error::Panic)));
		assert!(matches!(publish.container_finish(container), Err(Error::Panic)));
	}

	#[tokio::test]
	async fn a_finish_after_a_driver_panic_reports_it_until_the_close() {
		let (origin, completion, trigger) = crate::ffi::panicking_origin();
		let mut state = State::new();
		let (_publication, broadcast) = state
			.publish
			.create_publication((origin.create_broadcast("retained").unwrap(), completion.clone()))
			.unwrap();
		let producer = &state.publish.broadcast(broadcast).unwrap().producer;
		// A dynamic handle outlives the consumed broadcast, so only an explicit close ends it.
		let _dynamic = producer.requests();
		let consumer = producer.consume();
		*trigger
			.write()
			.unwrap_or_else(|_| unreachable!("test keeps its panic trigger open")) = true;
		assert!(matches!(completion.finished().await, Err(Error::Panic)));
		assert!(matches!(state.publish.catalog_finish(broadcast), Err(Error::Panic)));
		// The handle stays the caller's to close, and the close still ends the broadcast.
		assert!(!consumer.is_closed());
		assert!(matches!(
			state.publish_close(broadcast).unwrap().run(),
			Err(Error::Panic)
		));
		assert!(consumer.is_closed());
	}
}
