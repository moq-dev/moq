//! Native video capture, encoding, and publishing for Media over QUIC.
//!
//! Counterpart to [`moq-audio`](https://crates.io/crates/moq-audio) for
//! video tracks. Sits on top of [`moq_mux`] (and the `hang` catalog) and
//! adds the native pieces a desktop/CLI publisher needs:
//!
//! - [`capture`] describes a frame source ([`capture::Config`]) and grabs
//!   frames via [`nokhwa`](https://crates.io/crates/nokhwa) (avfoundation /
//!   v4l2 / msmf). Today that's a webcam.
//! - [`encode`] H.264-encodes frames with a native backend (openh264 /
//!   VideoToolbox / NVENC) and publishes them through
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
//! The public API is codec-agnostic: no public type, signature, or error
//! variant names a backend (openh264 / VideoToolbox / NVENC) or capture
//! (`nokhwa`) type. [`encode::Encoder`] takes raw RGBA bytes, and the camera
//! capture path stays internal. So swapping or bumping any backend crate is not
//! a breaking change for consumers. Config structs are `#[non_exhaustive]`:
//! build them via `default()`/`new()` and set fields, so new options stay additive.

pub mod capture;
pub mod encode;

mod error;
mod frame;

pub use error::Error;
