//! Subscribe to a moq broadcast and decode media frames.
//!
//! [`Fmp4`] subscribes to a broadcast, decodes every track via
//! [`Consumer<Hang>`](crate::container::Consumer), and yields a single fMP4 / CMAF byte
//! stream — the merged init segment followed by moof+mdat fragments in
//! timestamp order across tracks.

mod fmp4;
mod mkv;

pub use fmp4::*;
pub use mkv::*;

#[cfg(test)]
#[path = "mkv_test.rs"]
mod mkv_test;
