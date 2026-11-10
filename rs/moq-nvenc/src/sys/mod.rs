//! Auto-generated bindings to NVIDIA Video Codec SDK.
//!
//! The bindings were generated from the vendored headers with
//! [bindgen](https://github.com/rust-lang/rust-bindgen); see the upstream
//! `nvidia-video-codec-sdk` repo for the generation scripts.

mod guid;
mod version;

// The bindgen output is plain C-ABI type/enum/fn-pointer definitions generated
// for Linux, the only target that loads the driver. They compile on every
// target, so the crate builds elsewhere as a stub that never loads NVENC.
#[allow(warnings, reason = "bindgen output")]
#[rustfmt::skip]
mod linux_sys;
pub use linux_sys::*;
