import { expect, test } from "bun:test";
import { Delivered, type Judged } from "./delivered.ts";
import { max, min } from "./util/key.ts";

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
	const max = BigInt(Number.MAX_SAFE_INTEGER);
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
	for (const key of [3n, 5n, 6n, 9n, 12n]) delivered.add(key);
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
	for (const key of [2n, 5n, 9n]) delivered.add(key);
	delivered.addRun(4n, 9n);
	expect(delivered.runs).toBe(2);
	expect([...delivered.gaps(0n, 12n)]).toEqual([
		{ start: 0n, end: 2n },
		{ start: 3n, end: 4n },
		{ start: 10n, end: 12n },
	]);
	delivered.addRun(0n, 0n);
	delivered.addRun(11n, 12n);
	expect([...delivered.gaps(3n, 12n)]).toEqual([
		{ start: 3n, end: 4n },
		{ start: 10n, end: 11n },
	]);
	delivered.addRun(0n, 20n);
	expect(delivered.runs).toBe(1);
	expect([...delivered.gaps(0n, 20n)]).toEqual([]);
});

// The judged groups as a window places them, from `[start, end)` runs.
function judged(...runs: [bigint, bigint][]): Judged {
	return {
		placed: (from, end) =>
			runs
				.filter(([start, stop]) => start < end && stop > from)
				.map(([start, stop]) => ({ start: max(start, from), end: min(stop, end) })),
		pending: false,
	};
}

test("a fold reopens only the gaps no range judged", () => {
	const delivered = new Delivered();
	for (const key of [4n, 9n]) delivered.add(key);
	// Ranges hold 2..5 and 6..8 as the fold closes every gap below 9.
	delivered.foldBelow(9n, judged([2n, 5n], [6n, 8n]));
	expect([...delivered.gaps(0n, 10n)]).toEqual([]);
	expect(delivered.runs).toBe(4);
	// A range added over 0..9 owes the gaps folded unjudged, never a hand-out nor what the budget judged.
	delivered.reopen(judged([0n, 9n]));
	expect([...delivered.gaps(0n, 10n)]).toEqual([
		{ start: 0n, end: 2n },
		{ start: 5n, end: 6n },
		{ start: 8n, end: 9n },
	]);
	expect(delivered.runs).toBe(3);
	// Folded while the ranges are still being placed, every gap is unjudged; one settled since is not reopened.
	delivered.foldBelow(10n, undefined);
	delivered.addRun(0n, 1n);
	delivered.reopen(judged([0n, 10n]));
	expect([...delivered.gaps(0n, 10n)]).toEqual([
		{ start: 1n, end: 2n },
		{ start: 5n, end: 6n },
		{ start: 8n, end: 9n },
	]);
});

test("a fold keeps no more unjudged runs than the ranges hold", () => {
	const delivered = new Delivered();
	// Ranges moving to a fresh group each time, each handed out and folded past by ranges then
	// holding only the live edge, would leave a run no range held between each pair.
	for (let key = 10n; key < 100n; key += 10n) {
		delivered.add(key);
		delivered.foldBelow(key + 5n, judged([key + 5n, 1000n]));
	}
	expect(delivered.runs).toBe(2);
	// A range added over a group handed out before owes it again, and while it holds it, splits
	// the runs around it; one no longer held, as 97 next, joins them.
	delivered.reopen(judged([40n, 41n], [95n, 1000n]));
	expect(delivered.has(40n)).toBe(false);
	delivered.add(40n);
	delivered.add(97n);
	delivered.foldBelow(99n, judged([40n, 41n], [99n, 1000n]));
	expect(delivered.runs).toBe(3);
	delivered.reopen(judged([0n, 99n]));
	expect([...delivered.gaps(0n, 100n)]).toEqual([
		{ start: 0n, end: 40n },
		{ start: 41n, end: 100n },
	]);
});

test("a reopen closes the gaps no range holds any longer", () => {
	const delivered = new Delivered();
	delivered.add(100n);
	delivered.foldBelow(100n, judged([100n, 1000n]));
	// Lookbacks replaced one by one while none of their groups arrives leave one gap, not one each.
	for (let key = 10n; key < 90n; key += 10n) {
		delivered.reopen(judged([key, key + 1n], [100n, 1000n]));
		expect([...delivered.gaps(0n, 100n)]).toEqual([{ start: key, end: key + 1n }]);
	}
	expect(delivered.runs).toBe(4);
	// Nor while a live join waits, which may hold any group.
	delivered.reopen({ ...judged([20n, 21n]), pending: true });
	expect([...delivered.gaps(0n, 100n)]).toEqual([
		{ start: 20n, end: 21n },
		{ start: 80n, end: 81n },
	]);
});

test("remove splits the runs it cuts", () => {
	const delivered = new Delivered();
	delivered.addRun(2n, 7n);
	for (const key of [9n, 12n]) delivered.add(key);
	delivered.remove(4n, 10n);
	expect([...delivered.gaps(0n, 13n)]).toEqual([
		{ start: 0n, end: 2n },
		{ start: 4n, end: 12n },
	]);
	delivered.addRun(2n, 12n);
	delivered.remove(5n, 6n);
	expect(delivered.has(4n) && !delivered.has(5n) && delivered.has(6n)).toBe(true);
	delivered.remove(13n, 20n);
	delivered.remove(8n, 8n);
	expect(delivered.runs).toBe(2);
});
