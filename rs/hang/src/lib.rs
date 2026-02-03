//! # hang: WebCodecs compatible media encoding for MoQ
//!
//! Media-specific library built on [`moq_lite`] for streaming audio and video with WebCodecs.
//!
//! ## Overview
//!
//! `hang` adds media support to the generic [`moq_lite`] transport:
//!
//! - **Catalog**: JSON track containing codec info and track metadata, updated live as tracks change.
//! - **Container**: Simple frame format consisting of timestamp (microseconds) + codec bitstream payload.
//!
//! For importing fMP4/CMAF/HLS streams, see the `moq-mux` crate.
//!
//! ## Frame Container
//!
//! Each frame consists of:
//! - Timestamp (u64): presentation time in microseconds
//! - Payload: raw encoded codec data (H.264, Opus, etc.)
//!
//! This simple format works directly with WebCodecs APIs in browsers.
//!
mod error;

pub mod catalog;
pub mod model;

// export the moq-lite version in use
pub use moq_lite;

pub use error::*;
pub use model::*;
