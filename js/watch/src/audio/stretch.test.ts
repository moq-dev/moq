import { expect, it } from "bun:test";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { type Decision, decide, HOLD, type Inputs, Stretch } from "./stretch";

interface Rule extends Inputs {
	name: string;
	rate: number;
	decision: Decision;
}

interface Case {
	name: string;
	rate: number;
	channels: number;
	input: number[];
	decisions: Decision[];
	shifts: number[];
	output: number[];
}

// Shared with moq-audio, so neither implementation is graded against the other.
const shared = join(dirname(fileURLToPath(import.meta.url)), "../../../../doc/concept/audio-jitter/stretch");
const rules = JSON.parse(readFileSync(join(shared, "decisions.json"), "utf8")) as Rule[];
const cases = JSON.parse(readFileSync(join(shared, "corpus.json"), "utf8")) as Case[];

// The corpus is interleaved; the ring is planar.
const planar = (samples: number[], channels: number) =>
	Array.from({ length: channels }, (_, channel) =>
		Float32Array.from(samples.filter((_, i) => i % channels === channel)),
	);

// Play `input` the way the ring does: topped up to a window before each hop boundary decides.
function play(
	stretch: Stretch,
	input: Float32Array[],
	decisions: Decision[],
): { output: number[][]; shifts: number[] } {
	const frames = input[0].length;
	const output: number[][] = input.map(() => []);
	const chunk = input.map(() => new Float32Array(stretch.hop));
	const shifts: number[] = [];
	let fed = 0;
	for (;;) {
		const count = Math.min(frames - fed, stretch.window - stretch.queued, stretch.room());
		if (count > 0) {
			stretch.push(input, fed, count);
			fed += count;
		}
		if (stretch.due) {
			if (stretch.queued === 0) break;
			shifts.push(stretch.next(decisions[Math.min(shifts.length, decisions.length - 1)]));
		}
		const played = stretch.read(chunk, 0, stretch.hop);
		if (played === 0) break;
		chunk.forEach((samples, channel) => {
			output[channel].push(...samples.subarray(0, played));
		});
	}
	return { output, shifts };
}

for (const rule of rules) {
	it(`decides the corpus inputs: ${rule.name}`, () => {
		const { rate, lag, depth, bound, read } = rule;
		expect(decide(rate, { lag, depth, bound, read })).toEqual(rule.decision);
	});
}

for (const vector of cases) {
	it(`stretches the corpus PCM: ${vector.name}`, () => {
		const stretch = new Stretch(vector.rate, vector.channels);
		const { output, shifts } = play(stretch, planar(vector.input, vector.channels), vector.decisions);
		expect(shifts).toEqual(vector.shifts);
		const expected = planar(vector.output, vector.channels);
		for (let channel = 0; channel < vector.channels; channel++) {
			expect(output[channel].length).toBe(expected[channel].length);
			output[channel].forEach((sample, i) => {
				expect(Math.abs(sample - expected[channel][i])).toBeLessThanOrEqual(1e-6);
			});
		}
	});
}

// A 100 Hz tone at 8 kHz, whose 80-frame period is exactly one hop.
const tone = (frames: number) => [
	Float32Array.from({ length: frames }, (_, i) => 0.25 * Math.sin((2 * Math.PI * i) / 80)),
];
const bridge = (rate: number): Decision => decide(rate, { lag: 0, depth: 0, bound: rate, read: 128 });

it("maps a crossfade's played share onto the input it shifts to", () => {
	const stretch = new Stretch(8000, 1);
	stretch.push(tone(stretch.window), 0, stretch.window);
	// A hop of history, so the bridge can reach back one period.
	stretch.next(HOLD);
	stretch.read([new Float32Array(stretch.hop)], 0, stretch.hop);
	stretch.push(tone(stretch.window + stretch.hop), stretch.window, stretch.hop);
	expect(stretch.ahead).toBe(stretch.window);

	expect(stretch.next(bridge(8000))).toBe(-80);
	// Half the crossfade moves the playhead half of a hop less the period it repeats: nowhere.
	stretch.read([new Float32Array(40)], 0, 40);
	expect(stretch.ahead).toBe(stretch.window);
	expect(stretch.available).toBe(stretch.window + 40);
	stretch.read([new Float32Array(40)], 0, 40);
	expect(stretch.due).toBe(true);
	expect(stretch.ahead).toBe(stretch.window);
});

it("never reaches back across a boundary it forgot", () => {
	const stretch = new Stretch(8000, 1);
	const input = tone(2 * stretch.window);
	stretch.push(input, 0, stretch.window);
	stretch.next(HOLD);
	stretch.read([new Float32Array(stretch.hop)], 0, stretch.hop);
	stretch.push(input, stretch.window, stretch.hop);
	stretch.forget();
	// Without history the bridge has nowhere to repeat from.
	expect(stretch.next(bridge(8000))).toBe(0);
});

it("splices no faster than the cap refills its allowance", () => {
	// A bridge asks to splice every hop: past a burst of one span, any run of hops repeats at most
	// a tenth of what it plays.
	const stretch = new Stretch(8000, 1);
	const { shifts } = play(stretch, tone(8000), [bridge(8000)]);
	expect(shifts.filter((shift) => shift !== 0).length).toBeGreaterThan(10);
	for (let from = 0; from < shifts.length; from++) {
		let repeated = 0;
		for (let to = from; to < shifts.length; to++) {
			repeated -= shifts[to];
			expect(repeated).toBeLessThanOrEqual(stretch.span + 0.1 * (to - from + 1) * stretch.hop);
		}
	}
});

it("turns off without a bound, and plays a tail shorter than a hop as received", () => {
	expect(decide(48000, { lag: 4800, depth: 0, bound: 0, read: 128 })).toEqual(HOLD);
	const stretch = new Stretch(8000, 1);
	const input = tone(50);
	const { output, shifts } = play(stretch, input, [decide(8000, { lag: 800, depth: 8000, bound: 1600, read: 128 })]);
	expect(shifts).toEqual([0]);
	expect(output[0]).toEqual(Array.from(input[0]));
});
