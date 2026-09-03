//! Native audio capture, encoding, and decoding for Media over QUIC.
//!
//! Counterpart to [`moq-video`](https://crates.io/crates/moq-video) for audio
//! tracks, and shaped the same way. Sits on top of [`moq_mux`] and [`hang`] and
//! adds the missing piece for native callers: Rust-native Opus and uncompressed
//! PCM codecs that turn raw samples into HANG audio tracks and back, plus AAC-LC
//! on the way in, which is what the gateways (RTMP, SRT, HLS, gstreamer) publish.
//!
//! - `capture` describes an audio source (`capture::Config`) and grabs buffers
//!   per platform: a microphone via cpal (CoreAudio / WASAPI / ALSA) everywhere,
//!   or macOS system audio via ScreenCaptureKit. `capture::Source` picks between
//!   them and `capture::devices` lists the inputs and hands back the ids it
//!   takes. Requires the `capture` feature, so these names are unlinked here:
//!   they don't exist in a default build.
//! - [`encode`] encodes PCM and publishes it through `moq_mux::container`,
//!   registering the rendition in the `hang` catalog. Two entry points:
//!   - `encode::Publication` and `encode::Driver` capture a controllable
//!     microphone publication. The retained publication starts, stops, and
//!     replaces the input while preserving one track identity, and reports the
//!     active device, failures, and post-processing level.
//!   - `encode::publish_capture` is the turnkey shorthand. It encodes strictly
//!     on demand: the track and catalog are advertised up front, but the device
//!     opens only while a subscriber is listening and is released when the last
//!     one leaves.
//!   - [`encode::Producer`] publishes PCM you hand it.
//! - [`decode`] subscribes to an encoded track and decodes it back to PCM.
//!   [`decode::Consumer`] is the mirror of [`encode::Producer`]. It reads AAC-LC
//!   too, behind the default-on `aac` feature, since a broadcast that came in
//!   through a gateway is AAC rather than one of the two codecs we encode.
//! - `playback` plays decoded PCM out a speaker. `playback::Engine` owns the
//!   output device and mixes the `playback::Sink`s registered with it, so one
//!   device serves every track in a call. Requires the `playback` feature, so
//!   these names are unlinked here too.
//! - `aec` keeps the speaker out of the microphone, which is what a conference
//!   on a laptop needs to not send itself back. `playback::Engine::canceller`
//!   builds an `aec::Canceller` from the mix it is playing and
//!   `capture::Config::aec` hands it to the microphone. Requires the `aec`
//!   feature, which implies both of the above.
//!
//! [`Format`] mirrors WebCodecs `AudioData.format`; the helpers convert between
//! any supported layout and the interleaved `f32` representation libopus
//! expects. [`Frame`] is a thin owned buffer: a timestamp, a payload, and the
//! [`Activity`] it was decoded from, which is how a caller tells coded audio
//! from the frames an Opus sender withholds while its input is silent. PCM
//! layout lives on the producer / consumer via [`encode::Input`] /
//! [`decode::Config`], not on each frame, so callers can't drift between calls.

#[cfg(feature = "aac")]
mod aac;
mod activity;
mod error;
mod format;
mod frame;
mod opus;
mod pcm;
mod resample;

#[cfg(feature = "aec")]
pub mod aec;
#[cfg(feature = "capture")]
pub mod capture;
pub mod decode;
pub mod encode;

#[cfg(feature = "playback")]
pub mod playback;

pub use activity::Activity;
pub use error::Error;
pub use format::Format;
pub use frame::Frame;
pub use resample::Resampler;
