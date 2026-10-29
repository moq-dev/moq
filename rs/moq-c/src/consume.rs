use std::{
	collections::BTreeMap,
	ffi::c_char,
	future::Future,
	pin::Pin,
	task::{Poll, ready},
};
use tokio::sync::oneshot;

use crate::decode::Read;
use crate::ffi::OnStatus;
use crate::task::Task;
use crate::{
	Error, Id, NonZeroSlab, State, moq_audio_config, moq_datagram, moq_frame, moq_json_value, moq_section, moq_string,
	moq_video_config, moq_video_properties,
};

struct ConsumeCatalog {
	broadcast: moq_net::broadcast::Consumer,

	/// The origin `broadcast` was resolved through, if any. A rendition may name a sibling
	/// broadcast, and only an origin can fetch one; a broadcast handed over without one (a
	/// local producer) leaves such a rendition unresolvable rather than silently wrong.
	origin: Option<crate::origin::Consumer>,

	// Carries the untyped `Extra` extension so application catalog sections survive
	// into `moq_catalog_section_*` instead of being dropped on parse.
	catalog: moq_mux::catalog::hang::Catalog<moq_mux::catalog::hang::Extra>,

	/// We need to store the codec information on the heap unfortunately.
	audio_codec: Vec<String>,
	video_codec: Vec<String>,

	/// Section names and their JSON, serialized on the heap so the section iterator
	/// and direct-lookup APIs can hand C borrowed pointers into stable storage.
	sections: Vec<(String, String)>,
}

impl ConsumeCatalog {
	fn rendition<C: Clone>(&self, name: &str, config: &C) -> Rendition<C> {
		Rendition {
			broadcast: self.broadcast.clone(),
			config: config.clone(),
			name: name.to_string(),
			driver: self
				.origin
				.as_ref()
				.map(|origin| origin.driver.clone())
				.unwrap_or_default(),
		}
	}
}

/// A catalog media section whose renditions are consumed by index.
pub(crate) trait Media: hang::catalog::Reference + Clone {
	/// The delivery priority its tracks subscribe with.
	const PRIORITY: i8;

	/// Its renditions, keyed by ID.
	fn renditions(catalog: &moq_mux::catalog::hang::Catalog<moq_mux::catalog::hang::Extra>) -> &BTreeMap<String, Self>;

	/// Whether the publisher is serving this rendition.
	fn enabled(&self) -> bool;
}

impl Media for hang::catalog::VideoConfig {
	const PRIORITY: i8 = hang::catalog::PRIORITY.video;

	fn renditions(catalog: &moq_mux::catalog::hang::Catalog<moq_mux::catalog::hang::Extra>) -> &BTreeMap<String, Self> {
		&catalog.video.renditions
	}

	fn enabled(&self) -> bool {
		self.enabled
	}
}

impl Media for hang::catalog::AudioConfig {
	const PRIORITY: i8 = hang::catalog::PRIORITY.audio;

	fn renditions(catalog: &moq_mux::catalog::hang::Catalog<moq_mux::catalog::hang::Extra>) -> &BTreeMap<String, Self> {
		&catalog.audio.renditions
	}

	fn enabled(&self) -> bool {
		self.enabled
	}
}

/// A catalog rendition to decode: the broadcast and track serving it, and the owner whose panic
/// ends the decode.
pub struct Rendition<C> {
	pub broadcast: moq_net::broadcast::Consumer,
	pub config: C,
	pub name: String,
	pub driver: crate::ffi::Background,
}

/// A broadcast handed to C, plus the origin cursor that named it.
///
/// The origin is what lets a catalog rendition reference a sibling broadcast: only an origin
/// can fetch one, and it must be the cursor that named this broadcast, since the reference
/// resolves against the path that cursor stamped. A broadcast that never came from an origin
/// has none, which makes such a rendition unresolvable rather than silently wrong.
#[derive(Clone)]
struct ConsumeBroadcast {
	broadcast: moq_net::broadcast::Consumer,
	origin: Option<crate::origin::Consumer>,
}

/// The broadcast serving a rendition, honoring its catalog `broadcast` reference.
///
/// An absent or empty reference names the catalog's own broadcast, which the caller already
/// holds; anything else resolves against that broadcast's path through the origin, which
/// deduplicates the subscription the catalog itself already holds. Awaited inside the track
/// task, never under the state lock, so `origin` is cloned out rather than borrowed.
async fn resolve(
	broadcast: moq_net::broadcast::Consumer,
	origin: Option<crate::origin::Consumer>,
	reference: Option<moq_net::path::Relative>,
) -> Result<moq_net::broadcast::Consumer, Error> {
	let Some(reference) = reference.filter(|reference| !reference.is_empty()) else {
		return Ok(broadcast);
	};

	let origin = origin.ok_or_else(|| Error::UnresolvableBroadcast(reference.as_str().to_string()))?;
	let source = moq_mux::Source::new(origin.inner, broadcast);
	// The C ABI does not expose a reference's epoch yet, so it reads like an older reader: the
	// first instance resolved, without the epoch's guarantee.
	origin
		.driver
		.watch(async { Ok(source.resolve(Some(&reference), None).await?) })
		.await
}

