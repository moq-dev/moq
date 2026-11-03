import { expect, test } from "bun:test";
import { Delivered } from "./delivered.ts";

test("add joins neighbouring runs", () => {
	const delivered = new Delivered();
	expect(delivered.add(5)).toBe(true);
	expect(delivered.add(7)).toBe(true);
	expect(delivered.runs).toBe(2);
	expect(delivered.add(6)).toBe(true);
	expect(delivered.runs).toBe(1);
	expect(delivered.add(6)).toBe(false);
	expect(delivered.add(4)).toBe(true);
	expect(delivered.add(8)).toBe(true);
	expect(delivered.runs).toBe(1);
	expect(delivered.has(4) && delivered.has(8)).toBe(true);
	expect(delivered.has(3) || delivered.has(9)).toBe(false);
	expect(delivered.covers(4, 9) && delivered.covers(9, 9)).toBe(true);
	expect(delivered.covers(3, 5) || delivered.covers(8, 10)).toBe(false);
});

test("the extremes are sequences too", () => {
	const delivered = new Delivered();
	const max = Number.MAX_SAFE_INTEGER;
	expect(delivered.add(0)).toBe(true);
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
	expect(delivered.aboveGap()).toBe(3);
	delivered.forgetBelow(0);
	expect(delivered.runs).toBe(4);

	// A floor at a run's start takes the run along.
	delivered.forgetBelow(5);
	expect(delivered.runs).toBe(3);
	expect(delivered.aboveGap()).toBe(9);
	expect(delivered.has(4)).toBe(true);
	expect(delivered.has(7)).toBe(false);

	// A floor inside a gap keeps the run above it.
	delivered.forgetBelow(11);
	expect(delivered.runs).toBe(2);
	expect(delivered.has(10)).toBe(true);
	expect(delivered.aboveGap()).toBe(12);
	delivered.forgetBelow(13);
	expect(delivered.runs).toBe(1);
	expect(delivered.aboveGap()).toBeUndefined();
	expect(delivered.add(13)).toBe(true);
	expect(delivered.runs).toBe(1);
});

test("a run joins every run it touches, leaving the gaps around it", () => {
	const delivered = new Delivered();
	for (const sequence of [2, 5, 9]) delivered.add(sequence);
	delivered.addRun(4, 9);
	expect(delivered.runs).toBe(2);
	expect(delivered.gaps(0, 12)).toEqual([
		{ start: 0, end: 2 },
		{ start: 3, end: 4 },
		{ start: 10, end: 12 },
	]);
	delivered.addRun(0, 0);
	delivered.addRun(11, 12);
	expect(delivered.gaps(3, 12)).toEqual([
		{ start: 3, end: 4 },
		{ start: 10, end: 11 },
	]);
	delivered.addRun(0, 20);
	expect(delivered.runs).toBe(1);
	expect(delivered.gaps(0, 20)).toEqual([]);
});
