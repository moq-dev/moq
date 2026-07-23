use std::sync::{LazyLock, Mutex, MutexGuard};

use crate::{Consume, Origin, Publish, Session, audio::Audio, video::Video};

pub struct State {
	pub session: Session,
	pub origin: Origin,
	pub publish: Publish,
	pub consume: Consume,
	pub audio: Audio,
	#[cfg_attr(not(feature = "hw-video"), allow(dead_code))]
	pub video: Video,
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
		}
	}

	pub fn lock<'a>() -> MutexGuard<'a, Self> {
		STATE.lock().unwrap()
	}
}

/// Global shared state instance.
static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::new()));
