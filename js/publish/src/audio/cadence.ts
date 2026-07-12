/**
 * Frame-cadence timestamp snapping for the audio encoder. Kept in its own module (free of WebCodecs) so
 * it can be unit tested directly.
 *
 * @module
 */

/**
 * Snap an encoded frame's timestamp onto a running nominal cadence.
 *
 * When `actual` is within half a frame of the `expected` next slot it is pinned to that slot (returned
 * rounded to whole microseconds for the container) and the cadence advances by `nominal`; a larger jump (a
 * real gap: mute, DTX silence, suspend) re-anchors to `actual`. This removes the per-frame timestamp jitter
 * Safari's AudioEncoder introduces by stamping each output frame with the timestamp of the input AudioData
 * chunk that held its start, quantizing to the 128-sample capture-quantum grid (a 20 ms Opus frame
 * alternates 18667/21333 us). It is an identity rewrite on an already-exact cadence (Chrome/Firefox).
 *
 * @param expected - the cadence clock's next expected timestamp (us), or undefined to anchor here
 * @param actual - the encoder's reported timestamp (us) for this frame
 * @param nominal - the nominal frame duration (us), how far the cadence advances per frame
 * @param window - the snap tolerance (us): pin to `expected` within it, re-anchor beyond it. The caller
 *   sizes this to absorb one capture quantum of jitter while staying under one frame (a real gap)
 * @returns the container timestamp to emit and the next cadence value to carry forward
 */
export function snapCadence(
	expected: number | undefined,
	actual: number,
	nominal: number,
	window: number,
): { ts: number; next: number } {
	if (expected !== undefined && Math.abs(actual - expected) < window) {
		return { ts: Math.round(expected), next: expected + nominal };
	}
	return { ts: actual, next: actual + nominal };
}
