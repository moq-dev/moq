//! Native video capture, encoding, and publishing for Media over QUIC.
//!
//! Counterpart to [`moq-audio`](https://crates.io/crates/moq-audio) for
//! video tracks. Sits on top of [`moq_mux`] (and the `hang` catalog) and
//! adds the native pieces a desktop/CLI publisher needs:
//!
//! - [`capture`] grabs frames via libavdevice. Today that's a webcam
//!   ([`capture::Camera`], avfoundation / v4l2 / dshow); screen capture would
//!   slot in here too.
//! - [`encode`] turns those frames into Annex-B H.264 (preferring a platform
//!   hardware encoder) and publishes them through
//!   [`moq_mux::codec::h264::Import`], which handles catalog registration
//!   and framing.
//! - [`encode::publish_capture`] is the one-call capture-encode-publish entry
//!   the CLI uses. It encodes strictly on demand: the track and catalog are
//!   advertised up front, but the camera opens only while a subscriber is
//!   watching and is released when the last one leaves.
//!
//! The decode/consume side (the mirror of `moq-audio`'s `AudioConsumer`) is
//! not implemented yet; native subscribers can keep using `moq_mux` directly.
//!
//! ## API stability
//!
//! [`encode::publish_capture`] is the insulated, recommended entry point: its
//! signature is pure moq + plain config structs, so it survives an
//! `ffmpeg-next` bump. The lower-level building blocks ([`capture::Camera`],
//! [`encode::Encoder`]) expose [`ffmpeg`] frame/pixel types directly, so a
//! major `ffmpeg-next` version bump is a breaking change for code that uses
//! them. Config structs are `#[non_exhaustive]`: build them via `default()`
//! (or their constructor) and set fields, so new options stay additive.

pub mod capture;
pub mod encode;

mod error;

/// Re-export so callers can name the frame/pixel types in our signatures
/// without taking their own ffmpeg dependency.
pub use ffmpeg_next as ffmpeg;

pub use error::Error;
