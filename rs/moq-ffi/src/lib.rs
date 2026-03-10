//! UniFFI bindings for [`moq_lite`].
//!
//! Provides a Kotlin/Swift-compatible API for real-time pub/sub over QUIC.
//! Uses async UniFFI objects instead of callbacks for a native async experience.

mod api;
mod error;
mod ffi;

uniffi::setup_scaffolding!("moq");
