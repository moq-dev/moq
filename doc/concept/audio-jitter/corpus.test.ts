// The corpus is data the two implementations are graded against, so the one thing this
// has to catch is the corpus drifting from the algorithm it claims to encode: a checked-in
// file edited by hand to make a failing implementation pass would otherwise go unnoticed.
//
// Grading an implementation is each language's own test suite, since neither exists yet.

import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { type Case, cases } from "./corpus";

const here = dirname(fileURLToPath(import.meta.url));

describe("audio jitter corpus", () => {
	for (const expected of cases) {
		describe(expected.name, () => {
			const actual = JSON.parse(readFileSync(join(here, `${expected.name}.json`), "utf8")) as Case;

			test("matches the checked-in file", () => {
				expect(actual).toEqual(expected);
			});

			test("is a trace of equal-length clocks", () => {
				expect(actual.arrival.length).toBe(actual.media.length);
				expect(actual.arrival.length).toBeGreaterThan(0);
			});

			test("arrives in a monotonic order", () => {
				for (let i = 1; i < actual.arrival.length; i++) {
					expect(actual.arrival[i]).toBeGreaterThanOrEqual(actual.arrival[i - 1]);
				}
			});

			test("names a target for the first frame", () => {
				expect(actual.target[0]?.[0]).toBe(0);
			});

			test("reports every target as a change", () => {
				let previous: number | undefined;
				let last = -1;
				for (const [index, value] of actual.target) {
					expect(index).toBeGreaterThan(last);
					expect(index).toBeLessThan(actual.arrival.length);
					expect(value).not.toBe(previous);
					previous = value;
					last = index;
				}
			});
		});
	}
});
