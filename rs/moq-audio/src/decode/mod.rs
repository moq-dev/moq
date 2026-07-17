//! Subscribe to an encoded audio track and decode it to raw PCM.
//!
//! The decode counterpart to [`encode`](crate::encode), and the mirror of
//! `moq-video`'s [`decode`](https://docs.rs/moq-video) module.
//!
//! Entry points, high to low level:
//! - [`Consumer`] subscribes to a track and hands back decoded [`Frame`](crate::Frame)s.
//! - [`Decoder`] decodes packets you supply (bring your own payloads).
//!
//! [`Config`] configures both: it declares the PCM layout to emit, since the
//! codec's own shape comes from the catalog.

mod consumer;
mod decoder;

pub use consumer::Consumer;
pub use decoder::{Config, Decoder};
