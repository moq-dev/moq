import { expect, test } from "bun:test";
import { DropSet, Runs } from "./drops.ts";
import { NotFound } from "./error.ts";
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

test("drops: what a change replaced or forgot is reported since its revision, until it is cleared", () => {
	const drops = new DropSet();
	const hole = { error: new NotFound("gone") };
	drops.set(0n, 10n, hole);
	const before = drops.revision;
	drops.set(4n, 6n);
	drops.set(20n, 30n, hole);
	const extents = (runs: Run[] | undefined) => runs?.map((run) => [run.start, run.end]);
	expect(extents(drops.removedSince(before))).toEqual([[0n, 10n]]);
	expect(drops.removedSince(drops.revision)).toEqual([]);
	// Removals far outnumbering the live runs are cleared, so an older revision rebuilds instead.
	const recent = drops.revision;
	for (let i = 0; i < 100; i++) {
		drops.set(40n, 41n, hole);
		drops.set(40n, 41n);
	}
	expect(drops.removedSince(recent)).toBeUndefined();
	expect(drops.removedSince(drops.revision)).toEqual([]);
});
