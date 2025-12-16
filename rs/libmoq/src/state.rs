use std::collections::HashMap;
use std::ffi::c_char;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use hang::TrackConsumer;
use moq_lite::coding::Buf;
use tokio::sync::oneshot;
use url::Url;

use crate::ffi::OnStatus;
use crate::{ffi, Announced, AudioTrack, Error, Frame, Id, NonZeroSlab, VideoTrack};

/// Global state managing all active resources.
///
/// Stores all sessions, origins, broadcasts, tracks, and frames in slab allocators,
/// returning opaque IDs to C callers. Also manages async tasks via oneshot channels
/// for cancellation.
// TODO split this up into separate structs/mutexes
#[derive(Default)]
pub struct State {
	/// Active origin producers for publishing and consuming broadcasts.
	origin: NonZeroSlab<moq_lite::OriginProducer>,

	/// Session task cancellation channels.
	session_task: NonZeroSlab<oneshot::Sender<()>>,

	/// Broadcast announcement information (path, active status).
	announced: NonZeroSlab<(String, bool)>,

	/// Announcement listener task cancellation channels.
	announced_task: NonZeroSlab<oneshot::Sender<()>>,

	/// Active broadcast producers for publishing.
	publish_broadcast: NonZeroSlab<hang::BroadcastProducer>,

	/// Active media encoders/decoders for publishing.
	publish_media: NonZeroSlab<hang::import::Decoder>,

	/// Active broadcast consumers.
	consume_broadcast: NonZeroSlab<hang::BroadcastConsumer>,

	/// Active catalog consumers and their broadcast references.
	consume_catalog: NonZeroSlab<(hang::catalog::Catalog, moq_lite::BroadcastConsumer)>,

	/// Catalog consumer task cancellation channels.
	consume_catalog_task: NonZeroSlab<oneshot::Sender<()>>,

	/// We need to store the codec information on the heap.
	/// Key: Catalog ID, Audio Index
	/// Value: the codec name.
	consume_catalog_audio: HashMap<(Id, usize), String>,

	/// We need to store the codec information on the heap.
	/// Key: Catalog ID, Video Index
	/// Value: the codec name.
	consume_catalog_video: HashMap<(Id, usize), String>,

	/// Audio track consumer task cancellation channels.
	consume_audio_task: NonZeroSlab<oneshot::Sender<()>>,

	/// Video track consumer task cancellation channels.
	consume_video_task: NonZeroSlab<oneshot::Sender<()>>,

	/// Buffered frames ready for consumption.
	consume_frame: NonZeroSlab<hang::Frame>,
}

impl State {
	pub fn session_connect(
		&mut self,
		url: Url,
		publish: Option<Id>,
		consume: Option<Id>,
		mut callback: ffi::OnStatus,
	) -> Result<Id, Error> {
		let publish = publish
			.map(|id| self.origin.get(id).ok_or(Error::NotFound))
			.transpose()?
			.map(|origin| origin.consume());
		let consume = consume
			.map(|id| self.origin.get(id).cloned().ok_or(Error::NotFound))
			.transpose()?;

		// Used just to notify when the session is removed from the map.
		let closed = oneshot::channel();

		let id = self.session_task.insert(closed.0);
		tokio::spawn(async move {
			let res = tokio::select! {
				// No more receiver, which means [session_close] was called.
				_ = closed.1 => Err(Error::Closed),
				// The connection failed.
				res = Self::session_connect_run(url, publish, consume, &mut callback) => res,
			};
			callback.call(res);
		});

		Ok(id)
	}

	async fn session_connect_run(
		url: Url,
		publish: Option<moq_lite::OriginConsumer>,
		consume: Option<moq_lite::OriginProducer>,
		callback: &mut ffi::OnStatus,
	) -> Result<(), Error> {
		let config = moq_native::ClientConfig::default();
		let client = config.init().map_err(|err| Error::Connect(Arc::new(err)))?;
		let connection = client.connect(url).await.map_err(|err| Error::Connect(Arc::new(err)))?;
		let session = moq_lite::Session::connect(connection, publish, consume).await?;
		callback.call(());

		session.closed().await?;
		Ok(())
	}

	pub fn session_close(&mut self, id: Id) -> Result<(), Error> {
		self.session_task.remove(id).ok_or(Error::NotFound)?;
		Ok(())
	}

	pub fn origin_create(&mut self) -> Id {
		self.origin.insert(moq_lite::OriginProducer::default())
	}

