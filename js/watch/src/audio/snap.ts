/**
 * Timestamp snapping for the audio render pipeline. Kept in its own module (free of the worklet blob
 * import in decoder.ts) so it can be unit tested directly.
 *
 * @module
 */

/**
 * Snap `actual` to `expected` when they are within `thresholdMicro`, else return `actual` unchanged.
 *
 * Used to make near-contiguous decoded frames write back-to-back in the watcher's timestamp-indexed ring
 * buffer, avoiding a zero-fill or overwrite of a sample every frame (Safari-to-Safari crackle). Genuine
 * gaps (at least one frame apart: packet loss, DTX silence, publisher restart) exceed the threshold and
 * pass through so the ring still zero-fills them.
 */
export function snapTimestamp(expected: number | undefined, actual: number, thresholdMicro: number): number {
	if (expected !== undefined && Math.abs(actual - expected) <= thresholdMicro) return expected;
	return actual;
}