/// A raw track task also accepts subscription updates while it is running.
struct RawTaskEntry {
	task: Task,
	control: RawControl,
}

/// Where a raw track's subscription updates go.
enum RawControl {
	/// Waiting on SUBSCRIBE_OK, holding the latest update to apply once it arrives.
	Pending(Option<moq_net::track::Subscription>),
	/// Subscribed: updates apply at once, so a refusal reaches the caller.
	Live(moq_net::track::Control),
}

/// Outcome of polling a raw track source alongside its control channels.
enum RawStep<T> {
	/// A delivered value: the next group, or the next frame within a group.
	Item(T),
	/// The source is exhausted: the track finished, or the group was fully read.
	End,
	/// The consumer was closed; the task must unwind without re-polling `close`.
	Stop,
}

/// A raw frame, datagram, or JSON value timestamp in microseconds, `None` when untimed.
fn timestamp_us(timestamp: Option<moq_net::Timestamp>) -> Result<Option<u64>, Error> {
	timestamp
		.map(|timestamp| timestamp.as_micros().try_into())
		.transpose()
		.map_err(|_| Error::TimestampOverflow(moq_net::TimeOverflow))
}

/// A JSON value pre-serialized to a string, with its frame timestamp.
struct JsonValue {
	json: String,
	timestamp_us: Option<u64>,
}

impl JsonValue {
	fn new(value: moq_net::Timed<serde_json::Value>) -> Result<Self, Error> {
		Ok(Self {
			json: serde_json::to_string(&value.value)?,
			timestamp_us: timestamp_us(value.at)?,
		})
	}
}

#[derive(Default)]
pub struct Consume {
	/// Active broadcast consumers.
	broadcast: NonZeroSlab<ConsumeBroadcast>,

	/// Active catalog consumers and their broadcast references.
	catalog: NonZeroSlab<ConsumeCatalog>,

	/// Catalog consumer tasks. Close signals shutdown; the task delivers a final callback, then removes itself.
	catalog_task: NonZeroSlab<Task>,

	/// Track consumer tasks (video and audio).
	track_task: NonZeroSlab<Task>,

	/// Buffered frames ready for consumption.
	reads: NonZeroSlab<Read<moq_mux::container::Frame>>,

	/// Raw track consumer tasks (no media/container framing).
	raw_task: NonZeroSlab<RawTaskEntry>,

	/// Buffered raw frames ready for consumption.
	raw_frame: NonZeroSlab<moq_net::frame::Frame>,

	/// Raw track datagram consumer tasks (best-effort, parallel to `raw_task`).
	datagram_task: NonZeroSlab<Task>,

	/// Buffered datagrams ready for consumption.
	datagram: NonZeroSlab<moq_net::Datagram>,

	/// JSON consumer tasks (snapshot and stream share this slab).
	json_task: NonZeroSlab<Task>,

	/// Buffered JSON values ready for consumption.
	json_value: NonZeroSlab<JsonValue>,
}

impl Consume {
	/// Buffer a broadcast the origin handed out, keeping the cursor that named it so a catalog
	/// rendition referencing a sibling broadcast resolves through the same origin.
	pub fn start(
		&mut self,
		broadcast: moq_net::broadcast::Consumer,
		origin: Option<crate::origin::Consumer>,
	) -> Result<Id, Error> {
		self.broadcast.insert(ConsumeBroadcast { broadcast, origin })
	}

	pub fn catalog(&mut self, broadcast: Id, on_catalog: OnStatus) -> Result<Id, Error> {
		let ConsumeBroadcast { broadcast, origin } =
			self.broadcast.get(broadcast).ok_or(Error::BroadcastNotFound)?.clone();

		let driver = origin.as_ref().map(|origin| origin.driver.clone()).unwrap_or_default();

		// `subscribe` blocks on SUBSCRIBE_OK, so run it inside the task
		// to keep this entrypoint non-blocking.
		self.catalog_task.spawn(
			on_catalog,
			|state| &mut state.consume.catalog_task,
			|close| async move {
				driver
					.watch(async move {
						let catalog = broadcast
							.track(hang::catalog::Catalog::DEFAULT_NAME)?
							.subscribe(hang::catalog::Catalog::default_subscription())
							.await?;
						Self::run_catalog(on_catalog, broadcast.clone(), origin, catalog.into(), close).await
					})
					.await
			},
		)
	}

	async fn run_catalog(
		callback: OnStatus,
		broadcast: moq_net::broadcast::Consumer,
		origin: Option<crate::origin::Consumer>,
		mut catalog: moq_mux::catalog::hang::Consumer<moq_mux::catalog::hang::Extra>,
		mut close: oneshot::Receiver<()>,
	) -> Result<(), Error> {
		loop {
			// `biased` so a pending close always wins over a ready update: a hot
			// stream must not be able to starve the close signal, and we must not
			// deliver another update once close has been requested.
			let update = tokio::select! {
				biased;
				_ = &mut close => return Ok(()),
				next = catalog.next() => match next? {
					Some(update) => update,
					None => return Ok(()),
				},
			};

			// Unfortunately we need to store the codec information on the heap.
			let audio_codec = update
				.audio
				.renditions
				.values()
				.map(|config| config.codec.to_string())
				.collect();

			let video_codec = update
				.video
				.renditions
				.values()
				.map(|config| config.codec.to_string())
				.collect();

			// Serialize the untyped application sections to owned strings so the
			// C section APIs can borrow stable pointers from the snapshot.
			let sections = update
				.ext
				.iter()
				.map(|(name, value)| (name.clone(), value.to_string()))
				.collect();

			let snapshot = ConsumeCatalog {
				broadcast: broadcast.clone(),
				origin: origin.clone(),
				catalog: update,
				audio_codec,
				video_codec,
				sections,
			};

			// Hold the lock only to buffer the snapshot; release it before the callback.
			let snapshot_id = State::lock().consume.catalog.insert(snapshot)?;
			callback.call(Ok(snapshot_id));
		}
	}

