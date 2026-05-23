//! Matroska / WebM.
//!
//! An EBML-based file format. moq-mux uses it as an external interchange
//! format only, not as a wire format: [`Import`] parses MKV byte streams
//! into a broadcast and [`Export`] does the reverse.

mod export;
mod import;

pub use export::*;
pub use import::*;

#[cfg(test)]
mod export_test;
#[cfg(test)]
mod import_test;
