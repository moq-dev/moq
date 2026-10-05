import { describe, expect, test } from "bun:test";
import { analyze, type Input, percentile, SKIP_MS, skipAheads } from "./analyze.ts";
import type { Sample, Shaper } from "./schema.ts";

const row = { runtime: "chromium", codec: "opus", rate: 48000, profile: "mild", ring: "plain" } as const;
const counters = { packets: 100, lost: 0, overflowed: 0, throttled: 0, delayed: 100, reordered: 0 };
const shaper: Shaper = { seed: 7, status: 0, up: counters, down: counters };

/** A clean run: 40 s of samples, the render clock 1000 ms behind the viewer's, the playhead 200 ms behind. */
function clean(): Sample[] {
	return Array.from({ length: 160 }, (_, i) => {
		const at = 1000 + i * 250;
		return {
			at,
			render: at - 1000,
			timestamp: at - 200,
			stalled: false,
			delay: 120,
			quanta: i * 94,
			quiet: 0,
		};
	});
}

const input = (samples: Sample[], extra: Partial<Input> = {}): Input => ({
	row,
	samples,
	environment: { crossOriginIsolated: false, transport: "webtransport" },
	voids: [],
	notes: [],
	shaper,
	warmupMs: 5000,
	...extra,
});

describe("skipAheads", () => {
	test("the quantization band is not a skip", () => {
		const lags = Array.from({ length: 40 }, (_, i) => 200 + (i % 2 === 0 ? 25 : -25));
		expect(skipAheads(lags).count).toBe(0);
	});

	test("a step in the lag is one skip, of the step's size", () => {
		const lags = Array.from({ length: 40 }, (_, i) => (i < 20 ? 300 : 200));
		expect(skipAheads(lags)).toEqual({ count: 1, ms: 100 });
	});

	test("a step at the threshold is not one", () => {
		const lags = Array.from({ length: 40 }, (_, i) => (i < 20 ? 200 + SKIP_MS : 200));
		expect(skipAheads(lags).count).toBe(0);
	});
});

test("percentile is nearest rank", () => {
	expect(percentile([5, 1, 4, 2, 3], 50)).toBe(3);
	expect(percentile([1, 2, 3, 4, 5, 6, 7, 8, 9, 10], 95)).toBe(10);
	expect(percentile([], 50)).toBeNull();
});

describe("analyze", () => {
	test("a clean run has nothing to report and nothing void", () => {
		const summary = analyze(input(clean()));
		expect(summary.voids).toEqual([]);
		expect(summary.metrics).toMatchObject({
			underruns_total: 0,
			skip_aheads_total: 0,
			stalled_share: 0,
			silence_share: 0,
			target_ms_p95: 120,
			converge_ms_last: 0,
		});
		// Every key in the schema is present, measured or null.
		expect(summary.metrics.render_load_max).toBeNull();
		expect(summary.drift.find((d) => d.clock === "render")).toEqual({ clock: "render", rate: 0, offset: 1000 });
	});

	test("a gap on the render clock is counted where it lands on the viewer's", () => {
		const samples = clean();
		// 20 s into the viewer clock is 19 s on the render clock.
		samples[100] = { ...samples[100], gaps: [{ at: 19_000, ms: 8, quanta: 3, short: 1 }] } as Sample;
		// Inside the warmup, so not graded.
		samples[10] = { ...samples[10], gaps: [{ at: 1_000, ms: 4, quanta: 2, short: 1 }] } as Sample;
		const summary = analyze(input(samples));
		expect(summary.metrics).toMatchObject({
			underruns_total: 3,
			short_quanta_total: 1,
			underrun_episodes_total: 1,
			underrun_ms_total: 8,
			underrun_ms_max: 8,
		});
	});

	test("a gap that is the ring stalling is not an underrun", () => {
		const samples = clean();
		samples[100] = {
			...samples[100],
			gaps: [{ at: 19_000, ms: 300, quanta: 110, short: 0 }],
			stalls: [
				{ at: 19_020, stalled: true },
				{ at: 19_300, stalled: false },
			],
		} as Sample;
		expect(analyze(input(samples)).metrics.underruns_total).toBe(0);
	});

	test("a re-anchor that discards audio is a skip-ahead", () => {
		const samples = clean().map((s, i) => (i >= 80 ? { ...s, timestamp: s.at - 100 } : s));
		expect(analyze(input(samples)).metrics).toMatchObject({ skip_aheads_total: 1, discarded_ms_total: 100 });
	});

	test("a shaper that failed or never carried the page voids the row", () => {
		const failed = analyze(input(clean(), { shaper: { ...shaper, status: 1 } }));
		expect(failed.voids.map((v) => v.assertion)).toEqual(["shaper"]);
		const idle = analyze(input(clean(), { shaper: { ...shaper, down: { ...counters, packets: 0 } } }));
		expect(idle.voids.map((v) => v.assertion)).toEqual(["shaper"]);
	});

	test("a render clock that is not running at wall rate voids the row", () => {
		const samples = clean().map((s) => ({ ...s, render: (s.at - 1000) * 0.5 }));
		expect(analyze(input(samples)).voids.map((v) => v.assertion)).toEqual(["clock"]);
	});

	test("no audio at all voids the window", () => {
		const samples = clean().map((s) => ({ ...s, timestamp: undefined, stalled: true }));
		expect(analyze(input(samples)).voids.map((v) => v.assertion)).toContain("window");
	});

	test("convergence is the last move, not the first plateau", () => {
		const samples = clean().map((s, i) => ({ ...s, delay: i < 40 ? 100 : i < 60 ? 300 : 100 }));
		// First audio at sample 0, the target back on its final value from sample 60.
		expect(analyze(input(samples)).metrics.converge_ms_last).toBe(60 * 250);
	});
});
