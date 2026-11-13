//! Where a group starts on a timed track: its first frame's timestamp in the track's timescale.
//!
//! Starts strictly increase, so a group has to start after every group the track holds.

/// The start of a group whose first frame is at `timestamp`, and the newest start on `track` when
/// one is held: `timestamp` in the track's timescale. Refused with
/// [`TimestampMismatch`](moq_net::Error::TimestampMismatch) on a scale-less track or past the
/// timescale's range.
fn of(
	track: &moq_net::track::Producer,
	timestamp: moq_net::Timestamp,
) -> crate::Result<(moq_net::Timestamp, Option<u64>)> {
	let Some(start) = track.info().rescale(Some(timestamp))? else {
		unreachable!("a timestamp rescales to one or is refused");
	};
	Ok((start, track.largest().map(|largest| largest.group)))
}

/// The start of a group stamped at `timestamp` on `track`, and the timestamp to stamp its first
/// frame with: `timestamp` in the track's timescale, or one unit past the newest start when it
/// doesn't pass that.
///
/// For writes stamped from a clock rather than media, such as a catalog or a table revision, which
/// can land twice in one unit of the track's timescale: the later one is stamped a unit later
/// rather than refused.
pub(crate) fn next(
	track: &moq_net::track::Producer,
	timestamp: moq_net::Timestamp,
) -> crate::Result<(u64, moq_net::Timestamp)> {
	match of(track, timestamp)? {
		(start, Some(newest)) if start.value() <= newest => {
			let bumped = newest.checked_add(1).ok_or(moq_net::Error::TimestampMismatch)?;
			let at = moq_net::Timestamp::new(bumped, start.scale()).map_err(|_| moq_net::Error::TimestampMismatch)?;
			Ok((bumped, at))
		}
		(start, _) => Ok((start.value(), timestamp)),
	}
}

/// [`next`] for a group stamped one unit of the track's timescale before `timestamp`, so a group
/// whose first frame is at `timestamp` can still follow it, but never below `floor`.
pub(crate) fn before(
	track: &moq_net::track::Producer,
	timestamp: moq_net::Timestamp,
	floor: moq_net::Timestamp,
) -> crate::Result<(u64, moq_net::Timestamp)> {
	let (start, _) = of(track, timestamp)?;
	let (floor, _) = of(track, floor)?;
	let below = start.value().saturating_sub(1).max(floor.value());
	let below = moq_net::Timestamp::new(below, start.scale()).map_err(|_| moq_net::Error::TimestampMismatch)?;
	next(track, below)
}

/// The start of a group whose first frame is at `timestamp` on `track`: `timestamp` in the track's
/// timescale, or `None` when that doesn't pass the newest start, such as a keyframe repeating the
/// last one's timestamp.
///
/// For groups keyed by media, whose timestamps a publisher can't move.
pub(crate) fn opens(track: &moq_net::track::Producer, timestamp: moq_net::Timestamp) -> crate::Result<Option<u64>> {
	let (start, newest) = of(track, timestamp)?;
	Ok(newest
		.is_none_or(|newest| start.value() > newest)
		.then_some(start.value()))
}

/// [`opens`], refusing a start that doesn't pass the newest with
/// [`TimestampRewind`](crate::container::TimestampRewind).
pub(crate) fn after(track: &moq_net::track::Producer, timestamp: moq_net::Timestamp) -> crate::Result<u64> {
	match of(track, timestamp)? {
		(start, Some(newest)) if start.value() <= newest => {
			let floor = moq_net::Timestamp::new(newest, start.scale()).unwrap_or(start);
			Err(crate::container::TimestampRewind { timestamp, floor }.into())
		}
		(start, _) => Ok(start.value()),
	}
}

/// Where a group ends whole on `track` when its content stops at `timestamp`: `timestamp` in the
/// track's timescale, or `None` when that doesn't pass `start` or won't convert.
pub(crate) fn end(track: &moq_net::track::Producer, start: u64, timestamp: moq_net::Timestamp) -> Option<u64> {
	let end = track.info().rescale(Some(timestamp)).ok().flatten()?.value();
	(end > start).then_some(end)
}
