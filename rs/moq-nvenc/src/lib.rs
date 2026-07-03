//! Bindings for the [NVIDIA Video Codec SDK](https://developer.nvidia.com/video-codec-sdk).
//!
//! The raw bindings can be found in [`sys`].
//! Parts of the API have been wrapped in [`safe`].
//!
//! Feel free to contribute!
//!
//! ---
//!
//! # Encoding
//!
//! See [NVIDIA Video Codec SDK - Video Encoder API Programming Guide](https://docs.nvidia.com/video-technologies/video-codec-sdk/12.0/nvenc-video-encoder-api-prog-guide/index.html).
//!
//! The main entrypoint for the encoder API is the [`Encoder`] type.
//!
//! Usage follows this structure:
//! 1. Initialize an [`Encoder`] with an encode device (such as CUDA).
//! 2. Configure the encoder and start a [`Session`].
//! 3. Create input [`Buffer`]s  (or [`RegisteredResource`]) and output
//!    [`Bitstream`]s.
//! 4. Encode frames with [`Session::encode_picture`].
//!
//! See the mentioned types for more info on how to use each.
//!
//! # Decoding
//!
//! There is no safe wrapper yet.

// Vendored third-party bindings: keep the workspace's `clippy -D warnings` and
// `rustdoc -D warnings` from churning upstream code (broken intra-doc links, the
// strict clippy lints the crate opted into, etc.).
#![allow(clippy::all, clippy::pedantic, rustdoc::all)]

pub mod safe;
pub mod sys;

#[macro_use]
extern crate lazy_static;

pub use safe::*;
