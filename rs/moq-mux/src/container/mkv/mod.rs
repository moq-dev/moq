//! Matroska / WebM container.
//!
//! MKV is an external file format only — no moq wire-level [`Container`]
//! counterpart. [`Import`] parses MKV byte streams into a broadcast,
//! [`Export`] does the reverse.
//!
//! [`Container`]: crate::container::Container

pub mod export;
pub mod import;

pub use export::Export;
pub use import::Import;

#[cfg(test)]
mod export_test;
#[cfg(test)]
mod import_test;
