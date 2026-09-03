//! Serializes the tests that touch the process environment.

use std::ffi::OsString;
use std::sync::{Mutex, MutexGuard};

/// One lock for every test that reads or writes the process environment.
///
/// `set_var`'s safety contract is that no other thread reads the environment while it
/// runs, and this crate reads it all over: Usage consults the `MOQ_*` variables on
/// every parse, which both the argument tests and the completion tests trigger. So the
/// lock is not only for the tests that write. A single lock rather than one per
/// variable, because the hazard is a reader racing a writer, not two writers.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Holds [`ENV_LOCK`] for as long as a test needs the environment to itself, restoring
/// whatever it changed on drop.
///
/// Tests clear the `MOQ_*` variables so a value in the developer's own shell cannot
/// decide whether an assertion passes.
pub(crate) struct EnvGuard {
	_lock: MutexGuard<'static, ()>,
	saved: Vec<(&'static str, Option<OsString>)>,
}

impl EnvGuard {
	/// Take the lock and clear `vars`, for a test that only reads them.
	pub(crate) fn clear(vars: &[&'static str]) -> Self {
		Self::apply(vars.iter().map(|&name| (name, None)))
	}

	/// Take the lock and set each variable, remembering what it held.
	pub(crate) fn set(vars: &[(&'static str, &str)]) -> Self {
		Self::apply(vars.iter().map(|&(name, value)| (name, Some(value))))
	}

	fn apply<'a>(vars: impl Iterator<Item = (&'static str, Option<&'a str>)>) -> Self {
		let lock = ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
		let saved = vars
			.map(|(name, value)| {
				let previous = std::env::var_os(name);
				// SAFETY: ENV_LOCK is held, and every test that reads or writes the
				// environment takes it first.
				unsafe {
					match value {
						Some(value) => std::env::set_var(name, value),
						None => std::env::remove_var(name),
					}
				}
				(name, previous)
			})
			.collect();

		Self { _lock: lock, saved }
	}
}

impl Drop for EnvGuard {
	fn drop(&mut self) {
		// Runs before `_lock`, so the restore is still serialized.
		for (name, previous) in &self.saved {
			// SAFETY: see `apply`.
			unsafe {
				match previous {
					Some(value) => std::env::set_var(name, value),
					None => std::env::remove_var(name),
				}
			}
		}
	}
}
