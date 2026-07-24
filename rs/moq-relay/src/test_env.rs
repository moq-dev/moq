//! Serializes the tests that touch the process environment.

use std::ffi::OsString;
use std::sync::{Mutex, MutexGuard};

/// One lock for every test that reads or writes the process environment.
///
/// `env::set_var` / `remove_var` are not thread-safe against a concurrent read,
/// which is why they're `unsafe`, and clap reads the `MOQ_*` vars while it
/// parses. Per-variable-group locks aren't enough: they let one group clear its
/// vars while another group is inside `Config::parse_and_merge` reading the
/// environment. A single lock is what actually makes that pairing safe, and
/// these tests are fast enough that serializing them costs nothing.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Holds [`ENV_LOCK`] for as long as a test needs the environment to itself,
/// restoring anything it cleared on drop.
///
/// Tests clear the `MOQ_*` vars so a value in the developer's environment can't
/// make an assertion pass for the wrong reason. Restoring on drop keeps the rest
/// of the test process seeing the environment it started with.
pub(crate) struct EnvGuard {
	_lock: MutexGuard<'static, ()>,
	saved: Vec<(&'static str, Option<OsString>)>,
}

impl EnvGuard {
	/// Takes the lock without touching a variable, for a test that only reads
	/// the environment (parsing a config) and must not race a writer.
	pub(crate) fn lock() -> Self {
		Self::clear(&[])
	}

	/// Takes the lock, then removes each variable, remembering what it held.
	pub(crate) fn clear(vars: &[&'static str]) -> Self {
		let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
		let saved = vars
			.iter()
			.map(|&key| {
				let prev = std::env::var_os(key);
				// SAFETY: ENV_LOCK is held, and every test that touches the
				// environment takes it, so nothing else reads or writes here.
				unsafe { std::env::remove_var(key) };
				(key, prev)
			})
			.collect();

		Self { _lock: lock, saved }
	}
}

impl Drop for EnvGuard {
	fn drop(&mut self) {
		// Runs before `_lock` is dropped, so the restore is still serialized.
		for (key, prev) in &self.saved {
			// SAFETY: ENV_LOCK is still held; see `clear`.
			match prev {
				Some(value) => unsafe { std::env::set_var(key, value) },
				None => unsafe { std::env::remove_var(key) },
			}
		}
	}
}
