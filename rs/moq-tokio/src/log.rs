use serde::{Deserialize, Serialize};
use serde_with::DisplayFromStr;
use tracing::Level;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use url::Url;

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

/// A URL rendered without its credentials, for logging.
///
/// A relay URL routinely carries an auth token in its query (`?jwt=...`), and any
/// URL may carry HTTP userinfo (`https://user:pass@host/`). [`Display`] prints only
/// the scheme, host, port, and path, so wrap every URL headed for a log line.
///
/// ```
/// # use moq_tokio::RedactedUrl;
/// let url = url::Url::parse("https://user:pass@relay.example.com/anon/demo?jwt=secret").unwrap();
/// assert_eq!(RedactedUrl::new(&url).to_string(), "https://relay.example.com/anon/demo");
/// ```
///
/// [`Display`]: std::fmt::Display
#[derive(Clone, Copy)]
pub struct RedactedUrl<'a>(&'a Url);

impl<'a> RedactedUrl<'a> {
	/// Borrow `url` for redacted display.
	pub fn new(url: &'a Url) -> Self {
		Self(url)
	}
}

/// Delegates to [`Display`](std::fmt::Display) so `?redacted` in a log macro can't
/// undo the redaction a derived impl would have printed straight through.
impl std::fmt::Debug for RedactedUrl<'_> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{self}")
	}
}

impl std::fmt::Display for RedactedUrl<'_> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}://", self.0.scheme())?;

		// `host_str` brackets an IPv6 literal and `port` is `None` for the scheme's
		// default, so both render the way `Url`'s own `Display` would.
		if let Some(host) = self.0.host_str() {
			f.write_str(host)?;
			if let Some(port) = self.0.port() {
				write!(f, ":{port}")?;
			}
		}

		f.write_str(self.0.path())
	}
}

#[cfg(test)]
mod tests {
	use super::RedactedUrl;
	use url::Url;

	fn redact(url: &str) -> String {
		RedactedUrl::new(&Url::parse(url).unwrap()).to_string()
	}

	#[test]
	fn drops_query_and_userinfo() {
		let rendered = redact("https://user:pass@relay.example.com/anon/demo?jwt=secret#frag");
		assert_eq!(rendered, "https://relay.example.com/anon/demo");
		for secret in ["jwt", "secret", "user", "pass", "frag"] {
			assert!(!rendered.contains(secret), "{rendered} leaked {secret}");
		}
	}

	#[test]
	fn debug_matches_display() {
		let url = Url::parse("https://user:pass@relay.example.com/anon/demo?jwt=secret").unwrap();
		let redacted = RedactedUrl::new(&url);
		assert_eq!(format!("{redacted:?}"), redacted.to_string());
	}

	#[test]
	fn keeps_the_dial_target() {
		assert_eq!(redact("https://relay.example.com/"), "https://relay.example.com/");
		assert_eq!(
			redact("tcp://relay.example.com:4443/anon"),
			"tcp://relay.example.com:4443/anon"
		);
		assert_eq!(redact("https://[::1]:8443/anon"), "https://[::1]:8443/anon");
		assert_eq!(redact("unix:///run/moq/internal.sock"), "unix:///run/moq/internal.sock");
	}
}
