// Builds the conformance corpus described in /concept/audio-jitter.
//
// Run `bun doc/concept/audio-jitter/corpus.ts` to rewrite the JSON; `corpus.test.ts`
// fails when the checked-in files no longer match, so the corpus cannot be edited by
// hand to make a failing implementation pass.

import { readFileSync, writeFileSync } from "node:fs";
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
	/** Indices of the frames observed right after a declared end, in ascending order. */
	ends?: number[];
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

/**
 * Hold a FIFO queue to one frame per `service` ms from `from` for `length` ms, as a link
 * collapsing behind a full queue does, then drain it at 2 ms per frame.
 */
function outage(trace: Trace, from: number, length: number, service: number): Trace {
	const arrival = [...trace.arrival];
	let free = Number.NEGATIVE_INFINITY;
	for (let i = 0; i < arrival.length; i++) {
		const start = Math.max(arrival[i], free);
		if (start >= from && start < from + length) free = start + service;
		else if (start > arrival[i]) free = start + 2;
		else free = start;
		arrival[i] = free;
	}
	return { arrival, media: trace.media };
}

function paced(frames: number, frame: number, base: number): Trace {
	return flushed(frames, frame, 1, base);
}

/** Replay a trace through the reference and keep only the frames where the target moved. */
function run(
	trace: Trace & { ends?: number[] },
	config: { frame: number; advertised?: number; delay?: number },
): [number, number][] {
	if (config.delay !== undefined) return [[0, config.delay]];

	const estimator = new Estimator({ frame: config.frame, advertised: config.advertised });
	const target: [number, number][] = [];
	let previous: number | undefined;

	const ends = new Set(trace.ends);
	for (let i = 0; i < trace.arrival.length; i++) {
		if (ends.has(i)) estimator.end();
		estimator.observe({ arrival: trace.arrival[i], media: trace.media[i] });
		const value = estimator.target;
		if (value !== previous) target.push([i, value]);
		previous = value;
	}

	return target;
}

const FRAME = 20;

/** The first `until` ms of a recorded replay trace under `test/audio-quality/traces`. */
function recorded(name: string, until: number): Trace {
	const here = dirname(fileURLToPath(import.meta.url));
	const path = join(here, "../../../test/audio-quality/traces", `${name}.json`);
	const { arrivals } = JSON.parse(readFileSync(path, "utf8")) as { arrivals: [number, number][] };
	const kept = arrivals.filter(([arrival]) => arrival < until);
	return { arrival: kept.map(([arrival]) => arrival), media: kept.map(([, media]) => media) };
}

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
		about: "Every other run of five frames delivered after the run that follows it, as newest-first delivery does to a sender's flush. Each held-back run is measured against the fastest frame before it, so the target covers waiting for it; the first one precedes every frame observed and is not measured.",
		frame: FRAME,
		...reordered(paced(1000, FRAME, 50), 5, 2),
	},
	{
		name: "backlog",
		about: "The live edge, then the 1.5 s backlog behind it delivered newest first, then 30 s of even audio, as a subscription's start arrives. The backlog precedes every frame observed, so it is not measured, and the target stays near the frame duration.",
		frame: FRAME,
		...(() => {
			const live = 75 * FRAME;
			const arrival = [50];
			const media = [live];
			for (let i = 1; i <= 75; i++) {
				arrival.push(50 + i * 0.25);
				media.push(live - i * FRAME);
			}
			const even = paced(1500, FRAME, 50 + FRAME);
			return {
				arrival: [...arrival, ...even.arrival],
				media: [...media, ...even.media.map((v) => v + live + FRAME)],
			};
		})(),
	},
	{
		name: "join",
		about: "The first 30 s of a real microphone's subscription through the public relay (`relay-mic-rtt40`). The relay delivers its backlog interleaved with the live edge, and the live frames produced meanwhile queue behind it, up to 760 ms late. Every interval holding a frame due before the backlog finished arriving measured the join, not the path, so it folds nothing: the target leaves the prior for the path's own 60 to 100 ms within seconds, instead of holding 780 ms for a 20 s half-life.",
		frame: FRAME,
		...recorded("relay-mic-rtt40", 30_000),
	},
	{
		name: "idle",
		about: "A 30 s gap in both clocks, as a suspended tab produces. The idle intervals relax the histogram back toward the prior rather than freezing it.",
		frame: FRAME,
		...gap(paced(1000, FRAME, 50), 500, 30_000),
	},
	{
		name: "silence",
		about: "Even audio, a declared end and 60 s of silence, 5 s more, a declared 100 ms pause inside one interval, 5 s more, then an undeclared 30 s gap. Unlike `idle`, a declared silence is not evidence, so the target holds through both; the undeclared gap still relaxes it, since an end covers only the silence it declares.",
		frame: FRAME,
		...gap(gap(gap(paced(1500, FRAME, 50), 500, 60_000), 760, 100), 1000, 30_000),
		ends: [500, 760],
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
		name: "outage",
		about: "Even audio with a 1.5 s outage 10 s in: a queue serving one frame per 300 ms, then draining the backlog, so its delays climb past a second across four intervals. The frames stuck behind it are one stall, which folds once, so the one-off leaves the target where it was. So does a 1.5 s blackout 20 s in, recovering with the live edge before its backlog, whose empty intervals belong to the stall that ends them. Three more outages 3 s apart from 30 s still raise it, and it decays once they stop.",
		frame: FRAME,
		...(() => {
			let trace = paced(3000, FRAME, 50);
			trace = outage(trace, 10_050, 1500, 300);
			trace = outage(trace, 20_050, 1500, 1500);
			// The blackout recovers with the live edge first, then the backlog behind it.
			const arrival = [...trace.arrival];
			const media = [...trace.media];
			const [edge] = media.splice(1075, 1);
			arrival.splice(1075, 1);
			media.splice(1000, 0, edge);
			arrival.splice(1000, 0, arrival[1000]);
			trace = { arrival, media };
			for (const at of [30_000, 33_000, 36_000]) trace = outage(trace, at + 50, 1500, 300);
			return trace;
		})(),
	},
	{
		name: "batches",
		about: "A publisher flushing 1 s of media at once, every second, newest frame first. Each flush is due before the previous one finished arriving, so the flushes chain, but a stall covers at most a window of due time, so they still fold and the target rises to cover the flush span.",
		frame: FRAME,
		...(() => {
			const arrival: number[] = [];
			const media: number[] = [];
			for (let flush = 0; flush < 40; flush++) {
				for (let k = 0; k < 50; k++) {
					arrival.push(50 + (flush + 1) * 1000 + k * 2);
					media.push(flush * 1000 + (49 - k) * FRAME);
				}
			}
			return { arrival, media };
		})(),
	},
	{
		name: "straggler",
		about: "Even audio with a single 300 ms stall 8 s in, once the prior is spent. One interval of evidence is not enough to move the target, so the stall costs that one refill instead of raising the target and then dropping it, which skips buffered audio.",
		frame: FRAME,
		...(() => {
			const trace = paced(1500, FRAME, 50);
			const arrival = [...trace.arrival];
			for (let i = 400; i < 415; i++) arrival[i] = arrival[414];
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
