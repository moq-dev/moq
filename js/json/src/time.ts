// How a producer places a write's timestamp on its track: a group starts at its first frame's
// time in the track's timescale, or on a scale-less track at the key its unscaled timestamp names.
import * as Moq from "@moq/net";

/**
 * Whether a group whose first frame is at `at` would start past every group on `track`, which a
 * new group must: two groups never share a start. A timed group starts at its first frame's time
 * in the track's timescale, and a scale-less one at the key its unscaled timestamp names. False
 * while the track's timescale is unknown. Mirrors the Rust `starts_past`.
 */
export function startsPast(track: Moq.Track.Producer, at: Moq.Time.Timestamp): boolean {
	const largest = track.largest();
	if (largest === undefined) return true;
	let key = at.key;
	if (key === undefined) {
		const scale = track.timescale;
		if (scale === undefined) return false;
		// A scaled timestamp on a scale-less track is refused where the group opens.
		if (scale === null) return true;
		key = BigInt(Moq.Time.floorAt(at, scale));
	}
	return key > largest.group;
}

/**
 * `at`, or one unit of the track's timescale past its newest start when `at` doesn't pass that: a
 * write stamped from a clock, such as a catalog or a chat message, can land twice in one unit, and
 * two groups never share a start. Compared and minted in the track's timescale; `at` as given while
 * that is unknown or `at` is a scale-less key. Mirrors the Rust `moq_mux::start::next`.
 */
export function next(track: Moq.Track.Producer, at: Moq.Time.Timestamp): Moq.Time.Timestamp {
	const largest = track.largest();
	const scale = track.timescale;
	if (largest === undefined || !scale || at.scale === undefined) return at;
	if (BigInt(Moq.Time.floorAt(at, scale)) > largest.group) return at;
	return new Moq.Time.Timestamp(Number(largest.group + 1n), scale);
}

/**
 * `at` for a frame of `group`: a scale-less track's frames carry their group's key, so a frame
 * appended to an open group carries that group's key, whatever key the caller named. Mirrors the
 * Rust `frame_time`.
 */
export function frameTime(group: Moq.Group.Producer, at: Moq.Time.Timestamp): Moq.Time.Timestamp {
	return at.scale === undefined ? Moq.Time.Timestamp.unscaled(group.start) : at;
}