	pub fn origin_announced(&mut self, origin: Id, mut on_announce: OnStatus) -> Result<Id, Error> {
		let origin = self.origin.get_mut(origin).ok_or(Error::NotFound)?;
		let consumer = origin.consume();
		let channel = oneshot::channel();

		tokio::spawn(async move {
			let res = tokio::select! {
				res = Self::run_origin_announced(consumer, &mut on_announce) => res,
				_ = channel.1 => Ok(()),
			};
			on_announce.call(res);
		});

		let id = self.announced_task.insert(channel.0);
		Ok(id)
	}

	async fn run_origin_announced(
		mut consumer: moq_lite::OriginConsumer,
		on_announce: &mut OnStatus,
	) -> Result<(), Error> {
		while let Some((path, broadcast)) = consumer.announced().await {
			let id = STATE
				.lock()
				.unwrap()
				.announced
				.insert((path.to_string(), broadcast.is_some()));

			// Important: Don't hold the mutex during this callback.
			on_announce.call(id);
		}

		Ok(())
	}

	pub fn origin_announced_info(&self, announced: Id, dst: &mut Announced) -> Result<(), Error> {
		let announced = self.announced.get(announced).ok_or(Error::NotFound)?;
		*dst = Announced {
			path: announced.0.as_str().as_ptr() as *const c_char,
			path_len: announced.0.len(),
			active: announced.1,
		};
		Ok(())
	}

	pub fn origin_announced_close(&mut self, announced: Id) -> Result<(), Error> {
		self.announced_task.remove(announced).ok_or(Error::NotFound)?;
		Ok(())
	}

	pub fn origin_consume<P: moq_lite::AsPath>(&mut self, origin: Id, path: P) -> Result<Id, Error> {
		let origin = self.origin.get_mut(origin).ok_or(Error::NotFound)?;
		let broadcast = origin.consume().consume_broadcast(path).ok_or(Error::NotFound)?;

		let id = self.consume_broadcast.insert(broadcast.into());
		Ok(id)
	}

	pub fn origin_publish<P: moq_lite::AsPath>(&mut self, origin: Id, path: P, publish: Id) -> Result<(), Error> {
		let origin = self.origin.get_mut(origin).ok_or(Error::NotFound)?;
		let publish = self.publish_broadcast.get(publish).ok_or(Error::NotFound)?.clone();
		origin.publish_broadcast(path, publish.consume());
		Ok(())
	}

	pub fn origin_close(&mut self, origin: Id) -> Result<(), Error> {
		self.origin.remove(origin).ok_or(Error::NotFound)?;
		Ok(())
	}

	pub fn publish_create(&mut self) -> Result<Id, Error> {
		let broadcast = hang::BroadcastProducer::default();
		let id = self.publish_broadcast.insert(broadcast);
		Ok(id)
	}

	pub fn publish_close(&mut self, publish: Id) -> Result<(), Error> {
		self.publish_broadcast.remove(publish).ok_or(Error::NotFound)?;
		Ok(())
	}

	pub fn publish_media_init(&mut self, publish: Id, format: &str, init: &[u8]) -> Result<Id, Error> {
		let publish = self.publish_broadcast.get(publish).ok_or(Error::NotFound)?;
		let mut decoder = hang::import::Decoder::new(publish.clone(), format)
			.ok_or_else(|| Error::UnknownFormat(format.to_string()))?;

		let mut temp = init;
		decoder
			.initialize(&mut temp)
			.map_err(|err| Error::InitFailed(Arc::new(err)))?;
		assert!(init.is_empty(), "buffer was not fully consumed");

		let id = self.publish_media.insert(decoder);
		Ok(id)
	}

	pub fn publish_media_frame(&mut self, media: Id, frame: Frame) -> Result<(), Error> {
		let media = self.publish_media.get_mut(media).ok_or(Error::NotFound)?;

		let mut data = unsafe { ffi::parse_slice(frame.payload, frame.payload_size) }?;

		let timestamp = hang::Timestamp::from_micros(frame.timestamp_us)?;
		media
			.decode_frame(&mut data, Some(timestamp))
			.map_err(|err| Error::DecodeFailed(Arc::new(err)))?;
		assert!(data.is_empty(), "buffer was not fully consumed");

		Ok(())
	}

	pub fn publish_media_close(&mut self, media: Id) -> Result<(), Error> {
		self.publish_media.remove(media).ok_or(Error::NotFound)?;
		Ok(())
	}

