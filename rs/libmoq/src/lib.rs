//! FFI bindings for [`moq_lite`].
//!
//! Provides a C-compatible API (via the `c-api` feature, default) or a UniFFI-based API
//! (via the `uniffi-api` feature) for real-time pub/sub over QUIC. These features are
//! mutually exclusive; enabling both will produce a compile error.
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

#[cfg(all(feature = "c-api", feature = "uniffi-api"))]
compile_error!("Features `c-api` and `uniffi-api` are mutually exclusive. Enable only one.");

#[cfg(feature = "c-api")]
mod api;
mod consume;
mod error;
mod ffi;
mod id;
mod origin;
mod publish;
mod session;
mod state;
#[cfg(feature = "uniffi-api")]
mod uniffi_api;

#[cfg(feature = "c-api")]
pub use api::*;
pub use error::*;
pub use id::*;

#[cfg(feature = "uniffi-api")]
uniffi::setup_scaffolding!("moq");

pub(crate) use consume::*;
pub(crate) use origin::*;
pub(crate) use publish::*;
pub(crate) use session::*;
pub(crate) use state::*;
