//! C FFI bindings for [`moq_lite`].
//!
//! Provides a C-compatible API for real-time pub/sub over QUIC.
//!
//! ## Concepts
//!
//! - **Session**: Network connection to a MoQ relay
//! - **Origin**: Collection of broadcasts
//! - **Broadcast**: Container for tracks
//! - **Track**: Named stream of groups
//! - **Group**: Collection of frames
//! - **Frame**: Sized payload with timestamp
//!
//! ## Error Handling
//!
//! All functions return negative error codes on failure or non-negative values on success.
//! Resources are managed through opaque integer handles that must be explicitly closed.

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
