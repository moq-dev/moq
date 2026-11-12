import { expect, it } from "bun:test";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { type Decision, decide, Level, Stretch } from "./stretch";

interface Pcm {
	name: string;
	rate: number;
	channels: number;
	input: number[];
	decision: Decision;
	allowance: number;
	lag: number;
	output: number[];
}

interface Vectors {
	decisions: {
		name: string;
		rate: number;
		target: number;
		level: number;
		raw: number;
		lag: number;
		bound: number;
		cooldown: number;
		decision: Decision;
		allowance: number;
	}[];
	filter: {
		name: string;
		rate: number;
		target: number;
		steps: [number, number][];
		filtered: number[];
	}[];
}

// Shared with moq-audio, so neither implementation is graded against the other.
const shared = join(dirname(fileURLToPath(import.meta.url)), "../../../../doc/concept/audio-jitter/stretch");
const pcm = JSON.parse(readFileSync(join(shared, "corpus.json"), "utf8")) as Pcm[];
const vectors = JSON.parse(readFileSync(join(shared, "decisions.json"), "utf8")) as Vectors;

// The corpus is interleaved; the ring is planar.
const planar = (samples: number[], channels: number) =>
	Array.from({ length: channels }, (_, channel) => samples.filter((_, i) => i % channels === channel));

for (const vector of pcm) {
	it(`stretches the corpus PCM: ${vector.name}`, () => {
		const stretch = new Stretch(vector.rate, vector.channels);
		const input = planar(vector.input, vector.channels);
		input.forEach((samples, channel) => {
			stretch.input[channel].set(samples);
		});
		stretch.stage(stretch.window, { decision: vector.decision, allowance: vector.allowance });
		expect(Math.abs(stretch.window - stretch.available)).toBe(vector.lag);

		const output = Array.from({ length: vector.channels }, () => new Float32Array(stretch.available));
		expect(stretch.read(output, 0)).toBe(output[0].length);
		const expected = planar(vector.output, vector.channels);
		for (let channel = 0; channel < vector.channels; channel++) {
			expect(output[channel].length).toBe(expected[channel].length);
			output[channel].forEach((sample, i) => {
				expect(Math.abs(sample - expected[channel][i])).toBeLessThanOrEqual(1e-6);
			});
		}
	});
}

for (const vector of vectors.decisions) {
	it(`decides the corpus inputs: ${vector.name}`, () => {
		const { rate, cooldown, decision, allowance, ...inputs } = vector;
		expect(decide(rate, inputs, cooldown)).toEqual({ decision, allowance });
	});
}

for (const vector of vectors.filter) {
	it(`filters the corpus levels: ${vector.name}`, () => {
		const level = new Level();
		vector.steps.forEach(([value, stretched], step) => {
			const actual = level.update(vector.rate, vector.target, value, stretched);
			expect(Math.abs(actual - vector.filtered[step])).toBeLessThanOrEqual(1e-9);
		});
	});
}

it("takes what the stretch changed out of the next filter update", () => {
	// 48 kHz at a 100 ms target: a constant signal accelerates by its shortest period, 120 frames,
	// from a buffer at the high limit.
	const inputs = { target: 4800, level: 4800, raw: 4800, lag: 120, bound: 0 };
	const stretch = new Stretch(48000, 1);
	stretch.input[0].fill(0.25);
	stretch.cooldown = 0;
	expect(stretch.process(stretch.window, inputs)).toBe("Accelerate");
	expect(stretch.available).toBe(stretch.window - 120);
	// The first update takes the level as it is, the next takes out the period removed.
	expect(stretch.level.filtered).toBe(4800);
	stretch.process(stretch.window, inputs);
	expect(stretch.level.filtered).toBe(4680);
	// A reset starts again from the level.
	stretch.level.reset();
	stretch.process(stretch.window, inputs);
	expect(stretch.level.filtered).toBe(4800);
});

it("passes a short, input-less, or exhausted window through unchanged", () => {
	for (const rate of [8000, 44100, 48000, 96000]) {
		const stretch = new Stretch(rate, 2);
		for (const channel of stretch.input) channel.fill(0.25);
		stretch.cooldown = 0;

		// Ahead of the clock by a second: expand back onto it.
		const target = Math.floor(rate / 10);
		const inputs = { target, level: target, raw: target, lag: -rate, bound: 0 };
		expect(stretch.process(stretch.window, inputs)).toBe("Expand");
		expect(stretch.available).toBeGreaterThan(stretch.window);
		const output = [new Float32Array(rate), new Float32Array(rate)];
		expect(stretch.read(output, 0)).toBe(stretch.length);
		expect(stretch.read(output, 0)).toBe(0);

		stretch.cooldown = 0;
		stretch.process(stretch.window);
		expect(stretch.available).toBe(stretch.window);
		stretch.process(stretch.window >> 1, inputs);
		expect(stretch.available).toBe(stretch.window >> 1);
	}
});

it("never moves a seam past its allowance", () => {
	// A constant signal's shortest period is 2.5 ms: 120 frames at 48 kHz.
	const cases: [Decision, number, number][] = [
		["Accelerate", 119, 1440],
		["Accelerate", 120, 1320],
		["Accelerate", 239, 1320],
		["Fast", 360, 1080],
		["Expand", 119, 1440],
		["Expand", 120, 1560],
	];
	for (const [decision, allowance, length] of cases) {
		const stretch = new Stretch(48000, 1);
		stretch.input[0].fill(0.25);
		stretch.stage(stretch.window, { decision, allowance });
		expect(stretch.length).toBe(length);
	}
});
