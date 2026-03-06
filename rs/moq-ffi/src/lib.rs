//! UniFFI bindings for [`moq_lite`].
//!
//! Provides a Kotlin/Swift-compatible API for real-time pub/sub over QUIC.
//!
//! ## Concepts
//!
//! - **Session**: Network connection to a MoQ relay
//! - **Origin**: Collection of broadcasts
//! - **Broadcast**: Container for tracks
//! - **Track**: Named stream of groups
//! - **Group**: Collection of frames
//! - **Frame**: Sized payload with timestamp

mod api;
mod consume;
mod error;
mod ffi;
mod id;
mod origin;
mod publish;
mod session;
mod state;

pub use error::*;
pub use id::*;

uniffi::setup_scaffolding!("moq");

pub(crate) use consume::*;
pub(crate) use origin::*;
pub(crate) use publish::*;
pub(crate) use session::*;
pub(crate) use state::*;
