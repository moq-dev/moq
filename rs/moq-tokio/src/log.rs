use serde::{Deserialize, Serialize};
use serde_with::DisplayFromStr;
use tracing::Level;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Tracing log configuration.
#[serde_with::serde_as]
#[derive(Clone, usage::Args, Serialize, Deserialize, Debug)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct Log {
	/// The level filter to use.
	#[serde_as(as = "DisplayFromStr")]
	#[usage(name = "log-level", long = "log-level", default = "info", env = "MOQ_LOG_LEVEL")]
	pub level: Level,
}

impl Default for Log {
	fn default() -> Self {
		Self { level: Level::INFO }
	}
}

impl Log {
	/// Log at the given level and below.
	pub fn new(level: Level) -> Self {
		Self { level }
	}

	/// The configured level as a filter.
	pub fn level(&self) -> LevelFilter {
		LevelFilter::from_level(self.level)
	}

	/// Install this as the process-wide tracing subscriber.
	///
	/// `RUST_LOG` overrides the configured level. Logs go to stderr, or to
	/// logcat on Android. Errors if a subscriber is already installed, so call
	/// it once at startup.
	pub fn init(&self) -> crate::Result<()> {
		let filter = EnvFilter::builder()
			.with_default_directive(self.level().into()) // Default to our -q/-v args
			.from_env_lossy() // Allow overriding with RUST_LOG
			.add_directive("h2=warn".parse()?)
			.add_directive("quinn=info".parse()?)
			.add_directive("noq=info".parse()?)
			.add_directive("tungstenite=info".parse()?)
			.add_directive("rustls=info".parse()?)
			.add_directive("tracing::span=off".parse()?)
			.add_directive("tracing::span::active=off".parse()?)
			.add_directive("tokio=info".parse()?)
			.add_directive("runtime=info".parse()?);

		let registry = tracing_subscriber::registry();

		// On Android, route logs to logcat so they can be inspected via ADB/Android Studio.
		// Everywhere else, format to stderr.
		#[cfg(all(target_os = "android", feature = "android-logcat"))]
		let registry = {
			let logcat_layer = tracing_android::layer("MoQNative")
				.map_err(|e| crate::Error::Logcat(std::sync::Arc::new(e)))?
				.with_filter(filter);
			registry.with(logcat_layer)
		};

		#[cfg(not(all(target_os = "android", feature = "android-logcat")))]
		let registry = {
			let fmt_layer = tracing_subscriber::fmt::layer()
				.with_writer(std::io::stderr)
				.with_filter(filter);
			registry.with(fmt_layer)
		};

		registry
			.try_init()
			.map_err(|e| crate::Error::SetSubscriber(e.to_string()))?;

		Ok(())
	}
}