	pub fn video_config(&mut self, catalog: Id, index: usize, dst: &mut moq_video_config) -> Result<(), Error> {
		let consume = self.catalog.get(catalog).ok_or(Error::CatalogNotFound)?;

		let (rendition, config) = consume
			.catalog
			.video
			.renditions
			.iter()
			.nth(index)
			.ok_or(Error::NoIndex)?;
		let codec = consume.video_codec.get(index).ok_or(Error::NoIndex)?;

		*dst = moq_video_config {
			preference: config.preference,
			name: rendition.as_str().as_ptr() as *const c_char,
			name_len: rendition.len(),
			codec: codec.as_str().as_ptr() as *const c_char,
			codec_len: codec.len(),
			description: config
				.description
				.as_ref()
				.map(|desc| desc.as_ptr())
				.unwrap_or(std::ptr::null()),
			description_len: config.description.as_ref().map(|desc| desc.len()).unwrap_or(0),
			coded_width: config.coded_width.unwrap_or(0),
			coded_height: config.coded_height.unwrap_or(0),
			container: crate::api::borrow_container(&config.container),
			label: config
				.label
				.as_ref()
				.map(|label| label.as_ptr() as *const c_char)
				.unwrap_or(std::ptr::null()),
			label_len: config.label.as_ref().map_or(0, String::len),
		};

		Ok(())
	}

	/// Return whether a video rendition may be selected.
	pub fn video_enabled(&self, catalog: Id, index: usize) -> Result<bool, Error> {
		let consume = self.catalog.get(catalog).ok_or(Error::CatalogNotFound)?;
		let (_, config) = consume
			.catalog
			.video
			.renditions
			.iter()
			.nth(index)
			.ok_or(Error::NoIndex)?;
		Ok(config.enabled)
	}

	/// Fill `dst` with the properties shared by every video rendition.
	pub fn video_properties(&self, catalog: Id, dst: &mut moq_video_properties) -> Result<(), Error> {
		let consume = self.catalog.get(catalog).ok_or(Error::CatalogNotFound)?;
		let display = consume.catalog.video.display.as_ref();

		*dst = moq_video_properties {
			display_width: display.map_or(0, |display| display.width),
			display_height: display.map_or(0, |display| display.height),
			has_display: display.is_some(),
			rotation: consume.catalog.video.rotation.unwrap_or_default(),
			has_rotation: consume.catalog.video.rotation.is_some(),
			flip: consume.catalog.video.flip.unwrap_or_default(),
			has_flip: consume.catalog.video.flip.is_some(),
		};

		Ok(())
	}

	pub fn audio_config(&mut self, catalog: Id, index: usize, dst: &mut moq_audio_config) -> Result<(), Error> {
		let consume = self.catalog.get(catalog).ok_or(Error::CatalogNotFound)?;

		let (rendition, config) = consume
			.catalog
			.audio
			.renditions
			.iter()
			.nth(index)
			.ok_or(Error::NoIndex)?;
		let codec = consume.audio_codec.get(index).ok_or(Error::NoIndex)?;

		*dst = moq_audio_config {
			name: rendition.as_str().as_ptr() as *const c_char,
			name_len: rendition.len(),
			codec: codec.as_str().as_ptr() as *const c_char,
			codec_len: codec.len(),
			description: config
				.description
				.as_ref()
				.map(|desc| desc.as_ptr())
				.unwrap_or(std::ptr::null()),
			description_len: config.description.as_ref().map(|desc| desc.len()).unwrap_or(0),
			sample_rate: config.sample_rate,
			channel_count: config.channel_count,
			container: crate::api::borrow_container(&config.container),
			label: config
				.label
				.as_ref()
				.map(|label| label.as_ptr() as *const c_char)
				.unwrap_or(std::ptr::null()),
			label_len: config.label.as_ref().map_or(0, String::len),
		};

		Ok(())
	}

	/// Number of untyped application catalog sections in this snapshot.
	pub fn catalog_section_count(&self, catalog: Id) -> Result<usize, Error> {
		let consume = self.catalog.get(catalog).ok_or(Error::CatalogNotFound)?;
		Ok(consume.sections.len())
	}

