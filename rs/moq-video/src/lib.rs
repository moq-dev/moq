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
//! The public API is deliberately ffmpeg-free: no public type, signature, or
//! error variant names an `ffmpeg-next` type. [`encode::Encoder`] takes raw
//! RGBA bytes (not an ffmpeg frame), the camera capture/encode path stays
//! internal, and [`Error::Ffmpeg`] carries a plain message.
//! So a major `ffmpeg-next` bump is not a breaking change for consumers, and
//! we don't re-export `ffmpeg-next`. Config structs are `#[non_exhaustive]`:
//! build them via `default()`/`new()` and set fields, so new options stay additive.

pub mod capture;
pub mod encode;

mod error;

pub use error::Error;
