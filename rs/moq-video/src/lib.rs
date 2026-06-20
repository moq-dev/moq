//! Native video capture, encoding, and publishing for Media over QUIC.
//!
//! Counterpart to [`moq-audio`](https://crates.io/crates/moq-audio) for
//! video tracks. Sits on top of [`moq_mux`] (and the `hang` catalog) and
//! adds the native pieces a desktop/CLI publisher needs:
//!
//! - [`capture`] describes a frame source ([`capture::Config`]) and grabs
//!   frames per platform: AVFoundation/ScreenCaptureKit on macOS, native V4L2
//!   on Linux, native Media Foundation on Windows. Today that's a webcam or
//!   the screen.
//! - [`encode`] encodes frames with a native backend and publishes them through
//!   the matching `moq_mux::codec` importer, which handles catalog registration
//!   and framing. The codec is chosen via [`encode::Codec`]: H.264 (openh264 /
//!   VideoToolbox / NVENC / VAAPI) or H.265 (VideoToolbox). Two entry points:
//!   - [`encode::publish_capture`] captures a webcam and publishes it (turnkey).
//!     It encodes strictly on demand: the track and catalog are advertised up
//!     front, but the camera opens only while a subscriber is watching and is
//!     released when the last one leaves.
//!   - [`encode::Producer`] publishes packets you encoded yourself.
//! - [`decode`] subscribes to an H.264 track and decodes it to raw I420 frames
//!   with a native backend (VideoToolbox / openh264). [`decode::Consumer`] is the
//!   mirror of `moq-audio`'s `AudioConsumer`.
//!
//! ## API stability
//!
//! The public API is codec-agnostic: no public type, signature, or error
//! variant names a backend (openh264 / VideoToolbox / NVENC) or a capture
//! implementation. [`encode::Encoder`] takes raw RGBA bytes, [`decode::Consumer`]
//! returns raw I420, and the camera capture path stays internal. So swapping or
//! bumping any backend crate is not a breaking change for consumers. Config
//! structs are `#[non_exhaustive]`: build them via `default()`/`new()` and set
//! fields, so new options stay additive.

pub mod capture;
pub mod decode;
pub mod encode;

mod error;
mod frame;

#[cfg(target_os = "windows")]
mod mf;

pub use error::Error;