	/// Fill `dst` with the section at `index` (name + JSON value). The pointers
	/// borrow the snapshot and stay valid until it is freed.
	pub fn catalog_section_at(&self, catalog: Id, index: usize, dst: &mut moq_section) -> Result<(), Error> {
		let consume = self.catalog.get(catalog).ok_or(Error::CatalogNotFound)?;
		let (name, json) = consume.sections.get(index).ok_or(Error::NoIndex)?;

		*dst = moq_section {
			name: name.as_ptr() as *const c_char,
			name_len: name.len(),
			json: json.as_ptr() as *const c_char,
			json_len: json.len(),
		};

		Ok(())
	}

	/// Fill `dst` with the JSON value of the section named `name`. The pointer
	/// borrows the snapshot and stays valid until it is freed. Errors with
	/// [`Error::NotFound`] if no section with that name exists.
	pub fn catalog_section_get(&self, catalog: Id, name: &str, dst: &mut moq_string) -> Result<(), Error> {
		let consume = self.catalog.get(catalog).ok_or(Error::CatalogNotFound)?;
		let (_, json) = consume
			.sections
			.iter()
			.find(|(section, _)| section == name)
			.ok_or(Error::NotFound)?;

		*dst = moq_string {
			data: json.as_ptr() as *const c_char,
			len: json.len(),
		};

		Ok(())
	}

	pub fn catalog_close(&mut self, catalog: Id) -> Result<(), Error> {
		self.catalog_task.close(catalog, Error::CatalogNotFound)
	}

	pub fn catalog_free(&mut self, catalog: Id) -> Result<(), Error> {
		self.catalog.remove(catalog).ok_or(Error::CatalogNotFound)?;
		Ok(())
	}

	/// Subscribe to the video or audio rendition at catalog `index`, delivering its frames, gaps, and encoder ends.
	pub fn media<C: Media>(
		&mut self,
		catalog: Id,
		index: usize,
		max_delay: std::time::Duration,
		on_read: OnStatus,
	) -> Result<Id, Error>
	where
		for<'a> moq_mux::catalog::hang::Container: TryFrom<&'a C, Error = moq_mux::Error>,
	{
		let (consume, name, config) = self.select::<C>(catalog, index)?;
		if !config.enabled() {
			return Err(moq_net::Error::NotFound.into());
		}
		let name = hang::catalog::Reference::track(config, name).to_string();
		// Consume with the container the catalog actually advertises (Legacy / Cmaf / Loc)
		// instead of assuming Legacy, otherwise CMAF/fMP4 sources (e.g. ffmpeg moqenc,
		// browser @moq/publish) are misread as raw frames.
		let container = moq_mux::catalog::hang::Container::try_from(config)?;
		// The rendition may live in a sibling broadcast, so resolve its reference rather than
		// assuming the catalog's own broadcast serves the track.
		let reference = config.broadcast().cloned();
		let broadcast = consume.broadcast.clone();
		let origin = consume.origin.clone();
		let driver = origin.as_ref().map(|origin| origin.driver.clone()).unwrap_or_default();

		// `subscribe` blocks on SUBSCRIBE_OK, so run it inside the task.
		self.track_task.spawn(
			on_read,
			|state| &mut state.consume.track_task,
			|close| async move {
				driver
					.watch(async move {
						let broadcast = resolve(broadcast, origin, reference).await?;
						let track = broadcast
							.track(&name)?
							.subscribe(
								moq_net::track::Subscription::default()
									.with_priority(C::PRIORITY)
									.with_max_delay(max_delay),
							)
							.await?;
						let track = moq_mux::container::Consumer::new(track, container);
						Self::run_track(on_read, track, close).await
					})
					.await
			},
		)
	}

	async fn run_track(
		callback: OnStatus,
		mut track: moq_mux::container::Consumer<moq_mux::catalog::hang::Container>,
		mut close: oneshot::Receiver<()>,
	) -> Result<(), Error> {
		loop {
			// `biased` so a pending close always wins over a ready frame.
			let read = tokio::select! {
				biased;
				_ = &mut close => return Ok(()),
				read = track.read() => match read? {
					Some(moq_mux::container::Read::Frame { frame, .. }) => Read::Frame(frame),
					Some(moq_mux::container::Read::Gap { timestamp, duration }) => Read::gap(timestamp, duration)?,
					Some(moq_mux::container::Read::End { end, .. }) => Read::end(end)?,
					None => return Ok(()),
				},
			};

			// Hold the lock only to buffer the read; release it before the callback.
			let frame_id = State::lock().consume.reads.insert(read)?;
			callback.call(Ok(frame_id));
		}
	}

	pub fn track_close(&mut self, track: Id) -> Result<(), Error> {
		self.track_task.close(track, Error::TrackNotFound)
	}

	/// Read the tagged media item; payload pointers borrow it until [`Self::read_free`].
	pub fn read(&self, id: Id, dst: &mut crate::moq_media_read) -> Result<(), Error> {
		let read = self.reads.get(id).ok_or(Error::FrameNotFound)?;
		let (kind, gap, end_us) = read.info();
		let frame = match read {
			Read::Frame(f) => moq_frame {
				payload: f.payload.as_ptr(),
				payload_size: f.payload.len(),
				timestamp_us: f.timestamp.as_micros().try_into().map_err(|_| moq_net::TimeOverflow)?,
				timestamp_present: true,
				keyframe: f.keyframe,
			},
			_ => moq_frame {
				payload: std::ptr::null(),
				payload_size: 0,
				timestamp_us: 0,
				timestamp_present: false,
				keyframe: false,
			},
		};
		*dst = crate::moq_media_read {
			kind,
			frame,
			gap,
			end_us,
		};

		Ok(())
	}

