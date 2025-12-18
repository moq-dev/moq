use std::sync::{LazyLock, Mutex, MutexGuard};

use crate::{Consume, Origin, Publish, Session};

pub struct State {
	pub session: Session,
	pub origin: Origin,
	pub publish: Publish,
	pub consume: Consume,
}

impl State {
	pub fn new() -> Self {
		Self {
			session: Session::default(),
			origin: Origin::default(),
			publish: Publish::default(),
			consume: Consume::default(),
		}
	}

	pub fn lock<'a>() -> MutexGuard<'a, Self> {
		STATE.lock().unwrap()
	}
}

/// Global shared state instance.
static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::new()));
