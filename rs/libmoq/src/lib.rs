//! C FFI bindings for MoQ (Media over QUIC).
//!
//! This library provides a C-compatible API for working with MoQ broadcasts,
//! enabling real-time media delivery with low latency at scale.
//!
//! The API is organized around several key concepts:
//! - **Sessions**: Network connections to MoQ servers
//! - **Origins**: Collections of broadcasts that can be published or consumed
//! - **Broadcasts**: Container for media tracks
//! - **Tracks**: Individual audio or video streams
//! - **Frames**: Individual media samples with timestamps
//!
//! All functions return negative error codes on failure, or non-negative values on success.
//! Most resources are managed through opaque integer handles that must be explicitly closed.

mod api;
mod consume;
mod error;
mod ffi;
mod id;
mod origin;
mod publish;
mod session;
mod state;

pub use api::*;
pub use error::*;
pub use id::*;

pub(crate) use consume::*;
pub(crate) use origin::*;
pub(crate) use publish::*;
pub(crate) use session::*;
pub(crate) use state::*;