	pub fn consume_catalog(&mut self, broadcast: Id, mut on_catalog: OnStatus) -> Result<Id, Error> {
		let broadcast = self.consume_broadcast.get(broadcast).ok_or(Error::NotFound)?.clone();

		let channel = oneshot::channel();

		tokio::spawn(async move {
			let res = tokio::select! {
				res = Self::run_consume_catalog(broadcast, &mut on_catalog) => res,
				_ = channel.1 => Ok(()),
			};
			on_catalog.call(res);
		});

		let id = self.consume_catalog_task.insert(channel.0);

		Ok(id)
	}

	async fn run_consume_catalog(
		mut broadcast: hang::BroadcastConsumer,
		on_catalog: &mut OnStatus,
	) -> Result<(), Error> {
		while let Some(catalog) = broadcast.catalog.next().await? {
			let id = STATE
				.lock()
				.unwrap()
				.consume_catalog
				.insert((catalog.clone(), broadcast.inner.clone()));

			// Important: Don't hold the mutex during this callback.
			on_catalog.call(Ok(id));
		}

		Ok(())
	}

	pub fn consume_catalog_video(&mut self, catalog: Id, index: usize, dst: &mut VideoTrack) -> Result<(), Error> {
		let video = self
			.consume_catalog
			.get(catalog)
			.ok_or(Error::NotFound)?
			.0
			.video
			.as_ref()
			.ok_or(Error::NoIndex)?;
		let (rendition, config) = video.renditions.iter().nth(index).ok_or(Error::NoIndex)?;

		let codec = config.codec.to_string();

		*dst = VideoTrack {
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
			coded_width: config
				.coded_width
				.as_ref()
				.map(|width| width as *const u32)
				.unwrap_or(std::ptr::null()),
			coded_height: config
				.coded_height
				.as_ref()
				.map(|height| height as *const u32)
				.unwrap_or(std::ptr::null()),
		};

		// Store it on the heap so we can return it to the caller.
		self.consume_catalog_video.insert((catalog, index), codec);

		Ok(())
	}

	pub fn consume_catalog_video_close(&mut self, catalog: Id, index: usize) -> Result<(), Error> {
		self.consume_catalog_video
			.remove(&(catalog, index))
			.ok_or(Error::NotFound)?;

		Ok(())
	}

	pub fn consume_catalog_audio(&mut self, catalog: Id, index: usize, dst: &mut AudioTrack) -> Result<(), Error> {
		let audio = self
			.consume_catalog
			.get(catalog)
			.ok_or(Error::NotFound)?
			.0
			.audio
			.as_ref()
			.ok_or(Error::NoIndex)?;
		let (rendition, config) = audio.renditions.iter().nth(index).ok_or(Error::NoIndex)?.clone();

		let codec = config.codec.to_string();

		*dst = AudioTrack {
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
		};

		// Store it on the heap so we can return it to the caller.
		self.consume_catalog_audio.insert((catalog, index), codec);

		Ok(())
	}

	pub fn consume_catalog_audio_close(&mut self, catalog: Id, index: usize) -> Result<(), Error> {
		self.consume_catalog_audio
			.remove(&(catalog, index))
			.ok_or(Error::NotFound)?;

		Ok(())
	}

	pub fn consume_catalog_close(&mut self, catalog: Id) -> Result<(), Error> {
		self.consume_catalog.remove(catalog).ok_or(Error::NotFound)?;
		Ok(())
	}

	pub fn consume_video_track(
		&mut self,
		catalog: Id,
		index: usize,
		latency: std::time::Duration,
		mut on_frame: OnStatus,
	) -> Result<Id, Error> {
		let (catalog, broadcast) = self.consume_catalog.get(catalog).ok_or(Error::NotFound)?;
		let video = catalog.video.as_ref().ok_or(Error::NotFound)?;
		let rendition = video.renditions.keys().nth(index).ok_or(Error::NotFound)?;

		let track = broadcast.subscribe_track(&moq_lite::Track {
			name: rendition.clone(),
			priority: video.priority,
		});
		let track = TrackConsumer::new(track, latency);

		let channel = oneshot::channel();
		tokio::spawn(async move {
			let res = tokio::select! {
				res = Self::run_consume_track(track, &mut on_frame) => res,
				_ = channel.1 => Ok(()),
			};
			on_frame.call(res);
		});

		let id = self.consume_video_task.insert(channel.0);

		Ok(id)
	}

