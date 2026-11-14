// How a producer places a write's timestamp on its track: a timed track's groups take the next
// sequence, while a scale-less track's frames carry a group's key, which names its group.
import * as Moq from "@moq/net";

/**
 * Whether a group whose first frame is at `at` would start past every group on `track`, which a
 * new scale-less group must: two groups never share a key. A timed group takes the next sequence,
 * so it always does. Mirrors the Rust `starts_past`.
 */
export function startsPast(track: Moq.Track.Producer, at: Moq.Time.Timestamp): boolean {
	if (at.scale !== undefined) return true;
	const largest = track.largest();
	return largest === undefined || BigInt(at.value) > largest.group;
}

/**
 * Open a group for a write at `at`: the next sequence on a timed track, or on a scale-less one the
 * group `at` names, which must pass every group the track holds ({@link Moq.Error.TimestampMismatch}
 * otherwise). The sequences it passes are skipped, as `appendGroup` skips them.
 */
export function openGroup(track: Moq.Track.Producer, at: Moq.Time.Timestamp): Moq.Group.Producer {
	if (at.scale !== undefined) return track.appendGroup();
	if (!startsPast(track, at)) throw new Moq.Error.TimestampMismatch();
	const key = BigInt(at.value);
	const largest = track.largest();
	if (largest !== undefined && largest.group + 1n < key) track.skipGroups(largest.group + 1n, key);
	return track.createGroup(key);
}

/**
 * `at` for a frame of `group`: a scale-less track's frames carry their group's key, so a frame
 * appended to an open group carries that group's key, whatever key the caller named. Mirrors the
 * Rust `frame_time`.
 */
export function frameTime(group: Moq.Group.Producer, at: Moq.Time.Timestamp): Moq.Time.Timestamp {
	return at.scale === undefined ? Moq.Time.Timestamp.unscaled(group.sequence) : at;
}
