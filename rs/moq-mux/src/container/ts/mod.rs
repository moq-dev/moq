//! MPEG-TS (transport stream).
//!
//! An interchange format only, not a wire format: [`Import`] demuxes a TS byte
//! stream into a broadcast and [`Export`] muxes a broadcast back into TS. The
//! codec layer (H.264/H.265/AAC) does the elementary-stream parsing; this module
//! only handles PAT/PMT/PES framing, PTS, and ADTS framing for AAC.

mod adts;
mod export;
mod import;

pub use export::*;
pub use import::*;

#[cfg(test)]
mod export_test;
#[cfg(test)]
mod import_test;
