use std::str::FromStr;

use crate::error::MoqError;

/// Initialize logging with a level string: "error", "warn", "info", "debug", "trace", or "".
///
/// Returns an error if called more than once.
#[uniffi::export]
pub fn moq_log_level(level: String) -> Result<(), MoqError> {
	use std::sync::atomic::{AtomicBool, Ordering};
	use tracing::Level;

	static INITIALIZED: AtomicBool = AtomicBool::new(false);

	let log = match level.as_str() {
		"" => moq_native::Log::default(),
		s => moq_native::Log::new(Level::from_str(s)?),
	};

	if INITIALIZED.swap(true, Ordering::SeqCst) {
		return Err(MoqError::Log("logging already initialized".into()));
	}

	log.init();

	Ok(())
}
