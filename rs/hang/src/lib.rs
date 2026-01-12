//! # hang: WebCodecs compatible media encoding for MoQ
//!
//! `hang` is a media-specific library built on top of [`moq_lite`], providing
//! high-level components for real-time audio and video streaming over QUIC.
//! It implements media containers, codecs, and streaming protocols optimized
//! for real-time live broadcasting.
//!
//! ## Overview
//!
//! While [`moq_lite`] provides the generic transport layer, `hang` adds:
//! - **Catalog**: A list of available tracks and their metadata.
//! - **Container**: A simple timestamped container format.
//! - **Import**: Convert existing formats, like fMP4, into a hang broadcast.
//!
mod error;

pub mod catalog;
pub mod import;
pub mod model;

// export the moq-lite version in use
pub use moq_lite;

pub use error::*;
pub use model::*;
