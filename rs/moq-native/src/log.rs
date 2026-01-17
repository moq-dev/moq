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
#[derive(Clone, clap::Parser, Serialize, Deserialize, Debug)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct Log {
	/// The level filter to use.
	#[serde_as(as = "DisplayFromStr")]
	#[arg(id = "log-level", long = "log-level", default_value = "info", env = "MOQ_LOG_LEVEL")]
	pub level: Level,
}

impl Default for Log {
	fn default() -> Self {
		Self { level: Level::INFO }
	}
}

impl Log {
	pub fn new(level: Level) -> Self {
		Self { level }
	}

	pub fn level(&self) -> LevelFilter {
		LevelFilter::from_level(self.level)
	}

	pub fn init(&self) {
		let filter = EnvFilter::builder()
			.with_default_directive(self.level().into()) // Default to our -q/-v args
			.from_env_lossy() // Allow overriding with RUST_LOG
			.add_directive("h2=warn".parse().unwrap())
			.add_directive("quinn=info".parse().unwrap())
			.add_directive("tracing::span=off".parse().unwrap())
			.add_directive("tracing::span::active=off".parse().unwrap())
			.add_directive("tokio=info".parse().unwrap())
			.add_directive("runtime=info".parse().unwrap());

		let fmt_layer = tracing_subscriber::fmt::layer()
			.with_writer(std::io::stderr)
			.with_filter(filter);

		#[cfg(feature = "tokio-console")]
		{
			let console_layer = console_subscriber::spawn();
			tracing_subscriber::registry()
				.with(fmt_layer)
				.with(console_layer)
				.init();
		}

		#[cfg(not(feature = "tokio-console"))]
		{
			tracing_subscriber::registry().with(fmt_layer).init();
		}

		// Start deadlock detection thread (only in debug builds)
		#[cfg(debug_assertions)]
		std::thread::spawn(Self::deadlock_detector);
	}

	#[cfg(debug_assertions)]
	fn deadlock_detector() {
		loop {
			std::thread::sleep(std::time::Duration::from_secs(1));

			let deadlocks = parking_lot::deadlock::check_deadlock();
			if deadlocks.is_empty() {
				continue;
			}

			tracing::error!("DEADLOCK DETECTED");

			for (i, threads) in deadlocks.iter().enumerate() {
				tracing::error!("Deadlock #{}", i);
				for t in threads {
					tracing::error!("Thread Id {:#?}", t.thread_id());
					tracing::error!("{:#?}", t.backtrace());
				}
			}

			// Optionally: std::process::abort() to get a core dump
		}
	}
}
