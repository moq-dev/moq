/**
 * A value paired with its frame's timestamp.
 *
 * @module
 */
import type { Timestamp } from "./time.ts";

/**
 * A value read from a track, and its frame's timestamp.
 *
 * Writes take the same pair as two arguments, `(value, at)`. On a timed track `at` is when the
 * value was captured. A scale-less track has no time, so there it is {@link Timestamp.unscaled}
 * at a group key: a write that opens a group starts it at that key, which must pass every group
 * the track holds.
 */
export interface Timed<T> {
	/** The value. */
	value: T;
	/** The frame's timestamp: its capture time, or its group key on a scale-less track. */
	at: Timestamp;
}
