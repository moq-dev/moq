import { expect, it } from "bun:test";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { type Decision, Stretch } from "./stretch";

interface Vector {
	name: string;
	rate: number;
	channels: number;
	input: number[];
	buffered: number;
	target: number;
	/** The arrivals' lateness in frames, zero when the vector leaves it out. */
	lateness?: number;
	cooldown: number;
	decision: Decision;
	lag: number;
	output: number[];
}

// Shared with moq-audio, so neither implementation is graded against the other.
const corpus = join(
	dirname(fileURLToPath(import.meta.url)),
	"../../../../doc/concept/audio-jitter/stretch/corpus.json",
);
const vectors = JSON.parse(readFileSync(corpus, "utf8")) as Vector[];

// The corpus is interleaved; the ring is planar.
const planar = (samples: number[], channels: number) =>
	Array.from({ length: channels }, (_, channel) => samples.filter((_, i) => i % channels === channel));

for (const vector of vectors) {
	it(`stretches the corpus PCM: ${vector.name}`, () => {
		const stretch = new Stretch(vector.rate, vector.channels);
		planar(vector.input, vector.channels).forEach((samples, channel) => {
			stretch.input[channel].set(samples);
		});
		stretch.cooldown = vector.cooldown;
		const lateness = vector.lateness ?? 0;
		expect(stretch.decide(vector.buffered, vector.target, lateness)).toBe(vector.decision);

		stretch.process(stretch.window, vector.buffered, vector.target, lateness, true);
		expect(Math.abs(stretch.available - stretch.window)).toBe(vector.lag);

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

it("passes a short, disabled, or exhausted window through unchanged", () => {
	for (const rate of [8000, 44100, 48000, 96000]) {
		const stretch = new Stretch(rate, 2);
		for (const channel of stretch.input) channel.fill(0.25);
		stretch.cooldown = 0;

		// A ring well short of its target expands a full window of received PCM.
		stretch.process(stretch.window, stretch.window, rate, 0, true);
		expect(stretch.available).toBeGreaterThan(stretch.window);
		const output = [new Float32Array(rate), new Float32Array(rate)];
		expect(stretch.read(output, 0)).toBe(stretch.length);
		expect(stretch.read(output, 0)).toBe(0);

		stretch.cooldown = 0;
		stretch.process(stretch.window, stretch.window, rate, 0, false);
		expect(stretch.available).toBe(stretch.window);
		stretch.process(stretch.window >> 1, stretch.window, rate, 0, true);
		expect(stretch.available).toBe(stretch.window >> 1);
	}
});