	pub fn read_free(&mut self, frame: Id) -> Result<(), Error> {
		self.reads.remove(frame).ok_or(Error::FrameNotFound)?;
		Ok(())
	}

	pub fn close(&mut self, consume: Id) -> Result<(), Error> {
		self.broadcast.remove(consume).ok_or(Error::BroadcastNotFound)?;
		Ok(())
	}

	/// Subscribe to a raw track by name, delivering each frame's payload as-is.
	///
	/// No catalog lookup or container parsing. This is the moq-net primitive for
	/// non-media tracks. `on_frame` is called with a raw frame ID for each frame,
	/// in sequence order. Frames must be released with [`Self::raw_frame_close`].
	pub fn raw_track(
		&mut self,
		broadcast: Id,
		name: &str,
		subscription: Option<moq_net::track::Subscription>,
		on_frame: OnStatus,
	) -> Result<Id, Error> {
		let context = self.broadcast.get(broadcast).ok_or(Error::BroadcastNotFound)?;
		let driver = context
			.origin
			.as_ref()
			.map(|origin| origin.driver.clone())
			.unwrap_or_default();
		let broadcast = context.broadcast.clone();
		let name = name.to_string();

		let (task, close) = Task::new(on_frame);
		let control = RawControl::Pending(None);
		let id = self.raw_task.insert(RawTaskEntry { task, control })?;

		// `subscribe` blocks on SUBSCRIBE_OK, so run it inside the task.
		tokio::spawn(async move {
			let res = crate::ffi::run(driver.watch(async move {
				let track = broadcast.track(&name)?.subscribe(subscription.clone()).await?.ordered();
				let control = track.control();
				Self::update_raw(&control, Self::raw_subscription(subscription))?;
				// Go live under the lock, so no later update lands before the pending one.
				if let Some(entry) = State::lock().consume.raw_task.get_mut(id) {
					let pending = std::mem::replace(&mut entry.control, RawControl::Live(control.clone()));
					if let RawControl::Pending(Some(subscription)) = pending {
						Self::update_raw(&control, subscription)?;
					}
				}
				Self::run_raw(on_frame, track, close).await
			}))
			.await;

			// Deliver one final terminal callback (code <= 0), then drop the entry.
			// Pull it out from under the lock so the callback never runs while held.
			let entry = State::lock().consume.raw_task.remove(id);
			if let Some(entry) = entry {
				entry.task.callback.call(res);
			}
		});

		Ok(id)
	}

	/// A raw reader's preferences, which join at the latest group unless a range names
	/// a floor of its own.
	fn raw_subscription(subscription: Option<moq_net::track::Subscription>) -> moq_net::track::Subscription {
		let mut subscription = subscription.unwrap_or_default();
		for range in &mut subscription.ranges {
			if range.start == moq_net::track::Range::oldest().start {
				range.start = moq_net::track::Range::live().start;
			}
		}
		subscription
	}

	fn update_raw(control: &moq_net::track::Control, subscription: moq_net::track::Subscription) -> Result<(), Error> {
		match control.update(subscription) {
			// A closed track makes the update meaningless; the reader already sees the close.
			Ok(()) | Err(moq_net::Error::Closed) => Ok(()),
			Err(err) => Err(err.into()),
		}
	}

	async fn run_raw(
		callback: OnStatus,
		mut track: moq_net::track::Ordered,
		mut close: oneshot::Receiver<()>,
	) -> Result<(), Error> {
		// Deliver every frame in sequence order, reading all frames within each
		// group rather than the one-frame-per-group convenience. This is the
		// "raw track contents" model: the consumer sees exactly what the
		// producer wrote, regardless of how it was grouped.
		//
		// `close` is a oneshot that panics if polled after completion, so a `Stop`
		// must unwind the whole task rather than fall through to the outer loop.
		loop {
			let mut group =
				match moq_net::kio::wait(|waiter| -> Poll<Result<RawStep<moq_net::group::Consumer>, Error>> {
					if Pin::new(&mut close).poll(&mut waiter.context()).is_ready() {
						return Poll::Ready(Ok(RawStep::Stop));
					}
					match ready!(track.poll_next_group(waiter))? {
						Some(group) => Poll::Ready(Ok(RawStep::Item(group))),
						None => Poll::Ready(Ok(RawStep::End)),
					}
				})
				.await?
				{
					RawStep::Item(group) => group,
					// Track finished or the consumer was closed: nothing left to deliver.
					RawStep::End | RawStep::Stop => return Ok(()),
				};

			loop {
				let frame = match moq_net::kio::wait(|waiter| -> Poll<Result<RawStep<moq_net::frame::Frame>, Error>> {
					if Pin::new(&mut close).poll(&mut waiter.context()).is_ready() {
						return Poll::Ready(Ok(RawStep::Stop));
					}
					match ready!(group.poll_read_frame(waiter))? {
						Some(frame) => Poll::Ready(Ok(RawStep::Item(frame))),
						None => Poll::Ready(Ok(RawStep::End)),
					}
				})
				.await?
				{
					RawStep::Item(frame) => frame,
					// Group fully read: advance to the next group.
					RawStep::End => break,
					// Consumer closed mid-group: terminate without touching `close` again.
					RawStep::Stop => return Ok(()),
				};

				// Hold the lock only to buffer the read; release it before the callback.
				let frame_id = State::lock().consume.raw_frame.insert(frame)?;
				callback.call(Ok(frame_id));
			}
		}
	}

