//! Import media into a moq broadcast.
//!
//! The importers split by multiplicity: [`Track`] / [`TrackStream`] publish a
//! single codec onto one MoQ track, while [`Container`] decodes a container
//! that may publish more than one track. A single codec splits again by frame
//! boundaries: [`Track`] takes whole frames (the typical case for files and
//! reassembled network input), while [`TrackStream`] infers them from a raw
//! byte stream (piped Annex-B H.264, ...). A container recovers its own
//! framing, so [`Container`] takes chunks split anywhere.
//!
//! Each importer takes a typed format (parsed from e.g. `"avc3"` or `"fmp4"`)
//! and errors on a format it doesn't handle; `TrackStream` accepts only the
//! self-delimiting formats. The concrete importers live with their format
//! under [`crate::container`] or [`crate::codec`] and own their
//! catalog rendition through their container producer.
//!
//! A single-codec importer takes a track the caller mints, typically
//! `broadcast.unique_track(suffix, catalog.track_info())`, so the retention the catalog
//! declares reaches the track.

mod container;
mod format;
mod init;
mod track;

pub use container::*;
pub use format::{AudioFormat, ContainerFormat, Format, Kind, VideoFormat};
pub use init::{AudioInit, ContainerInit, VideoInit};
pub use track::*;
