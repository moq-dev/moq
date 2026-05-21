//! Subscribe to a moq broadcast and decode media frames.
//!
//! - [`Fmp4`] subscribes to a broadcast, decodes every track via
//!   [`Consumer<Hang>`](crate::container::Consumer), and yields a single fMP4 / CMAF byte
//!   stream — the merged init segment followed by moof+mdat fragments in
//!   timestamp order across tracks.
//! - [`Mkv`] does the same but yields a Matroska / WebM byte stream — EBML
//!   header + unknown-size Segment + Cluster fragments.
//! - [`Avc1`] / [`Hvc1`] are codec-shape transmuxers used by the container
//!   exporters: they convert Annex-B sources (Avc3/Hev1) into length-prefixed
//!   samples + out-of-band avcC/hvcC, and pass through Avc1/Hvc1 sources
//!   unchanged.

mod avc1;
mod fmp4;
mod hvc1;
mod mkv;

pub use avc1::Avc1;
pub use fmp4::Fmp4;
pub use hvc1::Hvc1;
pub use mkv::Mkv;

#[cfg(test)]
mod test;
