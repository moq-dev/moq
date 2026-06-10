//! Native video capture, encoding, and publishing for Media over QUIC.
//!
//! Counterpart to [`moq-audio`](https://crates.io/crates/moq-audio) for
//! video tracks. Sits on top of [`moq_mux`] (and the `hang` catalog) and
//! adds the native pieces a desktop/CLI publisher needs:
//!
//! - [`capture`] describes a frame source ([`capture::Config`]) and grabs
//!   frames via libavdevice. Today that's a webcam (avfoundation / v4l2 /
//!   dshow); screen capture would slot in here too.
//! - [`encode`] H.264-encodes frames and publishes them through
//!   [`moq_mux::codec::h264::Import`], which handles catalog registration
//!   and framing. Two entry points:
//!   - [`encode::publish_capture`] captures a webcam and publishes it (turnkey).
//!     It encodes strictly on demand: the track and catalog are advertised up
//!     front, but the camera opens only while a subscriber is watching and is
//!     released when the last one leaves.
//!   - [`encode::Producer`] publishes H.264 you encoded yourself.
//!
//! The decode/consume side (the mirror of `moq-audio`'s `AudioConsumer`) is
//! not implemented yet; native subscribers can keep using `moq_mux` directly.
//!
//! ## API stability
//!
//! The public API is deliberately ffmpeg-free at the signature level: the raw
//! capture/encode types that traffic in [`ffmpeg`] frames are internal, so a
//! major `ffmpeg-next` bump is not a breaking change for consumers. (The one
//! exception is [`Error::Ffmpeg`], which carries a typed `ffmpeg_next::Error`.)
//! Config structs are `#[non_exhaustive]`: build them via `default()` and set
//! fields, so new options stay additive.

pub mod capture;
pub mod encode;

mod error;

/// Re-exported so callers can name the inner type of [`Error::Ffmpeg`]
/// without taking their own (possibly version-mismatched) ffmpeg dependency.
pub use ffmpeg_next as ffmpeg;

pub use error::Error;
