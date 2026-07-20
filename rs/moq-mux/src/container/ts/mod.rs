//! MPEG-TS (transport stream).
//!
//! An interchange format only, not a wire format: [`Import`] demuxes a TS byte
//! stream into a broadcast and [`Export`] muxes a broadcast back into TS. The
//! codec layer (H.264/H.265/AAC, plus the legacy MP2/AC-3/E-AC-3 parsers) does
//! the elementary-stream parsing; this module only handles PAT/PMT/PES framing,
//! PTS, and ADTS framing for AAC.
//!
//! Elementary streams we don't decode (SCTE-35, teletext, DVB subtitles, private
//! data, ...) are carried verbatim, one MoQ track per PID, described in the
//! [`Mpegts`] catalog section. SCTE-35 is just one such stream (`stream_type` 0x86).

mod adts;
mod export;
mod import;

// The `mpegts` catalog section (per-track PID + descriptors plus verbatim carriage
// of undecoded elementary streams) and the `Catalog` capability, re-exported flat so
// they read as `ts::Catalog`, `ts::Mpegts`, ... instead of stuttering under `catalog`.
mod catalog;

pub use catalog::{Catalog, Descriptor, Ext, Framing, Mpegts, Track, Verbatim};
pub use export::*;
pub use import::*;

#[cfg(test)]
mod export_test;
#[cfg(test)]
mod import_test;
