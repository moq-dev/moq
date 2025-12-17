use std::sync::{LazyLock, Mutex};

use crate::{
	ffi::{self, ReturnCode},
	Consume, Error, Origin, Publish, Session,
};

pub struct State {
	pub runtime: tokio::runtime::Handle,
	pub session: Session,
	pub origin: Origin,
	pub publish: Publish,
	pub consume: Consume,
}

impl State {
	pub fn new() -> Self {
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

		Self {
			runtime: handle,
			session: Session::default(),
			origin: Origin::default(),
			publish: Publish::default(),
			consume: Consume::default(),
		}
	}

	/// Runs the provided function while holding the global state and runtime lock.
	/// Additionally, we convert the return code to a C-compatible return value.
	pub fn enter<C: ffi::ReturnCode, F: FnOnce(&mut Self) -> C>(f: F) -> i32 {
		STATE.lock().unwrap().run(f)
	}

	fn run<C: ffi::ReturnCode, F: FnOnce(&mut Self) -> C>(&mut self, f: F) -> i32 {
		let _guard = self.runtime.enter();
		match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self))) {
			Ok(ret) => ret.code(),
			Err(_) => Error::Panic.code(),
		}
	}
}

/// Global shared state instance.
static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::new()));
