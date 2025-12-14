use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use moq_lite::coding::Buf;
use tokio::sync::oneshot;
use url::Url;

use crate::{ffi, Error, Id, NonZeroSlab};

struct Session {
	// The collection of published broadcasts.
	origin: moq_lite::OriginProducer,

	// The URL this session is connected to.
	url: Url,

	// A simple signal to notify the background task when closed.
	#[allow(dead_code)]
	closed: oneshot::Sender<()>,
}

struct Subscription {
	// A simple signal to notify the background task when closed.
	#[allow(dead_code)]
	closed: oneshot::Sender<()>,
}

pub struct SubscriptionCallbacks {
	pub user_data: *mut std::ffi::c_void,
	pub on_catalog:
		Option<unsafe extern "C" fn(user_data: *mut std::ffi::c_void, catalog_json: *const std::ffi::c_char)>,
	pub on_video: Option<
		unsafe extern "C" fn(
			user_data: *mut std::ffi::c_void,
			track: i32,
			data: *const u8,
			size: usize,
			pts: u64,
			keyframe: bool,
		),
	>,
	pub on_audio: Option<
		unsafe extern "C" fn(user_data: *mut std::ffi::c_void, track: i32, data: *const u8, size: usize, pts: u64),
	>,
	pub on_error: Option<unsafe extern "C" fn(user_data: *mut std::ffi::c_void, code: i32)>,
}

unsafe impl Send for SubscriptionCallbacks {}

pub struct State {
	// All sessions by ID.
	sessions: NonZeroSlab<Session>, // TODO clean these up on error.

	// All broadcasts, indexed by an ID.
	broadcasts: NonZeroSlab<hang::BroadcastProducer>,

	// All tracks, indexed by an ID.
	tracks: NonZeroSlab<hang::import::Decoder>,

	// All subscriptions by ID.
	subscriptions: NonZeroSlab<Subscription>,
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

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::new()));

// Global mapping from track names to stable numeric IDs for multi-track identification
static TRACK_NAME_TO_ID: LazyLock<Mutex<HashMap<String, u32>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

impl State {
	fn new() -> Self {
		Self {
			sessions: Default::default(),
			broadcasts: Default::default(),
			tracks: Default::default(),
			subscriptions: Default::default(),
		}
	}

	pub fn session_connect(&mut self, url: Url, mut callback: ffi::Callback) -> Result<Id, Error> {
		let origin = moq_lite::Origin::produce();

		// Used just to notify when the session is removed from the map.
		let closed = oneshot::channel();

		let id = self.sessions.insert(Session {
			closed: closed.0,
			origin: origin.producer,
			url: url.clone(),
		});

		tokio::spawn(async move {
			let err = tokio::select! {
				// No more receiver, which means [session_close] was called.
				_ = closed.1 => Ok(()),
				// The connection failed.
				res = Self::session_connect_run(url, origin.consumer, &mut callback) => res,
			}
			.err()
			.unwrap_or(Error::Closed);

			callback.call(err);
		});

		Ok(id)
	}

	async fn session_connect_run(
		url: Url,
		origin: moq_lite::OriginConsumer,
		callback: &mut ffi::Callback,
	) -> Result<(), Error> {
		let config = moq_native::ClientConfig::default();
		let client = config.init().map_err(|err| Error::Connect(Arc::new(err)))?;
		let connection = client.connect(url).await.map_err(|err| Error::Connect(Arc::new(err)))?;
		let session = moq_lite::Session::connect(connection, origin, None).await?;
		callback.call(());

		session.closed().await?;
		Ok(())
	}

	pub fn session_close(&mut self, id: Id) -> Result<(), Error> {
		self.sessions.remove(id).ok_or(Error::NotFound)?;
		Ok(())
	}

	pub fn publish_broadcast<P: moq_lite::AsPath>(&mut self, broadcast: Id, session: Id, path: P) -> Result<(), Error> {
		let path = path.as_path();
		let broadcast = self.broadcasts.get_mut(broadcast).ok_or(Error::NotFound)?;
		let session = self.sessions.get_mut(session).ok_or(Error::NotFound)?;

		session.origin.publish_broadcast(path, broadcast.consume());

		Ok(())
	}

	pub fn create_broadcast(&mut self) -> Id {
		let broadcast = moq_lite::Broadcast::produce();
		self.broadcasts.insert(broadcast.producer.into())
	}

	pub fn remove_broadcast(&mut self, broadcast: Id) -> Result<(), Error> {
		self.broadcasts.remove(broadcast).ok_or(Error::NotFound)?;
		Ok(())
	}

