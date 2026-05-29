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

	init_logging(level)?;

	Ok(())
}

#[cfg(all(target_os = "android", feature = "android-logcat"))]
fn init_logging(level: Level) -> Result<(), MoqError> {
	use tracing::level_filters::LevelFilter;
	use tracing_subscriber::EnvFilter;
	use tracing_subscriber::Layer;
	use tracing_subscriber::layer::SubscriberExt;
	use tracing_subscriber::util::SubscriberInitExt;

	let filter = EnvFilter::builder()
		.with_default_directive(LevelFilter::from_level(level).into())
		.from_env_lossy()
		.add_directive("h2=warn".parse().unwrap())
		.add_directive("quinn=trace".parse().unwrap())
		.add_directive("tungstenite=info".parse().unwrap())
		.add_directive("rustls=info".parse().unwrap())
		.add_directive("tracing::span=off".parse().unwrap())
		.add_directive("tracing::span::active=off".parse().unwrap())
		.add_directive("tokio=info".parse().unwrap())
		.add_directive("runtime=info".parse().unwrap());

	let logcat_layer = tracing_android::layer("MoQNative")
		.map_err(|err| MoqError::Log(format!("failed to initialize Android logcat layer: {err}")))?
		.with_filter(filter);

	tracing_subscriber::registry().with(logcat_layer).init();

	Ok(())
}

#[cfg(not(all(target_os = "android", feature = "android-logcat")))]
fn init_logging(level: Level) -> Result<(), MoqError> {
	moq_native::Log::new(level).init();
	Ok(())
}
