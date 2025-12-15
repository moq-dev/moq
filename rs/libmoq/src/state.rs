use std::ops::{Deref, DerefMut};
use std::str::FromStr;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use tokio::sync::oneshot;
use url::Url;

use crate::{ffi, Error, Id, NonZeroSlab};

struct Session {
	// The collection of published broadcasts.
	origin: moq_lite::OriginProducer,

	// A simple signal to notify the background task when closed.
	#[allow(dead_code)]
	closed: oneshot::Sender<()>,
}

pub struct State {
	// All sessions by ID.
	sessions: NonZeroSlab<Session>, // TODO clean these up on error.

	// All broadcasts, indexed by an ID.
	broadcasts: NonZeroSlab<hang::BroadcastProducer>,

	// All tracks, indexed by an ID.
	tracks: NonZeroSlab<hang::import::Decoder>,
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

impl State {
	fn new() -> Self {
		Self {
			sessions: Default::default(),
			broadcasts: Default::default(),
			tracks: Default::default(),
		}
	}

	pub fn session_connect(&mut self, url: Url, mut callback: ffi::Callback) -> Result<Id, Error> {
		let origin = moq_lite::Origin::produce();

		// Used just to notify when the session is removed from the map.
		let closed = oneshot::channel();

		let id = self.sessions.insert(Session {
			closed: closed.0,
			origin: origin.producer,
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
		// TODO add support for stream decoders too.
		let format =
			hang::import::DecoderFormat::from_str(format).map_err(|err| Error::UnknownFormat(err.to_string()))?;
		let mut decoder = hang::import::Decoder::new(broadcast.clone(), format);

		decoder
			.initialize(&mut init)
			.map_err(|err| Error::InitFailed(Arc::new(err)))?;
		assert!(init.is_empty(), "buffer was not fully consumed");

		Ok(self.tracks.insert(decoder))
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
}
