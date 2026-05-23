//! HLS playlist ingest.
//!
//! HLS is an external streaming format only — no moq wire-level
//! [`Container`] counterpart and no exporter today.
//!
//! [`Container`]: crate::container::Container

pub mod import;

pub use import::{HlsConfig, Import};
