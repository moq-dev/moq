//! Native playback for a MoQ broadcast.
//!
//! The `play` feature gates the device stacks: the winit event loop, the wgpu
//! window, and the cpal speaker. Everything the player decides rather than
//! draws (argument validation, rendition selection, media timing) compiles
//! without them, so a default build typechecks it and runs its tests.
//!
//! The media tasks need the decoders, so they stay behind `play`, but they
//! reach the devices only through `output`: tests drive them against a
//! recorder (`fake`), and the merge gate's test build turns `play` on for every
//! PR that reaches this crate.

#![cfg_attr(
	not(feature = "play"),
	allow(
		dead_code,
		reason = "with `play` off the event loop is gone and nothing calls the modules below. They are still compiled and tested, which is the point"
	)
)]

mod args;
mod buffer;
mod layout;
mod playback;
mod timeline;

#[cfg(all(test, feature = "play"))]
mod fake;
#[cfg(feature = "play")]
mod headless;
#[cfg(feature = "play")]
mod media;
#[cfg(feature = "play")]
mod metrics;
#[cfg(feature = "play")]
mod output;
#[cfg(feature = "play")]
mod video;
#[cfg(feature = "play")]
mod window;

#[cfg(feature = "play")]
pub use args::Args;
/// Play a broadcast through a window or an audio-only speaker.
#[cfg(feature = "play")]
pub async fn run(
	origin: hang::moq_net::origin::Consumer,
	broadcast: String,
	args: Args,
	network: tokio::task::JoinSet<anyhow::Result<()>>,
) -> anyhow::Result<()> {
	if args.select.video == Some(crate::subscribe::Mode::Off) {
		headless::run(origin, broadcast, args, network).await
	} else {
		window::run(origin, broadcast, args, network)
	}
}
