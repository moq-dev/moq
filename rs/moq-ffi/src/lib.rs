//! UniFFI bindings for [`moq_net`].
//!
//! Provides a Kotlin/Swift-compatible API for real-time pub/sub over QUIC.
//! Uses async UniFFI objects instead of callbacks for a native async experience.

#[cfg(target_os = "android")]
mod android;
pub mod audio;
pub mod consumer;
pub mod error;
mod ffi;
mod log;
pub mod media;
pub mod origin;
pub mod producer;
pub mod server;
pub mod session;

uniffi::setup_scaffolding!("moq");

#[cfg(test)]
mod test;
