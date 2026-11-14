// The corpus is data the two players are graded against, so the one thing this has to catch is
// the corpus drifting from the algorithm it claims to encode: a checked-in file edited by hand to
// make a failing implementation pass would otherwise go unnoticed.
//
// Grading an implementation is each language's own test suite: js/watch's stretch.test.ts and
// moq-audio's stretch.rs.

import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { type Case, cases, type Rule, rules } from "./corpus";

const here = dirname(fileURLToPath(import.meta.url));
function read<T>(name: string): T {
	return JSON.parse(readFileSync(join(here, name), "utf8")) as T;
}

describe("audio stretch corpus", () => {
	test("the decisions match the checked-in file", () => {
		expect(read<Rule[]>("decisions.json")).toEqual(rules);
	});

	const actual = read<Case[]>("corpus.json");
	test("the cases match the checked-in file", () => {
		expect(actual.map(({ name }) => name)).toEqual(cases.map(({ name }) => name));
	});

	for (const expected of cases) {
		describe(expected.name, () => {
			const found = actual.find(({ name }) => name === expected.name);

			test("matches the checked-in case", () => {
				expect(found).toEqual(expected);
			});

			test("decides every hop it plays", () => {
				expect(expected.decisions.length).toBeGreaterThan(0);
				expect(expected.shifts.length).toBeGreaterThanOrEqual(expected.decisions.length);
			});

			test("moves the output by exactly the shifts it took", () => {
				const frames = expected.input.length / expected.channels;
				const moved = expected.shifts.reduce((sum, shift) => sum + shift, 0);
				expect(expected.output.length / expected.channels).toBe(frames - moved);
			});
		});
	}
});
