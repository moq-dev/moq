use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use crate::{Consume, Origin, Publish, Session, audio::Audio, video::Video};

pub struct State {
	pub session: Session,
	pub origin: Origin,
	pub publish: Publish,
	pub consume: Consume,
	pub audio: Audio,
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

/// A resource a C call works on with the global [`State`] lock released.
///
/// Some calls block for a long time: encoding a frame is a round trip to the
/// codec thread, and a wedged codec never comes back. Doing that under the
/// process-wide `State` mutex parks every unrelated session, callback and
/// shutdown behind it, so those entry points resolve the handle, drop the global
/// guard, and work behind the resource's own lock instead. That lock is what
/// keeps writes to one resource ordered, without ordering them against the rest
/// of the process.
///
/// Locking order is `State` then this, never the reverse.
pub struct Shared<T>(Arc<Mutex<Option<T>>>);

impl<T> Shared<T> {
	pub fn new(value: T) -> Self {
		Self(Arc::new(Mutex::new(Some(value))))
	}

	/// The resource, locked for as long as the guard is held.
	///
	/// `None` once a terminal call has taken it. Its id is dropped from the slab
	/// under the global lock at the same moment, so that is only what a call which
	/// resolved the handle just before the end sees.
	pub fn lock(&self) -> MutexGuard<'_, Option<T>> {
		self.0.lock().unwrap()
	}

	/// Take the resource, for the call that ends it and needs to consume it.
	pub fn take(&self) -> Option<T> {
		self.lock().take()
	}

	/// How many callers hold this handle right now: the slab plus whoever has
	/// resolved it. Lets a test tell that a blocking call has made it past the
	/// global lock and is inside the resource's own.
	#[cfg(test)]
	pub fn holders(&self) -> usize {
		Arc::strong_count(&self.0)
	}
}

impl<T> Clone for Shared<T> {
	fn clone(&self) -> Self {
		Self(self.0.clone())
	}
}
