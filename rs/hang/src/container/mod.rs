/// How long a superseded group of a media track stays fresh: the media publisher's explicit
/// choice, which [`track_info`] declares on every media track.
///
/// Media is the one thing on a broadcast that is read as history rather than followed at the live
/// edge: a segmented egress (HLS/DASH) may only advertise segments a FETCH can still reach, a
/// standard player starts several target durations behind live, and a recorder absorbs
/// reordering. moq-net's default of zero suits a live-edge follower and would leave those
/// addressing groups that are already stale.
///
/// Declared per track rather than as moq-net's default, so the tracks that do not index history
/// keep the live edge alone: the catalog is snapshot mode and an HLS record track restates its
/// window in each new group, and their newest group never goes stale.
pub const MAX_AGE: std::time::Duration = std::time::Duration::from_secs(30);

mod frame;

pub use frame::*;
