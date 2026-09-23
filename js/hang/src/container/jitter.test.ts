import { describe, expect, it } from "bun:test";
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import type { Time } from "@moq/net";
import { Jitter } from "./jitter";

// The conformance corpus beside doc/concept/audio-jitter.md, read directly so this is graded
// against the document rather than against another implementation.
const CORPUS = join(dirname(fileURLToPath(import.meta.url)), "../../../../doc/concept/audio-jitter");

interface Case {
	name: string;
	frame: number;
	advertised?: number;
	delay?: number;
	arrival: number[];
	media: number[];
	target: [number, number][];
}

const cases: Case[] = readdirSync(CORPUS)
	.filter((file) => file.endsWith(".json"))
	.map((file) => JSON.parse(readFileSync(join(CORPUS, file), "utf8")));

// The composition the document specifies on top of the measured term. `@moq/watch` owns it in
// production (`audio/latency.ts`); it is repeated here so the whole target series is checked.
function target(jitter: Jitter, c: Case): number {
	if (c.delay !== undefined) return c.delay;
	return Math.max(jitter.measured, c.advertised ?? 0) + c.frame;
}

describe("audio jitter conformance", () => {
	it("reads the whole corpus", () => {
		expect(cases.map((c) => c.name).sort()).toEqual([
			"advertised",
			"buildup",
			"fixed",
			"flush",
			"idle",
			"paced",
			"reorder",
			"spike",
			"tunein",
		]);
	});

	for (const c of cases) {
		it(c.name, () => {
			const jitter = new Jitter();
			const expected = new Map(c.target);
			let current: number | undefined;
			for (let i = 0; i < c.arrival.length; i++) {
				jitter.observe(c.arrival[i] as Time.Milli, c.media[i] as Time.Milli);
				const value = target(jitter, c);
				const listed = expected.get(i);
				if (listed !== undefined) {
					expect(value).toBeCloseTo(listed, 6);
				} else if (current !== undefined) {
					// Unlisted frames hold the previous target.
					expect(value).toBeCloseTo(current, 6);
				}
				current = value;
			}
		});
	}
});

describe("Jitter", () => {
	it("starts at the prior's 95th percentile", () => {
		expect(new Jitter().measured).toBe(100 as Time.Milli);
	});

	it("is not moved by a stale frame ahead of the live edge", () => {
		// The #3517 regression: one frame from a stale group, then the live edge 14.5s of media later,
		// then evenly paced audio with no real jitter. The jump must not read as a delay or a frame.
		const jitter = new Jitter();
		jitter.observe(0 as Time.Milli, 0 as Time.Milli);
		for (let i = 0; i < 500; i++) {
			jitter.observe((10 + i * 20) as Time.Milli, (14_500 + i * 20) as Time.Milli);
		}
		expect(jitter.measured).toBe(20 as Time.Milli);
	});

	it("does not follow the absolute delay", () => {
		// Only the spread of arrivals matters, not how far away the sender is.
		const near = new Jitter();
		const far = new Jitter();
		for (let i = 0; i < 500; i++) {
			near.observe((10 + i * 20) as Time.Milli, (i * 20) as Time.Milli);
			far.observe((2000 + i * 20) as Time.Milli, (i * 20) as Time.Milli);
		}
		expect(far.measured).toBe(near.measured);
	});
});
