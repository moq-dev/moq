//! Import media into a moq broadcast.
//!
//! The importers split along two axes. By multiplicity: [`Track`] / [`TrackStream`]
//! publish a single codec onto one MoQ track, while [`Container`] /
//! [`ContainerStream`] decode a container that may publish more than one track. By
//! frame boundaries: [`Track`] / [`Container`] take whole frames or chunks (the
//! typical case for files and reassembled network input), while [`TrackStream`] /
//! [`ContainerStream`] infer boundaries from a raw byte stream (piped Annex-B
//! H.264, an fMP4 reader, …).
//!
//! Each importer's `new` takes a format string (e.g. `"avc3"`, `"fmp4"`) and
//! errors on a format it doesn't handle — `TrackStream` / `ContainerStream`
//! accept only the self-delimiting formats. The concrete importers live with
//! their format under [`crate::container`] or [`crate::codec`] and publish their
//! own catalog rendition (see [`crate::catalog::VideoTrack`] /
//! [`crate::catalog::AudioTrack`]).
//!
//! A single-codec importer takes a track the caller mints, typically
//! `broadcast.unique_track(suffix, catalog.track_info())`, so the retention the catalog
//! declares reaches the track.

mod container;
mod init;
mod track;

pub use container::*;
pub use init::Init;
pub use track::*;

#[doc(hidden)]
#[deprecated(
	note = "call broadcast.unique_track(suffix, info) directly, with the catalog's track_info() when one is in scope so a declared retention reaches the track"
)]
pub fn unique_track(
	broadcast: &mut moq_net::broadcast::Producer,
	suffix: &str,
) -> crate::Result<moq_net::track::Producer> {
	Ok(broadcast.unique_track(suffix, hang::container::track_info())?)
}
