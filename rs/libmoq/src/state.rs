use std::ffi::c_char;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use hang::TrackConsumer;
use moq_lite::coding::Buf;
use tokio::sync::oneshot;
use url::Url;

use crate::ffi::OnStatus;
use crate::{ffi, Announced, AudioTrack, Error, Frame, Id, NonZeroSlab, VideoTrack};
#[derive(Default)]
pub struct State {
	origin: NonZeroSlab<moq_lite::OriginProducer>,

	// Contains a oneshot so we can detect when the session is removed from the slab.
	session_task: NonZeroSlab<oneshot::Sender<()>>,

	announced: NonZeroSlab<(String, bool)>,
	announced_task: NonZeroSlab<oneshot::Sender<()>>,

	publish_broadcast: NonZeroSlab<hang::BroadcastProducer>,
	publish_media: NonZeroSlab<hang::import::Decoder>,

	consume_broadcast: NonZeroSlab<hang::BroadcastConsumer>,
	consume_catalog: NonZeroSlab<hang::catalog::Catalog>,
	consume_catalog_task: NonZeroSlab<oneshot::Sender<()>>,
	consume_audio_task: NonZeroSlab<oneshot::Sender<()>>,
	consume_video_task: NonZeroSlab<oneshot::Sender<()>>,

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
				_ = closed.1 => Ok(()),
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
			let mut state = STATE.lock().unwrap();
			let id = state.announced.insert((path.to_string(), broadcast.is_some()));
			on_announce.call(id);
		}

		Ok(())
	}

	pub fn origin_announced_info(&self, announced: Id, dst: &mut Announced) -> Result<(), Error> {
		let announced = self.announced.get(announced).ok_or(Error::NotFound)?;
		dst.path = announced.0.as_str().as_ptr() as *const c_char;
		dst.path_len = announced.0.len();
		dst.active = announced.1;
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

		let pts = hang::Timestamp::from_micros(frame.pts)?;
		media
			.decode_frame(&mut data, Some(pts))
			.map_err(|err| Error::DecodeFailed(Arc::new(err)))?;
		assert!(data.is_empty(), "buffer was not fully consumed");

		Ok(())
	}

	pub fn publish_media_close(&mut self, media: Id) -> Result<(), Error> {
		self.publish_media.remove(media).ok_or(Error::NotFound)?;
		Ok(())
	}

	pub fn consume_catalog(&mut self, broadcast: Id, mut on_catalog: OnStatus) -> Result<Id, Error> {
		let catalog = self
			.consume_broadcast
			.get(broadcast)
			.ok_or(Error::NotFound)?
			.catalog
			.clone();

		let channel = oneshot::channel();

		tokio::spawn(async move {
			let res = tokio::select! {
				res = Self::run_consume_catalog(catalog, &mut on_catalog) => res,
				_ = channel.1 => Ok(()),
			};
			on_catalog.call(res);
		});

		let id = self.consume_catalog_task.insert(channel.0);

		Ok(id)
	}

	async fn run_consume_catalog(
		mut catalog: hang::catalog::CatalogConsumer,
		on_catalog: &mut OnStatus,
	) -> Result<(), Error> {
		while let Some(catalog) = catalog.next().await? {
			let mut state = STATE.lock().unwrap();
			let id = state.consume_catalog.insert(catalog.clone());
			on_catalog.call(Ok(id));
		}

		Ok(())
	}

	pub fn consume_catalog_video(&mut self, catalog: Id, index: usize, dst: &mut VideoTrack) -> Result<(), Error> {
		let catalog = self.consume_catalog.get(catalog).ok_or(Error::NotFound)?;
		let video = catalog.video.as_ref().ok_or(Error::NoIndex)?;
		let (rendition, config) = video.renditions.iter().nth(index).ok_or(Error::NoIndex)?;

		dst.name = rendition.as_str().as_ptr() as *const c_char;
		dst.name_len = rendition.len();
		dst.codec = rendition.as_str().as_ptr() as *const c_char;
		dst.codec_len = rendition.len();
		dst.description = config.description.as_ref().map(|desc| desc.as_ptr() as *const u8);
		dst.description_len = config.description.as_ref().map(|desc| desc.len()).unwrap_or(0);
		dst.coded_width = config.coded_width;
		dst.coded_height = config.coded_height;

		Ok(())
	}

	pub fn consume_catalog_audio(&mut self, catalog: Id, index: usize, dst: &mut AudioTrack) -> Result<(), Error> {
		let catalog = self.consume_catalog.get(catalog).ok_or(Error::NotFound)?;
		let audio = catalog.audio.as_ref().ok_or(Error::NoIndex)?;
		let (rendition, config) = audio.renditions.iter().nth(index).ok_or(Error::NoIndex)?;

		dst.name = rendition.as_str().as_ptr() as *const c_char;
		dst.name_len = rendition.len();
		dst.codec = rendition.as_str().as_ptr() as *const c_char;
		dst.codec_len = rendition.len();
		dst.description = config.description.as_ref().map(|desc| desc.as_ptr() as *const u8);
		dst.description_len = config.description.as_ref().map(|desc| desc.len()).unwrap_or(0);
		dst.sample_rate = config.sample_rate;
		dst.channel_count = config.channel_count;

		Ok(())
	}

	pub fn consume_catalog_close(&mut self, catalog: Id) -> Result<(), Error> {
		self.consume_catalog.remove(catalog).ok_or(Error::NotFound)?;
		Ok(())
	}

	pub fn consume_video_track(
		&mut self,
		broadcast: Id,
		index: usize,
		latency: std::time::Duration,
		mut on_frame: OnStatus,
	) -> Result<Id, Error> {
		let broadcast = self.consume_broadcast.get(broadcast).ok_or(Error::NotFound)?;
		let catalog = broadcast.catalog.current().ok_or(Error::Offline)?;
		let video = catalog.video.as_ref().ok_or(Error::NotFound)?;
		let rendition = video.renditions.keys().nth(index).ok_or(Error::NotFound)?;

		let mut track = broadcast.subscribe(&moq_lite::Track {
			name: rendition.clone(),
			priority: video.priority,
		});
		track.set_latency(latency);

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
		broadcast: Id,
		index: usize,
		latency: std::time::Duration,
		mut on_frame: OnStatus,
	) -> Result<Id, Error> {
		let broadcast = self.consume_broadcast.get(broadcast).ok_or(Error::NotFound)?;
		let catalog = broadcast.catalog.current().ok_or(Error::Offline)?;
		let video = catalog.video.as_ref().ok_or(Error::NotFound)?;
		let rendition = video.renditions.keys().nth(index).ok_or(Error::NotFound)?;

		let mut track = broadcast.subscribe(&moq_lite::Track {
			name: rendition.clone(),
			priority: video.priority,
		});
		track.set_latency(latency);

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

	async fn run_consume_track(mut track: TrackConsumer, on_frame: &mut OnStatus) -> Result<(), Error> {
		while let Some(mut frame) = track.read_frame().await? {
			let mut state = STATE.lock().unwrap();

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

			let id = state.consume_frame.insert(new_frame);
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

		dst.payload = chunk.as_ptr();
		dst.payload_size = chunk.len();
		dst.pts = frame.timestamp.as_micros();
		dst.keyframe = frame.keyframe;

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
	pub fn lock() -> StateGuard {
		let runtime = RUNTIME.enter();
		let state = STATE.lock().unwrap();
		StateGuard {
			_runtime: runtime,
			state,
		}
	}
}

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

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));
