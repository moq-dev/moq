// How a producer places a write's timestamp on its track: a group starts at its first frame's
// time in the track's timescale, or on a scale-less track at the key its unscaled timestamp names.
import * as Moq from "@moq/net";

// Each track's timescale once its info resolved, `null` for a scale-less track.
const scales = new WeakMap<Moq.Track.Producer, Moq.Time.Timescale | null>();

// The track's timescale, `null` when scale-less, or undefined until its info resolves, which this
// starts reading.
function scaleOf(track: Moq.Track.Producer): Moq.Time.Timescale | null | undefined {
	if (scales.has(track)) return scales.get(track);
	void track.info().then(
		(info) => scales.set(track, info.timescale),
		() => {},
	);
	return undefined;
}

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
		const scale = scaleOf(track);
		if (scale === undefined) return false;
		// A scaled timestamp on a scale-less track is refused where the group opens.
		if (scale === null) return true;
		key = BigInt(Moq.Time.floorAt(at, scale));
	}
	return key > largest.group;
}

/**
 * Open a group for a write at `at`, starting there: past every group the track holds
 * ({@link Moq.Error.TimestampMismatch} otherwise). The starts it passes are skipped, as
 * `appendGroup` skips them.
 */
export function openGroup(track: Moq.Track.Producer, at: Moq.Time.Timestamp): Moq.Group.Producer {
	scaleOf(track);
	return track.appendGroup(at);
}

/**
 * `at` for a frame of `group`: a scale-less track's frames carry their group's key, so a frame
 * appended to an open group carries that group's key, whatever key the caller named. Mirrors the
 * Rust `frame_time`.
 */
export function frameTime(group: Moq.Group.Producer, at: Moq.Time.Timestamp): Moq.Time.Timestamp {
	return at.scale === undefined ? Moq.Time.Timestamp.unscaled(group.start) : at;
}
