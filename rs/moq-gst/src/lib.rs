//! GStreamer plugin with a `moqsink` element that publishes a pipeline to a MoQ relay
//! and a `moqsrc` element that subscribes to a broadcast, one source pad per rendition.

use gst::glib;

mod block;
mod connection;
mod sink;
mod source;

use block::block_on;

/// The `moqsink` publish connection lifecycle, exposed as its read-only `status` property.
pub use sink::ConnectionStatus;
/// The media container selected by the `moqsink` `container` property.
pub use sink::MediaContainer;

use tracing::level_filters::LevelFilter;
use tracing_subscriber::EnvFilter;

/// Registers `moqsink` and `moqsrc` with `plugin` and installs a stderr tracing subscriber
/// filtered by `RUST_LOG`, unless the host process already installed one.
pub fn plugin_init(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
	sink::register(plugin)?;
	source::register(plugin)?;

	let filter = EnvFilter::builder()
		.with_default_directive(LevelFilter::INFO.into())
		.from_env_lossy() // Allow overriding with RUST_LOG
		.add_directive("h2=warn".parse().unwrap())
		.add_directive("moq_tokio::quic::driver=info".parse().unwrap())
		.add_directive("tracing::span=off".parse().unwrap())
		.add_directive("tracing::span::active=off".parse().unwrap());

	let logger = tracing_subscriber::FmtSubscriber::builder()
		.with_writer(std::io::stderr)
		.with_env_filter(filter)
		.finish();

	// An embedding application that installed its own subscriber keeps it.
	let _ = tracing::subscriber::set_global_default(logger);
	Ok(())
}

gst::plugin_define!(
	moq,
	env!("CARGO_PKG_DESCRIPTION"),
	plugin_init,
	concat!(env!("CARGO_PKG_VERSION"), "-", env!("COMMIT_ID")),
	// GStreamer only loads plugins whose license string is in its recognised set. "Apache 2.0" is not,
	// so the registry silently refuses the plugin. The crate is MIT OR Apache-2.0, so declare the MIT side.
	"MIT/X11",
	env!("CARGO_PKG_NAME"),
	env!("CARGO_PKG_NAME"),
	env!("CARGO_PKG_REPOSITORY"),
	env!("BUILD_REL_DATE")
);
