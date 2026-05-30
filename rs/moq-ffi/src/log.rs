use std::str::FromStr;

use crate::error::MoqError;
use tracing::Level;

/// Initialize logging with a level string: "error", "warn", "info", "debug", "trace", or "".
///
/// Returns an error if called more than once.
#[uniffi::export]
pub fn moq_log_level(level: String) -> Result<(), MoqError> {
	use std::sync::atomic::{AtomicBool, Ordering};

	static INITIALIZED: AtomicBool = AtomicBool::new(false);

	let level = match level.as_str() {
		"" => Level::INFO,
		s => Level::from_str(s)?,
	};

	if INITIALIZED.swap(true, Ordering::SeqCst) {
		return Err(MoqError::Log("logging already initialized".into()));
	}

	moq_native::Log::new(level)
		.init()
		.inspect_err(|_| INITIALIZED.store(false, Ordering::SeqCst))
		.map_err(|err| MoqError::Log(err.to_string()))?;

	Ok(())
}
