import { describe, expect, test } from "bun:test";
import { analyze, type Input, percentile, recovery } from "./analyze.ts";
import type { Sample, Shaper } from "./schema.ts";

const row = { runtime: "chromium", codec: "opus", rate: 48000, profile: "mild" } as const;
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
	environment: { transport: "webtransport" },
	voids: [],
	notes: [],
	shaper,
	warmupMs: 5000,
	...extra,
});

test("recovery runs from a disturbance to audio back within a frame of where it rested before it", () => {
	const at = (lags: (number | "stalled")[]) =>
		lags.map((lag, i) => ({
			at: i * 250,
			lag: lag === "stalled" ? undefined : lag,
			stalled: lag === "stalled",
		}));
	// Resting 30 ms behind, jittering within a frame, then a stall and a trail that recovers.
	const rest = [30, 40, 25, 35, 30, 30];
	expect(recovery(at([...rest, "stalled", 400, 200, 60, 45, 30]), 20)).toBe(1000);
	// Ahead of where it rested counts too, as after a target rise.
	expect(recovery(at([...rest, -70, -20, 15, 30]), 20)).toBe(500);
	// Settling somewhere new never returns: it lasts to the last sample.
	expect(recovery(at([...rest, 200, 200, 200]), 20)).toBe(500);
	expect(recovery(at(rest), 20)).toBe(0);
	// Nothing before the first sample to rest on.
	expect(recovery(at(["stalled"]), 20)).toBeNull();
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
			stalled_share: 0,
			silence_share: 0,
			target_ms_p95: 120,
			converge_ms_last: 0,
		});
		// Every key in the schema is present, measured or null.
		expect(summary.metrics.render_load_max).toBeNull();
		expect(summary.drift.find((d) => d.clock === "render")).toEqual({ clock: "render", rate: 0, offset: 1000 });
	});

	test("a render clock that only starts with the audio is not void", () => {
		// A native device sits unpulled for the first two seconds of its stream, before first audio.
		const startup = Array.from({ length: 8 }, (_, i) => ({ at: i * 125, render: 0, stalled: true }));
		const summary = analyze(input([...startup, ...clean()]));
		expect(summary.voids).toEqual([]);
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

	test("the player's own underruns are its counter's rise across the window", () => {
		// Three counted by the end of the warmup, then three more inside the window.
		const samples = clean().map((s, i) => ({ ...s, playerUnderruns: i < 100 ? 3 : 6 }));
		expect(analyze(input(samples)).metrics).toMatchObject({ player_underruns_total: 3 });
		expect(analyze(input(clean())).metrics.player_underruns_total).toBeNull();
	});

	test("the player's own skips are its counter's rise across the window", () => {
		const samples = clean().map((s, i) => ({ ...s, playerSkips: i < 100 ? 1 : 3 }));
		expect(analyze(input(samples)).metrics).toMatchObject({ player_skips_total: 2 });
		expect(analyze(input(clean())).metrics.player_skips_total).toBeNull();
	});

	test("the A/V lag is how far the playhead trails the shared clock while playing", () => {
		// Three in four samples 20 ms ahead of the clock, the rest 10 ms; a stalled sample's playhead is
		// frozen, so it is left out however far the clock has run.
		const samples = clean().map((s, i) => {
			const timestamp = s.timestamp ?? 0;
			if (i === 50) return { ...s, stalled: true, clock: timestamp + 500 };
			return { ...s, clock: timestamp - 20 + (i % 4 === 3 ? 10 : 0) };
		});
		expect(analyze(input(samples)).metrics).toMatchObject({ av_lag_ms_p50: -20, av_lag_ms_p95: -10 });
		expect(analyze(input(clean())).metrics.av_lag_ms_p50).toBeNull();
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

	test("recovery is the longest audio spent off where it rested before a disturbance", () => {
		// At rest 20 ms ahead of the clock; samples 80 to 83 stall, and audio then trails until 91.
		const samples = clean().map((s, i) => {
			const timestamp = s.timestamp ?? 0;
			if (i >= 80 && i < 84) return { ...s, stalled: true, clock: timestamp + 500 };
			const lag = i >= 84 && i < 92 ? 300 - (i - 84) * 30 : -20;
			return { ...s, clock: timestamp + lag };
		});
		// 300 down to 90 ms trail by more than a frame past rest; 92 is back.
		expect(analyze(input(samples)).metrics).toMatchObject({ av_lag_ms_p50: -20, recover_ms_max: 12 * 250 });
		expect(analyze(input(clean())).metrics.recover_ms_max).toBeNull();
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
