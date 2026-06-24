use std::sync::{LazyLock, Mutex, MutexGuard};

use crate::{Consume, Origin, Publish, Session, audio::Audio, cache::Cache, video::Video};

pub struct State {
	pub session: Session,
	pub origin: Origin,
	pub publish: Publish,
	pub consume: Consume,
	pub audio: Audio,
	pub video: Video,
	pub cache: Cache,
}

impl State {
	pub fn new() -> Self {
		Self {
			session: Session::default(),
			origin: Origin::default(),
			publish: Publish::default(),
			consume: Consume::default(),
			audio: Audio::default(),
			video: Video::default(),
			cache: Cache::default(),
		}
	}

	pub fn lock<'a>() -> MutexGuard<'a, Self> {
		STATE.lock().unwrap()
	}
}

/// Global shared state instance.
static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::new()));
