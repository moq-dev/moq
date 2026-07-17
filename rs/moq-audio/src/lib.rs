//! Native audio capture, encoding, and decoding for Media over QUIC.
//!
//! Counterpart to [`moq-video`](https://crates.io/crates/moq-video) for audio
//! tracks, and shaped the same way. Sits on top of [`moq_mux`] and [`hang`] and
//! adds the missing piece for native callers: a Rust-native Opus implementation
//! that turns raw PCM into the bitstreams `moq_mux::codec::opus` already knows
//! how to ingest (and vice versa for decode).
//!
//! - `capture` describes an audio source (`capture::Config`) and grabs buffers
//!   per platform: a microphone via cpal (CoreAudio / WASAPI / ALSA) everywhere,
//!   or macOS system audio via ScreenCaptureKit. `capture::Source` picks between
//!   them and `capture::devices` lists the inputs and hands back the ids it
//!   takes. Requires the `capture` feature, so these names are unlinked here:
//!   they don't exist in a default build.
//! - [`encode`] encodes PCM and publishes it through `moq_mux::container`,
//!   registering the rendition in the `hang` catalog. Two entry points:
//!   - `encode::publish_capture` captures a microphone and publishes it
//!     (turnkey). It encodes strictly on demand: the track and catalog are
//!     advertised up front, but the device opens only while a subscriber is
//!     listening and is released when the last one leaves.
//!   - [`encode::Producer`] publishes PCM you hand it.
//! - [`decode`] subscribes to an encoded track and decodes it back to PCM.
//!   [`decode::Consumer`] is the mirror of [`encode::Producer`].
//!
//! [`Format`] mirrors WebCodecs `AudioData.format`; the helpers convert between
//! any supported layout and the interleaved `f32` representation libopus
//! expects. [`Frame`] is a thin owned buffer: a timestamp and a payload. PCM
//! layout lives on the producer / consumer via [`encode::Input`] /
//! [`decode::Config`], not on each frame, so callers can't drift between calls.

mod error;
mod format;
mod frame;
mod opus;
mod resample;

#[cfg(feature = "capture")]
pub mod capture;
pub mod decode;
pub mod encode;

pub use error::Error;
pub use format::Format;
pub use frame::Frame;
pub use resample::Resampler;
