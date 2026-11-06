//! Run a test under an environment of its own, without writing the process environment.
//!
//! `cargo test` runs every test in a binary as a thread of one process. Writing the
//! environment there (`std::env::set_var`) while any other thread reads it is undefined
//! behavior on glibc, and a lock only orders the tests that take it, not a `getenv` inside a
//! library. So a test that needs a variable set or absent runs again in a child process
//! started with that environment.
//!
//! ```ignore
//! #[test]
//! fn reads_the_variable() {
//!     if !test_env::Env::default().set("MOQ_CONNECT", "https://relay").isolate() {
//!         return;
//!     }
//!     // Runs with `MOQ_CONNECT` set.
//! }
//! ```

use std::ffi::{OsStr, OsString};
use std::process::Command;

/// Names the test a child process was started to run.
const CHILD: &str = "TEST_ENV_CHILD";

/// The environment a test needs: some variables set to a value, others absent.
///
/// The default leaves every variable as the process has it.
#[derive(Default)]
pub struct Env {
	vars: Vec<(String, Option<OsString>)>,
}

impl Env {
	/// An environment where none of `names` is set.
	pub fn without(names: &[&str]) -> Self {
		let vars = names.iter().map(|&name| (name.to_owned(), None)).collect();
		Self { vars }
	}

	/// The same environment with `name` set to `value`, even if it was to be absent.
	pub fn set(mut self, name: &str, value: impl AsRef<OsStr>) -> Self {
		self.vars.retain(|(other, _)| other != name);
		self.vars.push((name.to_owned(), Some(value.as_ref().to_owned())));
		self
	}

	/// Whether the calling test runs its body in this process.
	///
	/// True when this process already has the environment, or is the child started to run the
	/// test. Otherwise runs the test in a child given the environment, panics when it fails, and
	/// returns false: the test already ran, so the caller returns.
	///
	/// Call it from the test's own thread, which libtest names after the test. A child sees the
	/// values set here, so a test that picks one per run (a temporary path) reads it back from the
	/// environment rather than picking again.
	pub fn isolate(self) -> bool {
		let test = std::thread::current()
			.name()
			.expect("isolate runs on the test's own thread")
			.to_owned();
		if std::env::var_os(CHILD).is_some_and(|child| child == *test) {
			return true;
		}
		if self.vars.iter().all(|(name, value)| std::env::var_os(name) == *value) {
			return true;
		}

		let mut command = Command::new(std::env::current_exe().expect("the test binary"));
		command
			.args([&test, "--exact", "--include-ignored", "--test-threads=1"])
			.env(CHILD, &test);
		for (name, value) in &self.vars {
			match value {
				Some(value) => command.env(name, value),
				None => command.env_remove(name),
			};
		}

		let output = command.output().expect("failed to start the test binary");
		let stdout = String::from_utf8_lossy(&output.stdout);
		// Through the harness's capture, so a failure shows the child's output too.
		print!("{stdout}");
		eprint!("{}", String::from_utf8_lossy(&output.stderr));
		assert!(output.status.success(), "{test} failed in its child: {}", output.status);
		// `--exact` matching nothing exits zero, which would pass a test that never ran.
		assert!(stdout.contains("running 1 test"), "the child ran no test named {test}");
		false
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_set_variable_reaches_the_child() {
		if !Env::default().set("TEST_ENV_PROBE", "set").isolate() {
			return;
		}
		assert_eq!(std::env::var_os("TEST_ENV_PROBE").as_deref(), Some(OsStr::new("set")));
	}

	#[test]
	fn an_environment_already_in_place_runs_here() {
		assert!(Env::without(&["TEST_ENV_NEVER_SET"]).isolate());
	}
}
