import { expect, test } from "bun:test";
import type { Outcome } from "./page/stats.ts";
import { expectedEstimates, resumeFailure, type SamplingShape, samplingFailure } from "./sampling.ts";

const outcome = (estimated: number, connected = 4, received = 0): Outcome => ({
	connected,
	estimated,
	received,
	transport: "webtransport",
});

test("only reservations over a WebTransport with getStats() expect a slice", () => {
	expect(expectedEstimates({ mode: "private", connections: 4, reservations: 4 })).toBe(16);
	expect(expectedEstimates({ mode: "pooled", connections: 4, reservations: 1 })).toBe(1);
	expect(expectedEstimates({ mode: "private", connections: 4, reservations: 0 })).toBe(0);
	expect(expectedEstimates({ mode: "qmux", connections: 4, reservations: 1 })).toBe(0);
	expect(expectedEstimates({ mode: "private", connections: 4, reservations: 1, absent: true })).toBe(0);
});

test("a window where sampling stopped fails rather than reading as cheaper", () => {
	const shape: SamplingShape = { mode: "private", connections: 4, reservations: 1 };
	expect(samplingFailure(shape, outcome(0), outcome(4))).toBeUndefined();
	expect(samplingFailure(shape, outcome(0), outcome(0))).toBe("0 of 4 expected estimates");
	expect(samplingFailure(shape, outcome(0), outcome(3))).toBe("3 of 4 expected estimates");
});

test("an estimate where none can exist fails", () => {
	const shape: SamplingShape = { mode: "qmux", connections: 4, reservations: 1 };
	expect(samplingFailure(shape, outcome(0), outcome(0))).toBeUndefined();
	expect(samplingFailure(shape, outcome(0), outcome(1))).toBe("1 of 0 expected estimates");
});

test("lost connections and stalled playback fail", () => {
	expect(samplingFailure({ mode: "private", connections: 4, reservations: 0 }, outcome(0), outcome(0, 3))).toBe(
		"3 of 4 connected",
	);
	const receive: SamplingShape = { mode: "receive", connections: 4, reservations: 0 };
	expect(samplingFailure(receive, outcome(0, 4, 10), outcome(0, 4, 13))).toBe("received 3 frames in the window");
	expect(samplingFailure(receive, outcome(0, 4, 10), outcome(0, 4, 70))).toBeUndefined();
});

test("a resume cycle that never got its slice fails", () => {
	expect(resumeFailure([0.4, 0.5, 0.6])).toBeUndefined();
	expect(resumeFailure([0.4, Number.NaN, Number.NaN])).toBe("2 of 3 cycles never got a send-rate slice");
});
