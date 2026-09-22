// Builds the conformance corpus described in /concept/audio-jitter.
//
// Run `bun doc/concept/audio-jitter/corpus.ts` to rewrite the JSON; `corpus.test.ts`
// fails when the checked-in files no longer match, so the corpus cannot be edited by
// hand to make a failing implementation pass.

import { writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { Estimator } from "./reference";

/** One arrival trace and the target series every implementation must produce from it. */
export interface Case {
	name: string;
	/** What the trace is shaped like, and what it is holding the implementation to. */
	about: string;
	/** The codec frame duration, in milliseconds. */
	frame: number;
	/** The publisher's advertised flush span, when the catalog carries one. */
	advertised?: number;
	/** An explicit target that fixes the buffer and disables adaptation. */
	delay?: number;
	/** The receiver's monotonic clock per frame, in delivery order. */
	arrival: number[];
	/** Each frame's container timestamp in milliseconds, in the same order. */
	media: number[];
	/** `[frame index, target]` wherever the target differs from the previous frame's. */
	target: [number, number][];
}

type Trace = { arrival: number[]; media: number[] };

/** A sender emitting one frame per `frame` ms, every run of `run` flushed together. */
function flushed(frames: number, frame: number, run: number, base: number): Trace {
	const arrival: number[] = [];
	const media: number[] = [];
	for (let i = 0; i < frames; i++) {
		const last = Math.floor(i / run) * run + run - 1;
		arrival.push(base + last * frame);
		media.push(i * frame);
	}
	return { arrival, media };
}

/** Delay every frame in `[from, from + count)` by a ramp climbing `step` ms per frame. */
function ramp(trace: Trace, from: number, count: number, step: number): Trace {
	const arrival = [...trace.arrival];
	for (let i = from; i < arrival.length; i++) {
		arrival[i] += step * Math.min(i - from + 1, count);
	}
	return { arrival, media: trace.media };
}

/** Deliver every `period`-th run of `run` frames after the run that follows it. */
function reordered(trace: Trace, run: number, period: number): Trace {
	const arrival: number[] = [];
	const media: number[] = [];
	const runs = Math.floor(trace.media.length / run);
	const order: number[] = [];
	for (let r = 0; r < runs; r++) order.push(r);
	for (let r = 0; r + 1 < runs; r += period) {
		[order[r], order[r + 1]] = [order[r + 1], order[r]];
	}
	// Arrival stays monotonic because delivery order is what the receiver sees; only
	// the media timestamps come back out of order.
	let now = trace.arrival[0];
	for (const r of order) {
		for (let k = 0; k < run; k++) {
			const i = r * run + k;
			arrival.push(now);
			media.push(trace.media[i]);
			now += trace.arrival[1] - trace.arrival[0];
		}
	}
	return { arrival, media };
}

/** Splice a gap into both clocks, as a suspended tab or a paused publisher would. */
function gap(trace: Trace, at: number, length: number): Trace {
	const arrival = trace.arrival.map((v, i) => (i >= at ? v + length : v));
	const media = trace.media.map((v, i) => (i >= at ? v + length : v));
	return { arrival, media };
}

function paced(frames: number, frame: number, base: number): Trace {
	return flushed(frames, frame, 1, base);
}

/** Replay a trace through the reference and keep only the frames where the target moved. */
function run(trace: Trace, config: { frame: number; advertised?: number; delay?: number }): [number, number][] {
	if (config.delay !== undefined) return [[0, config.delay]];

	const estimator = new Estimator({ frame: config.frame, advertised: config.advertised });
	const target: [number, number][] = [];
	let previous: number | undefined;

	for (let i = 0; i < trace.arrival.length; i++) {
		estimator.observe({ arrival: trace.arrival[i], media: trace.media[i] });
		const value = estimator.target;
		if (value !== previous) target.push([i, value]);
		previous = value;
	}

	return target;
}

const FRAME = 20;

const traces: Omit<Case, "target">[] = [
	{
		name: "paced",
		about: "A perfectly even sender for 60 s, long enough to leave the cold-start ramp. The target settles on one bucket plus one frame, which is the floor the estimator can report.",
		frame: FRAME,
		...paced(3000, FRAME, 50),
	},
	{
		name: "flush",
		about: "A sender flushing 100 ms of media at a time, then 200 ms from halfway. The measured term follows the flush span less one frame, which is what the round-trip formula could not see.",
		frame: FRAME,
		...(() => {
			const first = flushed(500, FRAME, 5, 50);
			const second = flushed(500, FRAME, 10, 50);
			const offset = 500 * FRAME;
			return {
				arrival: [...first.arrival, ...second.arrival.map((v) => v + offset)],
				media: [...first.media, ...second.media.map((v) => v + offset)],
			};
		})(),
	},
	{
		name: "advertised",
		about: "The same even sender as `paced`, with a publisher advertising a 200 ms flush span. The advertised value is a floor on the measured term, not an addition to it, so the target holds there instead of converging down.",
		frame: FRAME,
		advertised: 200,
		...paced(1000, FRAME, 50),
	},
	{
		name: "buildup",
		about: "A queue filling at 2 ms per frame for 2 s, then holding. An inter-arrival estimator reads a hundred tiny observations; this reads the delay climbing against the fastest recent frame.",
		frame: FRAME,
		...ramp(paced(1500, FRAME, 50), 250, 100, 2),
	},
	{
		name: "reorder",
		about: "Every other run of five frames delivered after the run that follows it. The media-time gap between the runs must not land in the delay measurement.",
		frame: FRAME,
		...reordered(paced(1000, FRAME, 50), 5, 2),
	},
	{
		name: "idle",
		about: "A 30 s gap in both clocks, as a suspended tab produces. The idle intervals relax the histogram back toward the prior rather than freezing it.",
		frame: FRAME,
		...gap(paced(1000, FRAME, 50), 500, 30_000),
	},
	{
		name: "tunein",
		about: "One frame from a stale group, then the live edge 14.5 s later, then 30 s of even audio. The #3517 branch reported 14.56 s here; the target must stay near the frame duration.",
		frame: FRAME,
		arrival: [
			0,
			10,
			...paced(1500, FRAME, 10)
				.arrival.slice(1)
				.map((v) => v),
		],
		media: [
			0,
			14_500,
			...paced(1500, FRAME, 10)
				.media.slice(1)
				.map((v) => v + 14_500),
		],
	},
	{
		name: "spike",
		about: "A 300 ms stall every 5 s for the first 10 s, then 30 s of clean arrivals. The target rises to cover the stalls, then decays at the forget factor once they stop.",
		frame: FRAME,
		...(() => {
			const trace = paced(2000, FRAME, 50);
			const arrival = [...trace.arrival];
			for (let start = 250; start <= 500; start += 250) {
				for (let i = start; i < start + 15; i++) arrival[i] = arrival[start + 14];
			}
			return { arrival, media: trace.media };
		})(),
	},
	{
		name: "fixed",
		about: "An explicit 250 ms target over the bursty trace. Adaptation is off: the target is what was asked for, whatever the arrivals say.",
		frame: FRAME,
		delay: 250,
		...flushed(500, FRAME, 5, 50),
	},
];

export const cases: Case[] = traces.map((trace) => ({ ...trace, target: run(trace, trace) }));

if (import.meta.main) {
	const here = dirname(fileURLToPath(import.meta.url));
	for (const entry of cases) {
		writeFileSync(join(here, `${entry.name}.json`), `${JSON.stringify(entry)}\n`);
	}
	console.log(`wrote ${cases.length} cases`);
}
