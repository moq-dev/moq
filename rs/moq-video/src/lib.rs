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
//! - `capture` describes a frame source and grabs frames per platform:
//!   AVFoundation/ScreenCaptureKit on macOS, native V4L2 on Linux, native Media
//!   Foundation (camera), DXGI Desktop Duplication (screen), and GDI (window) on
//!   Windows, plus portal/PipeWire on Wayland and X11 capture on Linux. Use
//!   [`capture::open`] for an embeddable raw-frame stream or
//!   [`encode::publish_capture`] for turnkey publication. It requires the
//!   default-on `capture` feature.
//! - [`encode`] encodes frames with a native backend and publishes them through
//!   the matching `moq_mux::codec` importer, which handles catalog registration
//!   and framing. The codec is chosen via [`encode::Codec`]: H.264 (openh264 /
//!   VideoToolbox / Media Foundation / NVENC / VAAPI / V4L2) or H.265
//!   (VideoToolbox / Media Foundation / NVENC). Two entry points:
//!   - `encode::publish_capture` captures a webcam and publishes it (turnkey).
//!     It encodes strictly on demand: the track and catalog are advertised up
//!     front (the camera opens once at startup so they can be exact), and the
//!     encoder runs only while a subscriber is watching. Requires the `capture`
//!     feature.
//!   - [`encode::Encoder`] encodes [`Frame`]s you supply (from capture, a
//!     decoder, or your own pixels via [`Surface::rgba`]) and
//!     [`encode::Producer`] publishes the results.
//! - [`decode`] subscribes to an H.264, H.265, or AV1 track and decodes it to
//!   raw frames with a native backend (VideoToolbox on macOS, Media Foundation /
//!   DXVA on Windows, NVDEC or an ARM SoC's V4L2 M2M decoder on Linux, openh264
//!   software fallback for H.264).
//!   [`decode::Consumer`] is the mirror of `moq_audio::decode::Consumer`. An
//!   NVDEC frame stays in CUDA memory and feeds [`encode::Encoder::encode`]
//!   zero-copy (the transcode path), scaled in hardware via
//!   [`decode::Config::resize`].
//! - [`convert`] downloads any [`Surface`] to owned, tightly packed RGBA pixels
//!   for CPU image and UI toolkits, honoring native color metadata when present.
//! - `render` draws a [`Frame`] on the GPU and hands back a `wgpu` texture to
//!   present, importing a GPU frame's surface directly where the platform
//!   allows and uploading I420 otherwise. Behind the non-default `render`
//!   feature, since it pulls in a graphics stack a publisher or relay does not
//!   need.
//!
//! ## API stability
//!
//! The public API is codec-agnostic: no public type, signature, or error
//! variant names a backend (openh264 / VideoToolbox / NVENC / NVDEC / V4L2) or a
//! codec implementation. [`encode::Encoder`] takes a [`Frame`],
//! [`decode::Consumer`] returns one (CPU I420 on demand, GPU-resident when
//! hardware decoded), and [`capture::Stream`] returns a [`Surface`]. So swapping
//! or bumping any backend crate is not a breaking change for consumers. Config
//! structs are `#[non_exhaustive]`: build them via `default()`/`new()` and set
//! fields, so new options stay additive.
//!
//! The one deliberate exception is [`Surface`], the enum behind every frame.
//! Its variants name platform representations (`CVPixelBuffer`, Direct3D11,
//! CUDA, `AHardwareBuffer`) so you can render or re-encode a frame yourself
//! without a CPU round trip, which means a major bump of one of those platform
//! crates is a breaking change here. It is `#[non_exhaustive]` and every variant has a universal
//! fallback in [`Surface::into_i420`], so matching on it stays portable: take the
//! fast path you recognize and let the `_` arm handle the rest.

#[cfg(feature = "capture")]
pub mod capture;
pub mod convert;
pub mod decode;
pub mod encode;
#[cfg(feature = "render")]
pub mod render;
pub mod resize;

mod color;
mod error;
pub mod frame;
mod size;
// Only the threaded sinks use this, and both are compiled out on macOS, where
// the codecs run inline (no COM apartment to confine). Ungated it is dead code
// there, which `-D warnings` rejects.
#[cfg(not(target_os = "macos"))]
mod worker;

#[cfg(target_os = "windows")]
mod mf;

#[cfg(all(target_os = "linux", feature = "v4l2"))]
mod v4l2;

pub use color::Color;
pub use error::Error;
#[cfg(all(target_os = "linux", feature = "dmabuf"))]
pub use frame::{DmaBuf, DmaBufExport, DmaBufPlane, DrmFormat};
pub use frame::{Frame, I420, Surface};
pub use size::Size;

/// The NDK bindings [`frame::android::HardwareBuffer::buffer`] hands back,
/// re-exported for the same reason as the Apple and Windows ones: name the
/// exact version this crate links rather than guessing at a matching one, since a
/// hardware buffer from a different `ndk` build is a different type. A major bump
/// here is a breaking change for this crate.
#[cfg(all(target_os = "android", feature = "mediacodec"))]
pub use ndk;
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