	pub fn raw_track_update(
		&mut self,
		track: Id,
		subscription: Option<moq_net::track::Subscription>,
	) -> Result<(), Error> {
		let entry = self.raw_task.get_mut(track).ok_or(Error::TrackNotFound)?;
		let subscription = Self::raw_subscription(subscription);
		match &mut entry.control {
			RawControl::Pending(pending) => *pending = Some(subscription),
			RawControl::Live(control) => Self::update_raw(control, subscription)?,
		}
		Ok(())
	}

	pub fn raw_track_close(&mut self, track: Id) -> Result<(), Error> {
		self.raw_task
			.get_mut(track)
			.ok_or(Error::TrackNotFound)?
			.task
			.close(Error::TrackNotFound)
	}

	/// Fill `dst` with a raw frame's payload. The pointer is valid until the
	/// frame is released with [`Self::raw_frame_close`].
	pub fn raw_frame(&self, frame: Id, dst: &mut moq_frame) -> Result<(), Error> {
		let frame = self.raw_frame.get(frame).ok_or(Error::FrameNotFound)?;
		let timestamp_us = timestamp_us(frame.timestamp)?;

		*dst = moq_frame {
			payload: frame.payload.as_ptr(),
			payload_size: frame.payload.len(),
			timestamp_us: timestamp_us.unwrap_or_default(),
			timestamp_present: timestamp_us.is_some(),
			keyframe: false,
		};

		Ok(())
	}

	pub fn raw_frame_close(&mut self, frame: Id) -> Result<(), Error> {
		self.raw_frame.remove(frame).ok_or(Error::FrameNotFound)?;
		Ok(())
	}

	/// Subscribe to a raw track's best-effort datagrams by name.
	///
	/// Parallel to [`Self::raw_track`], but delivers datagrams instead of frames.
	/// `on_datagram` is called with a datagram ID for each datagram in arrival order;
	/// each must be released with [`Self::datagram_close`].
	pub fn datagram_track(&mut self, broadcast: Id, name: &str, on_datagram: OnStatus) -> Result<Id, Error> {
		let context = self.broadcast.get(broadcast).ok_or(Error::BroadcastNotFound)?;
		let driver = context
			.origin
			.as_ref()
			.map(|origin| origin.driver.clone())
			.unwrap_or_default();
		let broadcast = context.broadcast.clone();
		let name = name.to_string();

		// `subscribe` blocks on SUBSCRIBE_OK, so run it inside the task.
		self.datagram_task.spawn(
			on_datagram,
			|state| &mut state.consume.datagram_task,
			|close| async move {
				driver
					.watch(async move {
						let track = broadcast.track(&name)?.subscribe(None).await?;
						Self::run_datagrams(on_datagram, track, close).await
					})
					.await
			},
		)
	}

	async fn run_datagrams(
		callback: OnStatus,
		mut track: moq_net::track::Subscriber,
		mut close: oneshot::Receiver<()>,
	) -> Result<(), Error> {
		loop {
			// `biased` so a pending close always wins over a ready datagram.
			let datagram = tokio::select! {
				biased;
				_ = &mut close => return Ok(()),
				datagram = track.recv_datagram() => match datagram? {
					Some(datagram) => datagram,
					None => return Ok(()),
				},
			};

			// Hold the lock only to buffer the datagram; release it before the callback.
			let id = State::lock().consume.datagram.insert(datagram)?;
			callback.call(Ok(id));
		}
	}

	pub fn datagram_track_close(&mut self, track: Id) -> Result<(), Error> {
		self.datagram_task.close(track, Error::TrackNotFound)
	}

	/// Fill `dst` with a datagram's payload, timestamp, and sequence. The pointer is
	/// valid until the datagram is released with [`Self::datagram_close`].
	pub fn datagram(&self, datagram: Id, dst: &mut moq_datagram) -> Result<(), Error> {
		let value = self.datagram.get(datagram).ok_or(Error::FrameNotFound)?;

		let timestamp_us = timestamp_us(value.timestamp)?;

		*dst = moq_datagram {
			payload: value.payload.as_ptr(),
			payload_size: value.payload.len(),
			timestamp_us: timestamp_us.unwrap_or_default(),
			timestamp_present: timestamp_us.is_some(),
			sequence: value.sequence,
		};

		Ok(())
	}

	pub fn datagram_close(&mut self, datagram: Id) -> Result<(), Error> {
		self.datagram.remove(datagram).ok_or(Error::FrameNotFound)?;
		Ok(())
	}

