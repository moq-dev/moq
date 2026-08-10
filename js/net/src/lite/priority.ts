/**
 * Stream prioritization for moq-lite, mirroring the Rust `lite::priority` queue.
 *
 * @module
 */

// The transport compares send orders as a single number, so the two ranks are packed into
// disjoint ranges: the track priority takes the high bits so it always dominates, and the
// group sequence breaks ties below it. 2^32 is wide enough for any real sequence while
// keeping the product exact as a double (255 * 2^32 is well under 2^53).
const GROUP_SPAN = 2 ** 32;

/** The highest track priority the wire carries (a u8). */
const MAX_PRIORITY = 0xff;

/**
 * The transport send order for a group, where HIGHER values are transmitted first.
 *
 * A higher `priority` (the subscriber's track priority) always wins; a higher `sequence`
 * only breaks ties within the same track, so a newer group preempts an older one.
 */
export function sendOrder(priority: number, sequence: number): number {
	return clamp(priority, MAX_PRIORITY) * GROUP_SPAN + clamp(sequence, GROUP_SPAN - 1);
}

// Both ranks are unsigned and must stay inside their range, or a large sequence would
// bleed into the track's bits and outrank a higher-priority track.
function clamp(value: number, max: number): number {
	return Math.min(Math.max(Math.trunc(value), 0), max);
}
