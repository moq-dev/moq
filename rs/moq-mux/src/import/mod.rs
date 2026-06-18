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
//! [`unique_track`] mints a track for the single-codec importers.

mod container;
mod track;

pub use container::*;
pub use track::*;

/// Mint a fresh unique track for a legacy single-codec importer.
///
/// Picks a unique name from `suffix` and sets the microsecond
/// [`hang::container::TIMESCALE`] that the legacy importers stamp their frames
/// with, so the relay gets timing without parsing the payload. Hand the result to
/// the importer's `new`.
pub fn unique_track(broadcast: &mut moq_net::BroadcastProducer, suffix: &str) -> crate::Result<moq_net::TrackProducer> {
	let info = moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE);
	Ok(broadcast.unique_track(suffix, info)?)
}
