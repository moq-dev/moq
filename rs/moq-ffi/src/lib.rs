//! UniFFI bindings for [`moq_net`].
//!
//! Provides a Kotlin/Swift-compatible API for real-time pub/sub over QUIC.
//! Uses async UniFFI objects instead of callbacks for a native async experience.

// uniffi lowers every exported object as an `Arc<T>`, and on wasm moq-net's handles are
// `Rc`-backed, so clippy sees a pointlessly atomic `Arc` at every construction site. The
// `Arc` is uniffi's ABI rather than our choice, and the target is single-threaded, so the
// wasted atomics are what one type serving both targets costs.
#![cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]

#[cfg(target_os = "android")]
mod android;
// Native codecs, the QUIC server, and moq-native's logger have no browser
// equivalent: the browser encodes through WebCodecs and logs through tracing-web.
#[cfg(not(target_arch = "wasm32"))]
pub mod audio;
pub mod consumer;
pub mod error;
mod ffi;
pub mod json;
#[cfg(not(target_arch = "wasm32"))]
mod log;
pub mod media;
pub mod origin;
pub mod producer;
#[cfg(not(target_arch = "wasm32"))]
pub mod server;
pub mod session;
#[cfg(target_arch = "wasm32")]
mod transport;
#[cfg(not(target_arch = "wasm32"))]
pub mod video;

uniffi::setup_scaffolding!("moq");

// The test suite drives a native relay over a tokio runtime.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod test;
