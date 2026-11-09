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
	cooldown: number;
	/** As native playback decides: it also expands, which this player never does. */
	decision: Decision | "Expand";
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

// The corpus decides as NetEq does, on the buffered depth against a band around the target. This
// player decides on the excess above the target alone and never expands, so the corpus grades its
// seams: an excess past the 20 ms band fast-accelerates, one inside it accelerates, and a ring at or
// short of its target, where NetEq holds or expands, plays its window unchanged.
const excess = (vector: Vector) => {
	const band = Math.floor((vector.rate * 20) / 1000);
	if (vector.decision === "Fast") return vector.buffered - vector.target;
	if (vector.decision === "Accelerate") return band - 1;
	return Math.min(0, vector.buffered - vector.target);
};

for (const vector of vectors) {
	it(`stretches the corpus PCM: ${vector.name}`, () => {
		const stretch = new Stretch(vector.rate, vector.channels);
		const input = planar(vector.input, vector.channels);
		input.forEach((samples, channel) => {
			stretch.input[channel].set(samples);
		});
		stretch.cooldown = vector.cooldown;
		const expand = vector.decision === "Expand";
		expect(stretch.decide(excess(vector))).toBe(vector.decision === "Expand" ? "Hold" : vector.decision);

		stretch.process(stretch.window, excess(vector), true);
		expect(stretch.window - stretch.available).toBe(expand ? 0 : vector.lag);

		const output = Array.from({ length: vector.channels }, () => new Float32Array(stretch.available));
		expect(stretch.read(output, 0)).toBe(output[0].length);
		const expected = expand ? input : planar(vector.output, vector.channels);
		for (let channel = 0; channel < vector.channels; channel++) {
			expect(output[channel].length).toBe(expected[channel].length);
			output[channel].forEach((sample, i) => {
				expect(Math.abs(sample - expected[channel][i])).toBeLessThanOrEqual(1e-6);
			});
		}
	});
}

it("decides on the excess above the target, and never to slow down", () => {
	const stretch = new Stretch(48000, 1);
	stretch.cooldown = 0;
	// At or short of the target is audio on the clock or arrivals running late: either way, hold.
	expect(stretch.decide(0)).toBe("Hold");
	expect(stretch.decide(-48000)).toBe("Hold");
	// Any excess accelerates, so the ring rests on its target rather than in a band above it.
	expect(stretch.decide(1)).toBe("Accelerate");
	expect(stretch.decide(959)).toBe("Accelerate");
	// From NetEq's 20 ms granularity, fast-accelerate, even straight after a stretch.
	expect(stretch.decide(960)).toBe("Fast");
	stretch.cooldown = 1;
	expect(stretch.decide(959)).toBe("Hold");
	expect(stretch.decide(960)).toBe("Fast");
});

it("passes a short, disabled, exhausted, or late window through unchanged", () => {
	for (const rate of [8000, 44100, 48000, 96000]) {
		const stretch = new Stretch(rate, 2);
		for (const channel of stretch.input) channel.fill(0.25);
		stretch.cooldown = 0;

		// A ring well above its target accelerates a full window of received PCM.
		stretch.process(stretch.window, stretch.window, true);
		expect(stretch.available).toBeLessThan(stretch.window);
		const output = [new Float32Array(rate), new Float32Array(rate)];
		expect(stretch.read(output, 0)).toBe(stretch.length);
		expect(stretch.read(output, 0)).toBe(0);

		stretch.cooldown = 0;
		stretch.process(stretch.window, stretch.window, false);
		expect(stretch.available).toBe(stretch.window);
		stretch.process(stretch.window >> 1, stretch.window, true);
		expect(stretch.available).toBe(stretch.window >> 1);
		// A ring a whole window short of its target still plays it as received.
		stretch.process(stretch.window, -stretch.window, true);
		expect(stretch.available).toBe(stretch.window);
	}
});

it("accelerates only by a pitch period that fits in the excess", () => {
	// A constant signal's shortest period is 2.5 ms: 120 frames at 48 kHz.
	const stretch = new Stretch(48000, 1);
	stretch.input[0].fill(0.25);
	stretch.cooldown = 0;
	stretch.process(stretch.window, 119, true);
	expect(stretch.available).toBe(stretch.window);
	stretch.cooldown = 0;
	stretch.process(stretch.window, 120, true);
	expect(stretch.available).toBe(stretch.window - 120);
});
