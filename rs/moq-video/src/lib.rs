//! Native video capture, encoding, and publishing for Media over QUIC.
//!
//! Counterpart to [`moq-audio`](https://crates.io/crates/moq-audio) for video
//! tracks, and shaped the same way: both split into `capture` / `encode` /
//! `decode` role modules over a shared root [`Error`]. Sits on top of [`moq_mux`]
//! (and the `hang` catalog) and adds the native pieces a desktop/CLI publisher
//! needs:
//!
//! - [`capture`] describes a frame source ([`capture::Config`]) and grabs
//!   frames per platform: AVFoundation/ScreenCaptureKit on macOS, native V4L2
//!   on Linux, native Media Foundation (camera) and DXGI Desktop Duplication
//!   (screen) on Windows. [`capture::Source`] picks a camera, a display, or
//!   (macOS only) a single window or every window of an application;
//!   [`capture::cameras`], [`capture::displays`], [`capture::windows`], and
//!   [`capture::apps`] list what's available and hand back the ids it takes.
//! - [`encode`] encodes frames with a native backend and publishes them through
//!   the matching `moq_mux::codec` importer, which handles catalog registration
//!   and framing. The codec is chosen via [`encode::Codec`]: H.264 (openh264 /
//!   VideoToolbox / Media Foundation / NVENC / VAAPI) or H.265 (VideoToolbox /
//!   Media Foundation / NVENC). Two entry points:
//!   - [`encode::publish_capture`] captures a webcam and publishes it (turnkey).
//!     It encodes strictly on demand: the track and catalog are advertised up
//!     front, but the camera opens only while a subscriber is watching and is
//!     released when the last one leaves.
//!   - [`encode::Producer`] publishes packets you encoded yourself.
//! - [`decode`] subscribes to an H.264, H.265, or AV1 track and decodes it to
//!   raw frames with a native backend (VideoToolbox on macOS, Media Foundation /
//!   DXVA on Windows, NVDEC on Linux, openh264 software fallback for H.264).
//!   [`decode::Consumer`] is the mirror of `moq_audio::decode::Consumer`. An
//!   NVDEC frame stays in CUDA memory and feeds [`encode::Encoder::encode`]
//!   zero-copy (the transcode path), scaled in hardware via
//!   [`decode::Config::resize`]. [`Size`] names a resolution wherever one
//!   crosses the API.
//!
//! ## API stability
//!
//! The public API is codec-agnostic: no public type, signature, or error
//! variant names a backend (openh264 / VideoToolbox / NVENC / NVDEC) or a
//! capture implementation. [`encode::Encoder`] takes raw RGBA bytes,
//! [`decode::Consumer`] returns opaque [`decode::Frame`]s (CPU I420 on demand,
//! GPU-resident when hardware decoded), and the camera capture path stays
//! internal. So swapping or bumping any backend crate is not a breaking change
//! for consumers. Config structs are `#[non_exhaustive]`: build them via
//! `default()`/`new()` and set fields, so new options stay additive.
//!
//! The one deliberate exception is the off-by-default `surface` feature, which
//! exposes the GPU handle behind a decoded frame (`decode::Frame::pixel_buffer`)
//! so you can render or re-encode it yourself without a CPU round trip. That
//! handle is a platform type, so enabling the feature couples you to the
//! `objc2-core-video` version this crate links. Leave it off and the guarantee
//! above holds unchanged.

pub mod capture;
pub mod decode;
pub mod encode;

mod error;
mod frame;
mod size;

#[cfg(target_os = "windows")]
mod mf;

pub use error::Error;
pub use size::Size;

/// The CoreVideo bindings [`decode::Frame::pixel_buffer`] hands back, re-exported
/// so you name the exact version this crate links rather than guessing at a
/// matching one. A major bump here is a breaking change for `surface` users.
#[cfg(all(feature = "surface", target_os = "macos"))]
pub use objc2_core_video;
