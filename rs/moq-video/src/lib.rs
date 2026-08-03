//! Native video capture, encoding, and publishing for Media over QUIC.
//!
//! Counterpart to [`moq-audio`](https://crates.io/crates/moq-audio) for video
//! tracks, and shaped the same way: both split into `capture` / `encode` /
//! `decode` role modules over a shared root [`Error`], with `render` as video's
//! fourth. Sits on top of [`moq_mux`] (and the `hang` catalog) and adds the
//! native pieces a desktop/CLI publisher needs.
//!
//! A raw picture is a [`Frame`] wherever it crosses the API: a timestamp and a
//! [`Surface`] holding the pixels. Capture and [`decode`] produce them, [`encode`]
//! consumes them and hands back the compressed [`encode::Encoded`], and [`Size`]
//! names a resolution. Keyframes are the encoder's business: it inserts them per
//! [`encode::Config::gop`], and [`encode::Encoder::keyframe`] is there for the
//! rarer case where a caller needs one at a specific frame.
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
//!   - [`encode::Encoder`] encodes [`Frame`]s you supply (from capture, a
//!     decoder, or your own pixels via [`Surface::rgba`]) and
//!     [`encode::Producer`] publishes the results.
//! - [`decode`] subscribes to an H.264, H.265, or AV1 track and decodes it to
//!   raw frames with a native backend (VideoToolbox on macOS, Media Foundation /
//!   DXVA on Windows, NVDEC on Linux, openh264 software fallback for H.264).
//!   [`decode::Consumer`] is the mirror of `moq_audio::decode::Consumer`. An
//!   NVDEC frame stays in CUDA memory and feeds [`encode::Encoder::encode`]
//!   zero-copy (the transcode path), scaled in hardware via
//!   [`decode::Config::resize`].
//! - `render` draws a [`Frame`] on the GPU and hands back a `wgpu` texture to
//!   present, importing a GPU frame's surface directly where the platform
//!   allows and uploading I420 otherwise. Behind the non-default `render`
//!   feature, since it pulls in a graphics stack a publisher or relay does not
//!   need.
//!
//! ## API stability
//!
//! The public API is codec-agnostic: no public type, signature, or error
//! variant names a backend (openh264 / VideoToolbox / NVENC / NVDEC) or a
//! capture implementation. [`encode::Encoder`] takes a [`Frame`],
//! [`decode::Consumer`] returns one (CPU I420 on demand, GPU-resident when
//! hardware decoded), and the camera capture path stays internal. So swapping or bumping any backend crate is not a breaking change
//! for consumers. Config structs are `#[non_exhaustive]`: build them via
//! `default()`/`new()` and set fields, so new options stay additive.
//!
//! The one deliberate exception is [`Surface`], the enum behind every frame.
//! Its variants name platform representations (`CVPixelBuffer`, Direct3D11,
//! CUDA) so you can render or re-encode a frame yourself without a CPU round
//! trip, which means a major bump of one of those platform crates is a breaking
//! change here. It is `#[non_exhaustive]` and every variant has a universal
//! fallback in [`Surface::into_i420`], so matching on it stays portable: take the
//! fast path you recognize and let the `_` arm handle the rest.

pub mod capture;
pub mod decode;
pub mod encode;
#[cfg(feature = "render")]
pub mod render;
pub mod resize;

mod color;
mod error;
pub mod frame;
mod size;

#[cfg(target_os = "windows")]
mod mf;

pub use color::Color;
pub use error::Error;
pub use frame::{Frame, I420, Surface};
pub use size::Size;

/// The CoreFoundation bindings owning the handle [`Surface::into_pixel_buffer`]
/// returns, re-exported alongside [`objc2_core_video`] for the same reason.
#[cfg(target_os = "macos")]
pub use objc2_core_foundation;
/// The CoreVideo bindings [`Surface::into_pixel_buffer`] hands back,
/// re-exported so you name the exact version this crate links rather than guessing
/// at a matching one. A major bump here is a breaking change for this crate.
#[cfg(target_os = "macos")]
pub use objc2_core_video;
/// The Direct3D11 bindings [`frame::d3d11::Texture`] hands back, re-exported for
/// the same reason as the Apple ones above: name the exact version this crate
/// links rather than guessing at a matching one, since a device and a texture
/// from a different `windows` build are different types. A major bump here is a
/// breaking change for this crate.
#[cfg(target_os = "windows")]
pub use windows;
