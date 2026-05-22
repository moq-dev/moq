//! Subscribe to a moq broadcast and decode media frames.
//!
//! - [`Fmp4`] subscribes to a broadcast, decodes every track via
//!   [`Consumer<Hang>`](crate::container::Consumer), and yields a single fMP4 / CMAF byte
//!   stream — the merged init segment followed by moof+mdat fragments in
//!   timestamp order across tracks.
//! - [`Mkv`] does the same but yields a Matroska / WebM byte stream — EBML
//!   header + unknown-size Segment + Cluster fragments.
//!
//! Codec-shape conversion for Annex-B sources is handled by
//! [`crate::transform`], which both exporters compose internally.

mod fmp4;
mod mkv;

pub use fmp4::Fmp4;
pub use mkv::Mkv;

#[cfg(test)]
mod test;
