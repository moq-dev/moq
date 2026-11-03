import { expect, test } from "bun:test";
import { Delivered } from "./delivered.ts";

test("add joins neighbouring runs", () => {
	const delivered = new Delivered();
	expect(delivered.add(5n)).toBe(true);
	expect(delivered.add(7n)).toBe(true);
	expect(delivered.runs).toBe(2);
	expect(delivered.add(6n)).toBe(true);
	expect(delivered.runs).toBe(1);
	expect(delivered.add(6n)).toBe(false);
	expect(delivered.add(4n)).toBe(true);
	expect(delivered.add(8n)).toBe(true);
	expect(delivered.runs).toBe(1);
	expect(delivered.has(4n) && delivered.has(8n)).toBe(true);
	expect(delivered.has(3n) || delivered.has(9n)).toBe(false);
	expect(delivered.covers(4n, 9n) && delivered.covers(9n, 9n)).toBe(true);
	expect(delivered.covers(3n, 5n) || delivered.covers(8n, 10n)).toBe(false);
});

test("the extremes are sequences too", () => {
	const delivered = new Delivered();
	const max = Number.MAX_SAFE_INTEGER;
	expect(delivered.add(0n)).toBe(true);
	expect(delivered.add(max)).toBe(true);
	expect(delivered.add(max)).toBe(false);
	expect(delivered.runs).toBe(2);
	delivered.forgetBelow(max);
	expect(delivered.runs).toBe(1);
	expect(delivered.aboveGap()).toBeUndefined();
});

test("forgetBelow closes the gaps under the floor", () => {
	const delivered = new Delivered();
	for (const sequence of [3, 5, 6, 9, 12]) delivered.add(sequence);
	expect(delivered.aboveGap()).toBe(3n);
	delivered.forgetBelow(0n);
	expect(delivered.runs).toBe(4);

	// A floor at a run's start takes the run along.
	delivered.forgetBelow(5n);
	expect(delivered.runs).toBe(3);
	expect(delivered.aboveGap()).toBe(9n);
	expect(delivered.has(4n)).toBe(true);
	expect(delivered.has(7n)).toBe(false);

	// A floor inside a gap keeps the run above it.
	delivered.forgetBelow(11n);
	expect(delivered.runs).toBe(2);
	expect(delivered.has(10n)).toBe(true);
	expect(delivered.aboveGap()).toBe(12n);
	delivered.forgetBelow(13n);
	expect(delivered.runs).toBe(1);
	expect(delivered.aboveGap()).toBeUndefined();
	expect(delivered.add(13n)).toBe(true);
	expect(delivered.runs).toBe(1);
});

test("a run joins every run it touches, leaving the gaps around it", () => {
	const delivered = new Delivered();
	for (const sequence of [2, 5, 9]) delivered.add(sequence);
	delivered.addRun(4n, 9n);
	expect(delivered.runs).toBe(2);
	expect(delivered.gaps(0n, 12n)).toEqual([
		{ start: 0n, end: 2n },
		{ start: 3n, end: 4n },
		{ start: 10n, end: 12n },
	]);
	delivered.addRun(0n, 0n);
	delivered.addRun(11n, 12n);
	expect(delivered.gaps(3n, 12n)).toEqual([
		{ start: 3n, end: 4n },
		{ start: 10n, end: 11n },
	]);
	delivered.addRun(0n, 20n);
	expect(delivered.runs).toBe(1);
	expect(delivered.gaps(0n, 20n)).toEqual([]);
});
