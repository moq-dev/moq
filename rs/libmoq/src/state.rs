use std::sync::{LazyLock, Mutex, MutexGuard};

use crate::{Consume, Origin, Publish, Session, audio::Audio};

pub struct State {
	pub session: Session,
	pub origin: Origin,
	pub publish: Publish,
	pub consume: Consume,
	pub audio: Audio,
	/// Disable TLS cert verification for future sessions (set via moq_tls_disable_verify).
	pub tls_disable_verify: bool,
	/// Exponential backoff applied to session reconnects (set via moq_backoff_config).
	pub backoff: moq_native::Backoff,
}

impl State {
	pub fn new() -> Self {
		Self {
			session: Session::default(),
			origin: Origin::default(),
			publish: Publish::default(),
			consume: Consume::default(),
			audio: Audio::default(),
			tls_disable_verify: false,
			backoff: moq_native::Backoff::default(),
		}
	}

	pub fn lock<'a>() -> MutexGuard<'a, Self> {
		STATE.lock().unwrap()
	}
}

/// Global shared state instance.
static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::new()));
