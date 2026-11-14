// Scale-less frames for tests: a scale-less track has no time, so each frame's timestamp is its
// group's key.
import type * as Group from "./group.ts";
import { Timestamp } from "./time.ts";
import type * as Track from "./track.ts";

/** Append a group to the scale-less `track` and write `payload` as its first frame, leaving it open. */
export function appendFrame(track: Track.Producer, payload: Uint8Array): Group.Producer {
	const group = track.appendGroup();
	group.writeFrame({ payload, timestamp: Timestamp.unscaled(group.sequence) });
	return group;
}