	/// Subscribe to a JSON snapshot track (lossy latest-value) by name.
	///
	/// `on_value` is called with a value ID for each state, in order; a group superseded by a
	/// newer one before it is read is skipped. Values must be released with
	/// [`Self::json_value_close`].
	pub fn json_snapshot(&mut self, broadcast: Id, name: &str, on_value: OnStatus) -> Result<Id, Error> {
		let context = self.broadcast.get(broadcast).ok_or(Error::BroadcastNotFound)?;
		let driver = context
			.origin
			.as_ref()
			.map(|origin| origin.driver.clone())
			.unwrap_or_default();
		let broadcast = context.broadcast.clone();
		let name = name.to_string();

		// `subscribe` blocks on SUBSCRIBE_OK, so run it inside the task.
		self.json_task.spawn(
			on_value,
			|state| &mut state.consume.json_task,
			|close| async move {
				driver
					.watch(async move {
						let track = broadcast.track(&name)?.subscribe(None).await?;
						let consumer =
							moq_json::snapshot::Consumer::<serde_json::Value>::new(track, Default::default());
						Self::run_json_snapshot(on_value, consumer, close).await
					})
					.await
			},
		)
	}

	async fn run_json_snapshot(
		callback: OnStatus,
		mut consumer: moq_json::snapshot::Consumer<serde_json::Value>,
		mut close: oneshot::Receiver<()>,
	) -> Result<(), Error> {
		loop {
			// `biased` so a pending close always wins over a ready value.
			let value = tokio::select! {
				biased;
				_ = &mut close => return Ok(()),
				next = consumer.next() => match next? {
					Some(value) => value,
					None => return Ok(()),
				},
			};

			// Serialize before locking, hold the lock only to buffer the value, and release it
			// before the callback.
			let value = JsonValue::new(value)?;
			let value_id = State::lock().consume.json_value.insert(value)?;
			callback.call(Ok(value_id));
		}
	}

	/// Subscribe to a JSON stream track (lossless append-log) by name.
	///
	/// `on_value` is called with a value ID for each record, in order. Values must be released
	/// with [`Self::json_value_close`].
	pub fn json_stream(&mut self, broadcast: Id, name: &str, on_value: OnStatus) -> Result<Id, Error> {
		let context = self.broadcast.get(broadcast).ok_or(Error::BroadcastNotFound)?;
		let driver = context
			.origin
			.as_ref()
			.map(|origin| origin.driver.clone())
			.unwrap_or_default();
		let broadcast = context.broadcast.clone();
		let name = name.to_string();

		// `subscribe` blocks on SUBSCRIBE_OK, so run it inside the task.
		self.json_task.spawn(
			on_value,
			|state| &mut state.consume.json_task,
			|close| async move {
				driver
					.watch(async move {
						let track = broadcast.track(&name)?.subscribe(None).await?;
						let consumer = moq_json::stream::Consumer::<serde_json::Value>::new(track, Default::default());
						Self::run_json_stream(on_value, consumer, close).await
					})
					.await
			},
		)
	}

	async fn run_json_stream(
		callback: OnStatus,
		mut consumer: moq_json::stream::Consumer<serde_json::Value>,
		mut close: oneshot::Receiver<()>,
	) -> Result<(), Error> {
		loop {
			// `biased` so a pending close always wins over a ready record.
			let value = tokio::select! {
				biased;
				_ = &mut close => return Ok(()),
				next = consumer.next() => match next? {
					Some(value) => value,
					None => return Ok(()),
				},
			};

			let value = JsonValue::new(value)?;
			let value_id = State::lock().consume.json_value.insert(value)?;
			callback.call(Ok(value_id));
		}
	}

	/// Signal a JSON consumer task (snapshot or stream) to stop. The task still delivers one
	/// final terminal callback and then removes itself, so `user_data` stays valid until then.
	pub fn json_close(&mut self, task: Id) -> Result<(), Error> {
		self.json_task.close(task, Error::TrackNotFound)
	}

	/// Fill `dst` with a JSON value delivered via a consumer callback. The pointer is valid
	/// until the value is released with [`Self::json_value_close`].
	pub fn json_value(&self, value: Id, dst: &mut moq_json_value) -> Result<(), Error> {
		let value = self.json_value.get(value).ok_or(Error::FrameNotFound)?;

		*dst = moq_json_value {
			json: value.json.as_ptr() as *const c_char,
			json_len: value.json.len(),
			timestamp_us: value.timestamp_us.unwrap_or_default(),
			timestamp_present: value.timestamp_us.is_some(),
		};

		Ok(())
	}

	/// Release a JSON value and clean up its resources.
	pub fn json_value_close(&mut self, value: Id) -> Result<(), Error> {
		self.json_value.remove(value).ok_or(Error::FrameNotFound)?;
		Ok(())
	}

	/// Look up a video or audio rendition by catalog index, mirroring the selection in [`Self::media`].
	pub fn rendition<C: Media>(&self, catalog: Id, index: usize) -> Result<Rendition<C>, Error> {
		let (consume, name, config) = self.select(catalog, index)?;
		Ok(consume.rendition(name, config))
	}

	/// A catalog and its rendition at `index`, in catalog order.
	fn select<C: Media>(&self, catalog: Id, index: usize) -> Result<(&ConsumeCatalog, &String, &C), Error> {
		let consume = self.catalog.get(catalog).ok_or(Error::CatalogNotFound)?;
		let (name, config) = C::renditions(&consume.catalog)
			.iter()
			.nth(index)
			.ok_or(Error::NoIndex)?;
		Ok((consume, name, config))
	}
}

