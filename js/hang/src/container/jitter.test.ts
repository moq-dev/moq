import { describe, expect, it } from "bun:test";
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import type { Milli } from "@moq/clock";
import { type Entry, History, Jitter } from "./jitter";

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
			"backlog",
			"buildup",
			"fixed",
			"flush",
			"idle",
			"paced",
			"reorder",
			"spike",
			"straggler",
			"tunein",
		]);
	});

	for (const c of cases) {
		it(c.name, () => {
			const jitter = new Jitter();
			const expected = new Map(c.target);
			let current: number | undefined;
			for (let i = 0; i < c.arrival.length; i++) {
				jitter.observe(c.arrival[i] as Milli, c.media[i] as Milli);
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
		expect(new Jitter().measured).toBe(100 as Milli);
	});

	it("is not moved by a stale frame ahead of the live edge", () => {
		// The #3517 regression: one frame from a stale group, then the live edge 14.5s of media later,
		// then evenly paced audio with no real jitter. The jump must not read as a delay or a frame.
		const jitter = new Jitter();
		jitter.observe(0 as Milli, 0 as Milli);
		for (let i = 0; i < 500; i++) {
			jitter.observe((10 + i * 20) as Milli, (14_500 + i * 20) as Milli);
		}
		expect(jitter.measured).toBe(20 as Milli);
	});

	it("does not follow the absolute delay", () => {
		// Only the spread of arrivals matters, not how far away the sender is.
		const near = new Jitter();
		const far = new Jitter();
		for (let i = 0; i < 500; i++) {
			near.observe((10 + i * 20) as Milli, (i * 20) as Milli);
			far.observe((2000 + i * 20) as Milli, (i * 20) as Milli);
		}
		expect(far.measured).toBe(near.measured);
	});

	it("forks what it learned, but measures the replacement's arrivals against their own", () => {
		// A path where every tenth frame lands 300ms late.
		const warm = new Jitter();
		for (let i = 0; i < 500; i++) {
			warm.observe((i * 20 + (i % 10 === 0 ? 300 : 0)) as Milli, (i * 20) as Milli);
		}
		const learned = warm.measured;
		expect(learned).toBeGreaterThan(100 as Milli);

		const fork = warm.fork();
		expect(fork.measured).toBe(learned);

		// The replacement opens with a backlog: older media arriving at once, long after the warm
		// track's frames. Measured against the warm track's arrivals it would read as seconds late.
		for (let i = 0; i < 100; i++)
			fork.observe((20_000 + Math.floor(i / 50) * 1000) as Milli, (8000 + i * 20) as Milli);
		expect(fork.measured).toBeLessThanOrEqual(learned);
		expect(warm.measured).toBe(learned);
	});
});

// The documented scan: the first entry with the smallest key at or before `media`.
function scan(history: History, media: number): Entry | undefined {
	let reference: Entry | undefined;
	for (let i = 0; i < history.length; i++) {
		const entry = history.at(i);
		if (entry.media > media) break;
		if (!reference || entry.arrival - entry.media < reference.arrival - reference.media) reference = entry;
	}
	return reference;
}

describe("History", () => {
	// A tied entry with a different (arrival, media) rounds the delay differently, so the tree must
	// pick exactly the entry the scan would.
	it("picks the entry the scan would, ties included", () => {
		const history = new History();
		let seed = 1;
		let newest = Number.NEGATIVE_INFINITY;
		for (let i = 0; i < 400; i++) {
			seed = (seed * 1103515245 + 12345) % 2 ** 31;
			// Mostly in order, with an occasional reordered frame and a timestamp jump.
			const media = i % 37 === 0 ? i * 20 - 900 : i % 89 === 0 ? i * 20 + 3000 : i * 20;
			// Whole milliseconds of jitter tie often; the occasional fraction does not.
			const jitter = i % 5 === 0 ? seed / 2 ** 21 : seed % 8;
			if (media > newest) {
				newest = media;
				history.push(i * 20 + jitter, media);
			}
			for (const query of [media - 30, media - 10, media, media + 10, media + 1e6]) {
				expect(history.reference(query)).toEqual(scan(history, query));
			}
		}
	});

	it("caps a timestamp rate the peer chooses, and the wrapped ring answers every prefix", () => {
		const history = new History();
		for (let i = 0; i < 8192 + 5000; i++) {
			const media = i / 1000;
			history.push(i + (i % 7), media);
			if (i % 997 === 0) {
				for (const query of [media - 3, media - 0.5, media]) {
					expect(history.reference(query)).toEqual(scan(history, query));
				}
			}
		}
		expect(history.length).toBe(8192);
	});
});
