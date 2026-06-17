//! Track-minting helper shared by the import dispatchers and containers.

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
