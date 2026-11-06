import { expect, it } from "bun:test";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { ComfortNoise } from "./comfort";

type Step = { play: number[]; frames: number } | { fill: number };
type Case = { name: string; rate: number; channels: number; steps: Step[] };

// Shared with moq-audio, so neither implementation is graded against the other.
const corpus = join(
	dirname(fileURLToPath(import.meta.url)),
	"../../../../doc/concept/audio-jitter/comfort/corpus.json",
);
const cases = JSON.parse(readFileSync(corpus, "utf8")) as Case[];

for (const { name, rate, channels, steps } of cases) {
	it(`fills a silence at the corpus level: ${name}`, () => {
		const comfort = new ComfortNoise(rate);
		for (const step of steps) {
			if ("play" in step) {
				const output = step.play.map((value) => new Float32Array(step.frames).fill(value));
				comfort.played(output, 0, step.frames);
			} else {
				comfort.fill(
					Array.from({ length: channels }, () => new Float32Array(2)),
					0,
				);
				expect(Math.abs(comfort.level - step.fill)).toBeLessThan(1e-9);
			}
		}
	});
}
