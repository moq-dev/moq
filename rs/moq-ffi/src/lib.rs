//! UniFFI bindings for [`moq_lite`].
//!
//! Provides a Kotlin/Swift-compatible API for real-time pub/sub over QUIC.
//! Uses async UniFFI objects instead of callbacks for a native async experience.

pub mod consumer;
pub mod error;
mod ffi;
mod log;
pub mod media;
pub mod origin;
pub mod producer;
pub mod session;

uniffi::setup_scaffolding!("moq");

#[cfg(test)]
mod test;
