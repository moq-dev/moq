//! Media muxers and demuxers for MoQ.
//!
//! Sits between [`moq_net`] and [`hang`]: takes media in, produces a moq
//! broadcast, or the other way around.
//!
//! - [`container`](mod@container) — container formats.
//! - [`codec`] — codecs.
//! - [`catalog`] — catalog publish/subscribe.
//! - [`import`](mod@import) — format dispatcher.

pub mod catalog;
pub mod codec;
pub mod container;
mod error;
pub mod import;

pub use error::*;