	pub fn create_track(&mut self, broadcast: Id, format: &str, mut init: &[u8]) -> Result<Id, Error> {
		let broadcast = self.broadcasts.get_mut(broadcast).ok_or(Error::NotFound)?;
		let mut decoder = hang::import::Decoder::new(broadcast.clone(), format)
			.ok_or_else(|| Error::UnknownFormat(format.to_string()))?;

		decoder
			.initialize(&mut init)
			.map_err(|err| Error::InitFailed(Arc::new(err)))?;
		assert!(init.is_empty(), "buffer was not fully consumed");

		let id = self.tracks.insert(decoder);
		Ok(id)
	}

	pub fn write_track(&mut self, track: Id, mut data: &[u8], pts: u64) -> Result<(), Error> {
		let track = self.tracks.get_mut(track).ok_or(Error::NotFound)?;

		let pts = hang::Timestamp::from_micros(pts)?;
		track
			.decode_frame(&mut data, Some(pts))
			.map_err(|err| Error::DecodeFailed(Arc::new(err)))?;
		assert!(data.is_empty(), "buffer was not fully consumed");

		Ok(())
	}

	pub fn remove_track(&mut self, track: Id) -> Result<(), Error> {
		self.tracks.remove(track).ok_or(Error::NotFound)?;
		Ok(())
	}

	// Get or create a stable numeric ID for a track name
	fn get_or_create_track_id(track_name: &str) -> u32 {
		let mut map = TRACK_NAME_TO_ID.lock().unwrap();
		if let Some(&id) = map.get(track_name) {
			id
		} else {
			// Find the next available ID (starting from 0)
			let next_id = map.len() as u32;
			map.insert(track_name.to_string(), next_id);
			next_id
		}
	}

	pub fn subscribe_from_session(
		&mut self,
		session_id: Id,
		path: String,
		callbacks: SubscriptionCallbacks,
	) -> Result<Id, Error> {
		let session = self.sessions.get_mut(session_id).ok_or(Error::NotFound)?;

		// For now, we'll create a new connection for subscribing since the existing session is used for publishing
		// TODO: Support subscribing on the same session when moq_lite supports it
		let url = session.url.clone();

		// Used just to notify when the subscription is removed from the map.
		let closed = oneshot::channel();

		let id = self.subscriptions.insert(Subscription { closed: closed.0 });

		let _on_error = callbacks.on_error;
		let _user_data = callbacks.user_data;

		// For now, we'll handle errors by logging them since calling callbacks
		// from tokio threads with raw pointers is complex in Rust.
		// TODO: Implement a proper callback mechanism that works with tokio
		tokio::spawn(async move {
			let result = tokio::select! {
				// No more receiver, which means [subscription_close] was called.
				_ = closed.1 => Ok(()),
				// The connection failed.
				res = Self::subscribe_run(url, path, callbacks) => res,
			};

			if let Err(err) = result {
				tracing::error!("Subscription error: {}", err);
				// TODO: Send error to main thread for callback
			}
		});

		Ok(id)
	}

