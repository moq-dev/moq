/** Checks that a stats scenario window left every connection doing its work, so doing less cannot read as cheaper. */
import type { Outcome, Sampling } from "./page/stats.ts";

/** A sampling shape; `absent` deletes WebTransport getStats(), as shipping Chromium lacks it. */
export type SamplingShape = Omit<Sampling, "url" | "path"> & { absent?: boolean };

/**
 * Reservations that must hold a send-rate slice at the end of the window.
 *
 * Each one on a WebTransport with getStats() does; none can over qmux or without getStats().
 * Identical pooled handles share one connection, so only its reservations count.
 */
export function expectedEstimates(shape: SamplingShape): number {
	if (shape.mode === "qmux" || shape.absent) return 0;
	const distinct = shape.mode === "pooled" ? 1 : shape.connections;
	return distinct * shape.reservations;
}

/** Why the window does not count as equivalent work, or `undefined` when it does. */
export function samplingFailure(shape: SamplingShape, before: Outcome, after: Outcome): string | undefined {
	if (after.connected !== shape.connections) return `${after.connected} of ${shape.connections} connected`;
	const expected = expectedEstimates(shape);
	if (after.estimated !== expected) return `${after.estimated} of ${expected} expected estimates`;
	// Every playing connection reads frames throughout the window.
	if (shape.mode === "receive" && after.received - before.received < shape.connections)
		return `received ${after.received - before.received} frames in the window`;
	return undefined;
}

/** Why the resume cycles failed, or `undefined` when every cycle got its first slice. */
export function resumeFailure(waits: number[]): string | undefined {
	const missing = waits.filter((wait) => !Number.isFinite(wait)).length;
	if (missing) return `${missing} of ${waits.length} cycles never got a send-rate slice`;
	return undefined;
}
