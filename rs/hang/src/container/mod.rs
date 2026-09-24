/// How long a media track asks its publisher (and, through `track_info`, every relay) to keep a
/// non-latest group fetchable.
///
/// Media is the one thing on a broadcast that is read as HISTORY rather than followed at the live
/// edge: a segmented egress (HLS/DASH) may only advertise segments a FETCH can still reach, and a
/// standard player starts several target durations behind live. `moq_net`'s conservative default
/// is sized for a live-edge follower and leaves such a player addressing groups that are already
/// gone.
///
/// Declared per track rather than by raising that default, so the tracks that do NOT index history
/// keep the cheap default: the catalog is snapshot mode and the timeline is a single never-rolled
/// group, and in both the useful value is the live edge, which is retained unconditionally.
pub const MAX_AGE: std::time::Duration = std::time::Duration::from_secs(30);

mod frame;

pub use frame::*;