#[cfg(test)]
mod supervision_tests {
	use super::*;

	#[tokio::test]
	async fn a_decoded_rendition_watches_the_origin_that_named_it() {
		let (origin, driver, trigger) = crate::ffi::panicking_origin();
		let broadcast = moq_net::broadcast::Info::new().produce();
		let mut catalog = moq_mux::catalog::hang::Catalog::<moq_mux::catalog::hang::Extra>::default();
		catalog.audio.renditions.insert(
			"audio".into(),
			hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2),
		);
		let mut consume = Consume::default();
		let id = consume
			.catalog
			.insert(ConsumeCatalog {
				broadcast: broadcast.consume(),
				origin: Some(crate::origin::Consumer {
					inner: origin.consume(),
					driver: driver.clone(),
				}),
				catalog,
				audio_codec: Vec::new(),
				video_codec: Vec::new(),
				sections: Vec::new(),
			})
			.unwrap();
		let rendition = consume.rendition::<hang::catalog::AudioConfig>(id, 0).unwrap();
		*trigger
			.write()
			.unwrap_or_else(|_| unreachable!("test keeps its panic trigger open")) = true;
		assert!(matches!(driver.finished().await, Err(Error::Panic)));
		let result = rendition.driver.check();
		assert!(matches!(result, Err(Error::Panic)), "{result:?}");
	}
}

#[cfg(test)]
mod read_tests {
	use super::*;
	use moq_mux::container::Container as _;

	extern "C" fn received(data: *mut std::ffi::c_void, code: i32) {
		// The vector is held until run_track completes on this thread.
		unsafe { &mut *data.cast::<Vec<i32>>() }.push(code);
	}

	#[tokio::test(start_paused = true)]
	async fn encoded_media_c_reads_preserve_breaks_and_ownership() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("encoded", hang::container::track_info(0))
			.unwrap();
		let format = || moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Audio);
		let consumer = moq_mux::container::Consumer::new(
			track.subscribe(moq_net::track::Subscription::default().with_max_delay(std::time::Duration::ZERO)),
			format(),
		);
		let timestamp = |us| moq_net::Timestamp::from_micros(us).unwrap();
		let write = |sequence, us, payload: bytes::Bytes| {
			let mut group = track.create_group(moq_net::group::Info { sequence }).unwrap();
			format()
				.write(
					&mut group,
					&[moq_mux::container::Frame {
						timestamp: timestamp(us),
						payload,
						keyframe: true,
						duration: None,
					}],
				)
				.unwrap();
			group.finish().unwrap();
		};
		let mut ids = Vec::<i32>::new();
		let callback = unsafe { OnStatus::new((&mut ids as *mut Vec<i32>).cast(), Some(received)) }.unwrap();
		let (_close, closed) = oneshot::channel();
		let mut run = std::pin::pin!(Consume::run_track(callback, consumer, closed));
		write(0, 10_000, bytes::Bytes::from_static(b"encoded"));
		std::future::poll_fn(|cx| {
			assert!(run.as_mut().poll(cx).is_pending());
			Poll::Ready(())
		})
		.await;
		assert_eq!(ids.len(), 1);
		write(2, 20_000, bytes::Bytes::new());
		track.finish().unwrap();
		run.await.unwrap();
		assert_eq!(ids.len(), 3);
		let mut read = std::mem::MaybeUninit::<crate::moq_media_read>::uninit();
		for (index, id) in ids.into_iter().enumerate() {
			assert!(id > 0);
			assert_eq!(unsafe { crate::moq_consume_read(id as u32, read.as_mut_ptr()) }, 0);
			let read = unsafe { read.assume_init_read() };
			match index {
				0 => {
					assert_eq!(read.kind, crate::moq_decode_read_kind::MOQ_DECODE_READ_FRAME);
					assert_eq!(
						unsafe { std::slice::from_raw_parts(read.frame.payload, read.frame.payload_size) },
						b"encoded"
					);
					assert_eq!(read.frame.timestamp_us, 10_000);
					assert!(read.frame.timestamp_present && read.frame.keyframe);
					assert_eq!(read.gap.duration_us, 0);
					assert_eq!(read.end_us, 0);
				}
				1 => {
					assert_eq!(read.kind, crate::moq_decode_read_kind::MOQ_DECODE_READ_GAP);
					assert_eq!((read.gap.timestamp_us, read.gap.duration_us), (10_000, 10_000));
					assert!(read.frame.payload.is_null());
					assert_eq!(read.end_us, 0);
				}
				2 => {
					assert_eq!(read.kind, crate::moq_decode_read_kind::MOQ_DECODE_READ_END);
					assert_eq!(read.end_us, 20_000);
					assert!(read.frame.payload.is_null());
					assert_eq!(read.gap.duration_us, 0);
				}
				_ => unreachable!(),
			}
			assert_eq!(crate::moq_consume_read_free(id as u32), 0);
			assert!(crate::moq_consume_read_free(id as u32) < 0);
		}
	}
}
