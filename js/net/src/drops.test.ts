import { expect, test } from "bun:test";
import { Runs } from "./drops.ts";
import type { Run } from "./internal.ts";

type Tagged = Run & { tag: number };

function filled(): Runs<Tagged> {
	const runs = new Runs<Tagged>((a, b) => a.tag === b.tag);
	for (let i = 0n; i < 8n; i++) runs.set(i * 10n, i * 10n + 5n, { start: 0n, end: 0n, tag: Number(i % 2n) });
	return runs;
}

test("drops: clearing ranges in one pass matches clearing each", () => {
	// Whole runs, a run split in two, a prefix, a suffix, and a range spanning runs and gaps.
	const ranges: Run[] = [
		{ start: 0n, end: 5n },
		{ start: 11n, end: 13n },
		{ start: 20n, end: 22n },
		{ start: 24n, end: 31n },
		{ start: 42n, end: 65n },
	];
	const each = filled();
	for (const { start, end } of ranges) each.set(start, end);
	const bulk = filled();
	const { removed, added } = bulk.clear(ranges);
	expect(bulk.all).toEqual(each.all);
	expect(removed.map((run) => run.start)).toEqual([0n, 10n, 20n, 30n, 40n, 50n, 60n]);
	expect(added.map((run) => [run.start, run.end])).toEqual([
		[10n, 11n],
		[13n, 15n],
		[22n, 24n],
		[31n, 35n],
		[40n, 42n],
	]);
});
