import { expect, test } from "bun:test";
import { Duration } from "@moq/clock";
import { Counts, Cover } from "./cover.ts";
import { Ranges } from "./ranges.ts";
import type { Range, Start } from "./track.ts";
import { max, min, UNBOUNDED } from "./util/key.ts";
import { rangeKey, union } from "./window.ts";

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
// is held: time ranges, live joins by lag, and ranges from the oldest group, each at a policy.
test("the incremental union matches a full fold", () => {
	const random = rng(0x72616e67);
	const int = (below: number) => Math.floor(random(below));
	const start = (): Start | undefined => {
		const kind = int(10);
		if (kind === 0) return undefined;
		if (kind === 1) return { live: { min: Duration.millis(int(3)), max: Duration.millis(int(3)) } };
		return BigInt(int(12));
	};
	const keys = (ranges: readonly Range[]) => ranges.map(rangeKey);
	const ranges = new Ranges();
	const held: Range[] = [];
	let before: string[] = [];
	for (let step = 0; step < 20_000; step++) {
		const grow = held.length < 24 ? 0.65 : 0.35;
		let changed: boolean;
		if (held.length === 0 || random(1) < grow) {
			const range: Range = {};
			const from = start();
			if (from !== undefined) range.start = from;
			if (random(1) < 0.6) range.end = BigInt(int(12));
			const priority = int(3);
			if (priority !== 0) range.priority = priority;
			if (random(1) < 0.5) range.order = "ascending";
			held.push(range);
			changed = ranges.add(range);
		} else {
			const index = int(held.length);
			const [range] = held.splice(index, 1);
			changed = ranges.remove(range);
		}
		const after = keys(union(held));
		expect(keys(ranges.union())).toEqual(after);
		expect(changed).toBe(!Bun.deepEquals(before, after));
		before = after;
	}
});

// A live join reaches back only by its lag, so the history beside it stays asked, and each lag is a
// range of its own, unless a reader starts at the oldest group. Mirrors the Rust
// `combined_ranges_union`.
test("a live join covers nothing but itself", () => {
	const two = { start: { live: { min: Duration.secs(2), max: Duration.ZERO } } };
	const five = { start: { live: { min: Duration.secs(5), max: Duration.ZERO } } };
	const history = { start: 2n, end: 4n };
	expect(union([history, two, five, two]).map(rangeKey)).toEqual([history, two, five].map(rangeKey));
	expect(union([history, two, {}])).toEqual([{}]);
	// A join with an end is a range of its own too, apart from the same lag without one.
	const bounded = { ...two, end: 9n };
	expect(union([two, bounded]).map(rangeKey)).toEqual([two, bounded].map(rangeKey));
});

// A range folds only into ranges at a policy covering its own: a higher priority, or the same one in
// its order or newest first. Mirrors the Rust `a_range_folds_only_into_a_covering_policy`.
test("a range folds only into a covering policy", () => {
	const at = (range: Range, priority: number, order: "ascending" | "descending"): Range => {
		const out: Range = { ...range };
		if (priority !== 0) out.priority = priority;
		if (order === "ascending") out.order = order;
		return out;
	};
	const wide = { start: 0n };
	const narrow = { start: 5n, end: 9n };
	// A higher priority covers a lower one in either order.
	expect(union([at(wide, 2, "ascending"), at(narrow, 1, "descending")])).toEqual([at(wide, 2, "ascending")]);
	// Newest first covers oldest first at one priority, never the other way around.
	expect(union([at(wide, 1, "descending"), at(narrow, 1, "ascending")])).toEqual([at(wide, 1, "descending")]);
	expect(union([at(wide, 1, "ascending"), at(narrow, 1, "descending")])).toEqual([
		at(wide, 1, "ascending"),
		at(narrow, 1, "descending"),
	]);
	// A lower priority never covers a higher one, and both stay asked.
	expect(union([at(wide, 0, "descending"), at(narrow, 3, "ascending")])).toEqual([
		at(wide, 0, "descending"),
		at(narrow, 3, "ascending"),
	]);
	// Changing one range's policy moves the union by that range alone.
	const ranges = new Ranges();
	expect(ranges.add(at(wide, 0, "descending"))).toBe(true);
	expect(ranges.add(at(narrow, 0, "descending"))).toBe(false);
	expect(ranges.add(at(narrow, 3, "descending"))).toBe(true);
	expect(ranges.remove(at(narrow, 0, "descending"))).toBe(false);
	expect(ranges.union()).toEqual([at(wide, 0, "descending"), at(narrow, 3, "descending")]);
});

// A range from the oldest group with an end takes in every one ending before it, so only the furthest
// end moves the union. Mirrors the Rust `ranges_from_the_oldest_keep_only_the_furthest_end`.
test("ranges from the oldest keep only the furthest end", () => {
	const oldest = (end: bigint): Range => ({ end });
	expect(union([oldest(5n), oldest(3n), oldest(5n)])).toEqual([oldest(5n)]);
	const ranges = new Ranges();
	expect(ranges.add(oldest(3n))).toBe(true);
	expect(ranges.add(oldest(5n))).toBe(true);
	expect(ranges.add(oldest(4n))).toBe(false);
	expect(ranges.add(oldest(5n))).toBe(false);
	expect(ranges.union()).toEqual([oldest(5n)]);
	expect(ranges.remove(oldest(5n))).toBe(false);
	expect(ranges.remove(oldest(3n))).toBe(false);
	expect(ranges.remove(oldest(5n))).toBe(true);
	expect(ranges.union()).toEqual([oldest(4n)]);
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
