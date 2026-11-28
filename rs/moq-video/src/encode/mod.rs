//! Encode captured video and publish it as a moq video track.
//!
//! The output codec is selected via [`Codec`] (H.264 or H.265); see its docs
//! for which backends cover each on this platform.
//!
//! Entry points, high to low level:
//! - `publish_capture` captures and publishes a webcam (turnkey), returning a
//!   `Publication` that reports its state and the `Driver` that runs it.
//!   Requires the `capture` feature.
//! - [`Producer`] encodes raw [`Frame`](crate::Frame)s you supply and publishes
//!   them (bring your own frames), following the uplink's bandwidth grant between
//!   frames. The counterpart of `moq_audio::encode::Producer`; the capture driver
//!   runs on it.
//! - [`Encoder`] encodes raw frames into [`Encoded`] access units without
//!   publishing them. [`Sink`] is an [`Encoder`] that owns the thread it runs on,
//!   for an encoder outliving a single thread's stack (a shared object, an FFI
//!   handle, a task that migrates between workers).
//! - [`Publisher`] publishes frames you already encoded, with no rate control.
//!
//! Both [`Producer`] and [`Publisher`] advertise their catalog rendition as soon
//! as the track exists, resolved from the encoder itself via [`Config::probe`], so
//! a subscriber can discover a track nothing has encoded yet. That's what lets an
//! on-demand capture wait for its first viewer at all.
//!
//! [`Options`] / `Capture` (with `capture`) / [`Kind`] / [`Config`] configure
//! them. [`Config::preset`] picks a [`Preset`], how the encoder trades latency
//! for compression at the configured bitrate. The decode/consume counterpart
//! (mirror of `moq-audio`'s consumer) lives in the sibling
//! [`decode`](crate::decode) module.
//!
//! The shared [`moq_mux::rate`] policy maps a congestion-control bandwidth
//! estimate onto the encoder's bitrate, which [`Producer`] drives for you.

pub(crate) mod backend;
#[cfg(feature = "capture")]
mod capture;
#[cfg_attr(
	not(feature = "capture"),
	allow(
		dead_code,
		reason = "compiled without `capture` so its tests stay in the default merge gate"
	)
)]
mod cuts;
mod encoded;
mod encoder;
mod producer;
mod publisher;
mod sink;

pub use backend::NAMES;
#[cfg(feature = "capture")]
pub use capture::{Capture, Driver, Publication, Rendition, State, Status, publish_capture};
pub use encoded::Encoded;
pub use encoder::{Applied, Codec, Config, Encoder, Kind, Preset};
pub use producer::{Options, Producer, Rung};
pub use publisher::{Finished, Publisher};
pub use sink::Sink;

#[cfg(test)]
mod tests {
	/// The worker-backed API is the supported way to move an encoder between
	/// tasks or threads, so keep that contract checked on every platform.
	#[test]
	fn sink_is_send() {
		fn assert_send<T: Send>() {}
		assert_send::<super::Sink>();
	}
}
