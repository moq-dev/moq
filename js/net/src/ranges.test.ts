import { expect, test } from "bun:test";
import { Counts, Cover } from "./cover.ts";
import { Ranges } from "./ranges.ts";
import type { Anchor, Range } from "./track.ts";
import { max, min, UNBOUNDED } from "./util/key.ts";
import { union } from "./window.ts";

// A seeded generator, so a failure reproduces.
function rng(seed: number): (below: number) => number {
	let state = seed >>> 0;
	return (below) => {
		state = (state + 0x6d2b79f5) >>> 0;
		let t = state;
		t = Math.imul(t ^ (t >>> 15), t | 1);
		t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
		return (((t ^ (t >>> 14)) >>> 0) / 2 ** 32) * below;
	};
}

// Each change reports exactly whether the union moved, and the union matches a full fold of what
// is held.
test("the incremental union matches a full fold", () => {
	const random = rng(0x72616e67);
	const int = (below: number) => Math.floor(random(below));
	const anchor = (): Anchor | undefined => {
		const kind = int(6);
		if (kind === 0) return { relative: BigInt(int(4)) };
		if (kind === 1) return undefined;
		return { time: BigInt(int(12)) };
	};
	const ranges = new Ranges();
	const held: Range[] = [];
	for (let step = 0; step < 50_000; step++) {
		const grow = held.length < 24 ? 0.65 : 0.35;
		const before = union(held);
		let changed: boolean;
		if (held.length === 0 || random(1) < grow) {
			const range: Range = {};
			const start = anchor();
			if (start) range.start = start;
			const end = random(1) < 0.6 ? anchor() : undefined;
			if (end) range.end = end;
			held.push(range);
			changed = ranges.add(range);
		} else {
			const index = int(held.length);
			const [range] = held.splice(index, 1);
			changed = ranges.remove(range);
		}
		const after = union(held);
		expect(ranges.union()).toEqual(after);
		expect(changed).toBe(!Bun.deepEquals(before, after));
	}
});

// Every query matches a count kept per key.
test("a cover answers as its counts do", () => {
	const random = rng(0x636f76);
	const int = (below: number) => Math.floor(random(below));
	const keys = 40;
	const cover = new Cover();
	const counts = new Array<number>(keys + 1).fill(0);
	const held: [number, number][] = [];
	for (let step = 0; step < 5_000; step++) {
		if (held.length === 0 || random(1) < 0.55) {
			const start = int(keys);
			// The last key stands for open.
			const end = start + 1 + int(keys - start);
			held.push([start, end]);
			for (let key = start; key < (end === keys ? keys + 1 : end); key++) counts[key]++;
			cover.add(BigInt(start), end === keys ? UNBOUNDED : BigInt(end), 1);
		} else {
			const [[start, end]] = held.splice(int(held.length), 1);
			for (let key = start; key < (end === keys ? keys + 1 : end); key++) counts[key]--;
			cover.add(BigInt(start), end === keys ? UNBOUNDED : BigInt(end), -1);
		}
		const key = (index: number) => (index >= keys ? UNBOUNDED : BigInt(index));
		const runs: { start: bigint; end: bigint }[] = [];
		for (let index = 0; index < keys; index++) {
			if (counts[index] === 0) continue;
			const last = runs.at(-1);
			if (last && last.end === BigInt(index)) last.end = key(index + 1);
			else runs.push({ start: BigInt(index), end: key(index + 1) });
		}
		const from = int(keys);
		const to = from + int(keys - from + 1);
		const clipped = runs
			.filter((run) => run.end > BigInt(from) && run.start < key(to))
			.map((run) => ({
				start: max(run.start, BigInt(from)),
				end: min(run.end, key(to)),
			}))
			.filter((run) => run.start < run.end);
		expect(cover.runs(BigInt(from), key(to))).toEqual(clipped);
		expect(cover.contains(BigInt(from))).toBe(counts[from] > 0);
		expect(cover.next(BigInt(from))).toEqual(
			runs.find((run) => run.end > BigInt(from)) &&
				(counts[from] > 0 ? BigInt(from) : runs.find((run) => run.start > BigInt(from))?.start),
		);
		expect(cover.lowest()).toEqual(runs[0]?.start);
		expect(cover.end()).toBe(runs.at(-1)?.end ?? 0n);
		expect(cover.openFrom()).toEqual(runs.at(-1)?.end === UNBOUNDED ? runs.at(-1)?.start : undefined);
		const least = Math.min(...counts.slice(from, Math.max(to, from + 1)));
		expect(cover.least(BigInt(from), key(Math.max(to, from + 1)))).toBe(least);
	}
});

test("counts track their lowest and highest key", () => {
	const counts = new Counts<number>();
	expect(counts.add(3)).toBe(true);
	expect(counts.add(3)).toBe(false);
	expect(counts.add(-1)).toBe(true);
	expect([counts.first(), counts.last()]).toEqual([-1, 3]);
	expect(counts.remove(3)).toBe(false);
	expect(counts.remove(3)).toBe(true);
	expect(counts.remove(3)).toBe(false);
	expect([counts.first(), counts.last()]).toEqual([-1, -1]);
});