	async fn subscribe_run(url: Url, path: String, callbacks: SubscriptionCallbacks) -> Result<(), Error> {
		let config = moq_native::ClientConfig::default();
		let client = config.init().map_err(|err| Error::Connect(Arc::new(err)))?;
		let connection = client.connect(url).await.map_err(|err| Error::Connect(Arc::new(err)))?;
		let origin = moq_lite::Origin::produce();
		let session = moq_lite::Session::connect(connection, None, Some(origin.producer)).await?;

		tracing::info!(broadcast = %path, "waiting for broadcast to be online");

		let path: moq_lite::Path<'_> = path.into();
		let mut origin = origin.consumer.consume_only(&[path]).ok_or(Error::NotFound)?;

		// Track the current video and audio subscribers
		let mut video_subscribers: std::collections::HashMap<String, hang::TrackConsumer> =
			std::collections::HashMap::new();
		let mut audio_subscribers: std::collections::HashMap<String, hang::TrackConsumer> =
			std::collections::HashMap::new();

		loop {
			tokio::select! {
				Some(announce) = origin.announced() => match announce {
					(path, Some(broadcast)) => {
						tracing::info!(broadcast = %path, "broadcast is online, subscribing to catalog");

						// Subscribe to catalog track
						let catalog_track = broadcast.subscribe_track(&moq_lite::Track {
							name: hang::catalog::Catalog::DEFAULT_NAME.to_string(),
							priority: 100,
						});

						let mut catalog = hang::catalog::CatalogConsumer::new(catalog_track);

						// Wait for initial catalog
						if let Some(catalog_data) = catalog.next().await? {
							let catalog_json = catalog_data.to_string()?;
							if let Some(on_catalog) = callbacks.on_catalog {
								let c_string = std::ffi::CString::new(catalog_json).unwrap();
								unsafe { on_catalog(callbacks.user_data, c_string.as_ptr()) };
							}

							// Subscribe to video tracks
							if let Some(video) = &catalog_data.video {
								for track_name in video.renditions.keys() {
									let track = broadcast.subscribe_track(&moq_lite::Track {
										name: track_name.clone(),
										priority: video.priority,
									});
									video_subscribers.insert(track_name.clone(), hang::TrackConsumer::new(track));
								}
							}

							// Subscribe to audio tracks
							if let Some(audio) = &catalog_data.audio {
								for track_name in audio.renditions.keys() {
									let track = broadcast.subscribe_track(&moq_lite::Track {
										name: track_name.clone(),
										priority: audio.priority,
									});
									audio_subscribers.insert(track_name.clone(), hang::TrackConsumer::new(track));
								}
							}
						}
					}
					(path, None) => {
						tracing::warn!(broadcast = %path, "broadcast is offline, waiting...");
						video_subscribers.clear();
						audio_subscribers.clear();
					}
				},
				res = session.closed() => return res.map_err(Into::into),
				// Handle video frames
				Some((track_name, frame)) = Self::next_video_frame(&mut video_subscribers) => {
					if let Some(on_video) = callbacks.on_video {
						let track_id = Self::get_or_create_track_id(&track_name);
						let pts = frame.timestamp.as_micros();
						let keyframe = frame.keyframe;
						// Collect BufList chunks into a contiguous buffer for FFI
						let data: Vec<u8> = frame.payload.chunk().to_vec();
						let remaining = frame.payload.remaining();
						if data.len() < remaining {
							// Multiple chunks - need to collect all
							let mut full_data = Vec::with_capacity(remaining);
							let mut payload = frame.payload;
							while payload.has_remaining() {
								full_data.extend_from_slice(payload.chunk());
								let len = payload.chunk().len();
								payload.advance(len);
							}
							unsafe { on_video(callbacks.user_data, track_id as i32, full_data.as_ptr(), full_data.len(), pts, keyframe) };
						} else {
							unsafe { on_video(callbacks.user_data, track_id as i32, data.as_ptr(), data.len(), pts, keyframe) };
						}
					}
				},
				// Handle audio frames
				Some((track_name, frame)) = Self::next_audio_frame(&mut audio_subscribers) => {
					if let Some(on_audio) = callbacks.on_audio {
						let track_id = Self::get_or_create_track_id(&track_name);
						let pts = frame.timestamp.as_micros();
						// Collect BufList chunks into a contiguous buffer for FFI
						let data: Vec<u8> = frame.payload.chunk().to_vec();
						let remaining = frame.payload.remaining();
						if data.len() < remaining {
							// Multiple chunks - need to collect all
							let mut full_data = Vec::with_capacity(remaining);
							let mut payload = frame.payload;
							while payload.has_remaining() {
								full_data.extend_from_slice(payload.chunk());
								let len = payload.chunk().len();
								payload.advance(len);
							}
							unsafe { on_audio(callbacks.user_data, track_id as i32, full_data.as_ptr(), full_data.len(), pts) };
						} else {
							unsafe { on_audio(callbacks.user_data, track_id as i32, data.as_ptr(), data.len(), pts) };
						}
					}
				},
			}
		}
	}

	async fn next_video_frame(
		subscribers: &mut std::collections::HashMap<String, hang::TrackConsumer>,
	) -> Option<(String, hang::Frame)> {
		// This is a simplified version - in practice you'd need to handle multiple subscribers
		for (name, subscriber) in subscribers.iter_mut() {
			if let Ok(Some(frame)) = subscriber.read_frame().await {
				return Some((name.clone(), frame));
			}
		}
		None
	}

	async fn next_audio_frame(
		subscribers: &mut std::collections::HashMap<String, hang::TrackConsumer>,
	) -> Option<(String, hang::Frame)> {
		// This is a simplified version - in practice you'd need to handle multiple subscribers
		for (name, subscriber) in subscribers.iter_mut() {
			if let Ok(Some(frame)) = subscriber.read_frame().await {
				return Some((name.clone(), frame));
			}
		}
		None
	}

	pub fn unsubscribe(&mut self, id: Id) -> Result<(), Error> {
		self.subscriptions.remove(id).ok_or(Error::NotFound)?;
		Ok(())
	}
}
