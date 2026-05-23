//! Matroska / WebM container.
//!
//! MKV is an external file format only — no moq wire-level [`Container`]
//! counterpart. [`import::Import`] parses MKV byte streams into a broadcast,
//! [`export::Export`] does the reverse.
//!
//! [`Container`]: crate::container::Container

pub mod export;
pub mod import;

#[cfg(test)]
mod export_test;
#[cfg(test)]
mod import_test;