	pub fn consume_audio_track(
		&mut self,
		catalog: Id,
		index: usize,
		latency: std::time::Duration,
		mut on_frame: OnStatus,
	) -> Result<Id, Error> {
		let (catalog, broadcast) = self.consume_catalog.get(catalog).ok_or(Error::NotFound)?;
		let audio = catalog.audio.as_ref().ok_or(Error::NotFound)?;
		let rendition = audio.renditions.keys().nth(index).ok_or(Error::NotFound)?;

		let track = broadcast.subscribe_track(&moq_lite::Track {
			name: rendition.clone(),
			priority: audio.priority,
		});
		let track = TrackConsumer::new(track, latency);

		let channel = oneshot::channel();
		tokio::spawn(async move {
			let res = tokio::select! {
				res = Self::run_consume_track(track, &mut on_frame) => res,
				_ = channel.1 => Ok(()),
			};
			on_frame.call(res);
		});

		let id = self.consume_audio_task.insert(channel.0);

		Ok(id)
	}

	async fn run_consume_track(mut track: TrackConsumer, on_frame: &mut OnStatus) -> Result<(), Error> {
		while let Some(mut frame) = track.read_frame().await? {
			// TODO add a chunking API so we don't have to (potentially) allocate a contiguous buffer for the frame.
			let mut new_payload = hang::BufList::new();
			new_payload.push_chunk(if frame.payload.num_chunks() == 1 {
				// We can avoid allocating
				frame.payload.get_chunk(0).expect("frame has zero chunks").clone()
			} else {
				// We need to allocate
				frame.payload.copy_to_bytes(frame.payload.num_bytes())
			});

			let new_frame = hang::Frame {
				payload: new_payload,
				timestamp: frame.timestamp,
				keyframe: frame.keyframe,
			};

			// Important: Don't hold the mutex during this callback.
			let id = STATE.lock().unwrap().consume_frame.insert(new_frame);
			on_frame.call(Ok(id));
		}

		Ok(())
	}

	pub fn consume_audio_track_close(&mut self, track: Id) -> Result<(), Error> {
		self.consume_audio_task.remove(track).ok_or(Error::NotFound)?;
		Ok(())
	}

	pub fn consume_video_track_close(&mut self, track: Id) -> Result<(), Error> {
		self.consume_video_task.remove(track).ok_or(Error::NotFound)?;
		Ok(())
	}

	// NOTE: You're supposed to call this multiple times to get all of the chunks.
	pub fn consume_frame_chunk(&self, frame: Id, index: usize, dst: &mut Frame) -> Result<(), Error> {
		let frame = self.consume_frame.get(frame).ok_or(Error::NotFound)?;
		let chunk = frame.payload.get_chunk(index).ok_or(Error::NoIndex)?;

		*dst = Frame {
			payload: chunk.as_ptr(),
			payload_size: chunk.len(),
			timestamp_us: frame.timestamp.as_micros(),
			keyframe: frame.keyframe,
		};

		Ok(())
	}

	pub fn consume_frame_close(&mut self, frame: Id) -> Result<(), Error> {
		self.consume_frame.remove(frame).ok_or(Error::NotFound)?;
		Ok(())
	}

	pub fn consume_close(&mut self, consume: Id) -> Result<(), Error> {
		self.consume_broadcast.remove(consume).ok_or(Error::NotFound)?;
		Ok(())
	}
}

/// Guard that holds the global state lock and tokio runtime context.
///
/// Automatically enters the tokio runtime context when locked, allowing
/// spawning of async tasks from FFI functions.
pub struct StateGuard {
	_runtime: tokio::runtime::EnterGuard<'static>,
	state: MutexGuard<'static, State>,
}

impl Deref for StateGuard {
	type Target = State;
	fn deref(&self) -> &Self::Target {
		&self.state
	}
}

impl DerefMut for StateGuard {
	fn deref_mut(&mut self) -> &mut Self::Target {
		&mut self.state
	}
}

impl State {
	/// Lock the global state and enter the tokio runtime context.
	pub fn lock() -> StateGuard {
		let runtime = RUNTIME.enter();
		let state = STATE.lock().unwrap();
		StateGuard {
			_runtime: runtime,
			state,
		}
	}
}

/// Global tokio runtime handle.
///
/// Runs in a dedicated background thread to process async operations
/// spawned from FFI calls.
static RUNTIME: LazyLock<tokio::runtime::Handle> = LazyLock::new(|| {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap();
	let handle = runtime.handle().clone();

	std::thread::Builder::new()
		.name("libmoq".into())
		.spawn(move || {
			runtime.block_on(std::future::pending::<()>());
		})
		.expect("failed to spawn runtime thread");

	handle
});

/// Global shared state instance.
static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));
