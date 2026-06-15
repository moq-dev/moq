use gst::glib;

mod sink;
mod source;
mod spike;

use tracing::level_filters::LevelFilter;
use tracing_subscriber::EnvFilter;

pub fn plugin_init(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
	sink::register(plugin)?;
	source::register(plugin)?;
	spike::register(plugin)?;

	let filter = EnvFilter::builder()
		.with_default_directive(LevelFilter::INFO.into())
		.from_env_lossy() // Allow overriding with RUST_LOG
		.add_directive("h2=warn".parse().unwrap())
		.add_directive("quinn=info".parse().unwrap())
		.add_directive("tracing::span=off".parse().unwrap())
		.add_directive("tracing::span::active=off".parse().unwrap());

	let logger = tracing_subscriber::FmtSubscriber::builder()
		.with_writer(std::io::stderr)
		.with_env_filter(filter)
		.finish();

	tracing::subscriber::set_global_default(logger).unwrap();
	Ok(())
}

gst::plugin_define!(
	moq,
	env!("CARGO_PKG_DESCRIPTION"),
	plugin_init,
	concat!(env!("CARGO_PKG_VERSION"), "-", env!("COMMIT_ID")),
	// GStreamer only loads plugins whose license is in its recognised set; the crate is
	// MIT OR Apache-2.0, so declare the MIT side ("Apache 2.0" is rejected and the plugin never loads).
	"MIT/X11",
	env!("CARGO_PKG_NAME"),
	env!("CARGO_PKG_NAME"),
	env!("CARGO_PKG_REPOSITORY"),
	env!("BUILD_REL_DATE")
);
