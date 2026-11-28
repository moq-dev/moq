import { expect, test } from "bun:test";
import { insertRun, type Run, removeRun, runGaps } from "../internal.ts";
import { Writer } from "../stream.ts";
import { type Budget, budget as budgetOf, fit, fitAfter, UPDATE_FIELDS } from "./fit.ts";
import { MAX_MESSAGE_SIZE } from "./message.ts";
import { LiveRanges, rangeSize, SubscribeUpdate, type WireAnchor, type WireRange } from "./subscribe.ts";
import { Version } from "./version.ts";

const version = Version.DRAFT_07;

function budget(bytes: number): Budget {
	return { bytes, id: 2 };
}

/** A seeded generator, so a failing step replays: splitmix32. */
function seeded(seed: number) {
	let state = seed >>> 0;
	const next = () => {
		state = (state + 0x9e3779b9) >>> 0;
		let z = state;
		z = Math.imul(z ^ (z >>> 16), 0x85ebca6b) >>> 0;
		z = Math.imul(z ^ (z >>> 13), 0xc2b2ae35) >>> 0;
		return (z ^ (z >>> 16)) >>> 0;
	};
	return {
		/** In `[low, high)`. */
		int: (low: number, high: number) => low + (next() % (high - low)),
		/** In `[0, 2^bits)`, for up to 64 bits. */
		bits: (bits: number) => ((BigInt(next()) << 32n) | BigInt(next())) & ((1n << BigInt(bits)) - 1n),
		chance: (p: number) => next() / 2 ** 32 < p,
	};
}

type Rng = ReturnType<typeof seeded>;

function fillRange(run: Run): WireRange {
	return { start: { absolute: run.start }, end: { absolute: run.end } };
}

function total(budget: Budget, ranges: readonly WireRange[], groups: readonly Run[]): number {
	return [...ranges, ...groups.map(fillRange)].reduce((sum, range) => sum + rangeSize(range) + budget.id, 0);
}

/** The group a start names, or the exclusive group an end stops before. */
function group(anchor: WireAnchor | undefined, end: boolean): bigint | undefined {
	if (anchor?.absolute !== undefined) return anchor.absolute;
	if (anchor?.time !== undefined) return end ? anchor.time + 1n : anchor.time;
	return undefined;
}

/**
 * Whether `outer` asks for everything `inner` does, judged without the latest group: a range with
 * no start covers only another with none, which starts where it does.
 */
function covers(outer: WireRange, inner: WireRange): boolean {
	const start = (() => {
		if (outer.start === undefined) return inner.start === undefined;
		const [a, b] = [group(outer.start, false), group(inner.start, false)];
		return a !== undefined && b !== undefined && a <= b;
	})();
	const end = (() => {
		if (outer.end === undefined) return true;
		const [a, b] = [group(outer.end, true), group(inner.end, true)];
		return a !== undefined && b !== undefined && a >= b;
	})();
	return start && end;
}

function random(rng: Rng): { demand: WireRange[]; fills: Run[]; avoid: Run[] } {
	const start = () => BigInt(rng.int(0, 2_000)) << BigInt(rng.int(0, 30));
	const demand: WireRange[] = [];
	for (let count = rng.int(0, 300); count > 0; count--) {
		const from = start();
		const to = rng.chance(0.9) ? from + BigInt(rng.int(1, 50)) : undefined;
		const end: WireAnchor | undefined = to === undefined ? undefined : { absolute: to };
		const kind = rng.int(0, 10);
		if (kind < 2) demand.push({ end });
		// The demand names groups by time, inclusive as an end; a fill's are absolute.
		else if (kind < 6)
			demand.push({ start: { time: from }, end: to === undefined ? undefined : { time: to - 1n } });
		else demand.push({ start: { absolute: from }, end });
	}
	for (const range of demand) if (range.end === undefined) delete range.end;
	let fills: Run[] = [];
	const avoid: Run[] = [];
	for (let count = rng.int(0, 300); count > 0; count--) {
		const from = start();
		const to = from + BigInt(rng.int(1, 20));
		if (rng.chance(0.8)) insertRun(fills, from, to);
		else insertRun(avoid, from, to);
	}
	for (const run of avoid) fills = removeRun(fills, run.start, run.end);
	return { demand, fills, avoid };
}

// What fits stays as asked; what doesn't is widened to a covering superset that fits, never asking
// for a group the fills avoid while a narrower choice remains. Mirrors the Rust
// `widens_only_what_does_not_fit`.
test("fit widens only what does not fit", () => {
	const rng = seeded(0x666974);
	for (let step = 0; step < 2_000; step++) {
		const { demand, fills, avoid } = random(rng);
		const limit = budget(rng.int(200, 4_000));
		const before = total(limit, demand, fills);
		const got = fit(demand, fills, avoid, limit);
		if (before <= limit.bytes) {
			expect(got.ranges).toBe(demand);
			expect(got.fills).toBe(fills);
			continue;
		}
		expect(total(limit, got.ranges, got.fills)).toBeLessThanOrEqual(limit.bytes);
		for (const range of demand) {
			if (!got.ranges.some((outer) => covers(outer, range)))
				throw new Error(
					`step ${step}: ${JSON.stringify(range, (_, v) => (typeof v === "bigint" ? `${v}` : v))}`,
				);
		}
		for (const run of fills) expect(runGaps(got.fills, run.start, run.end)).toEqual([]);
	}
});

// Two fitted sets fit one SUBSCRIBE_UPDATE between them however little they share, at its widest
// fields, and either fits a SUBSCRIBE. Mirrors the Rust
// `any_edit_between_fitted_ranges_fits_one_message`.
test("fit leaves any edit between fitted ranges one message", async () => {
	const widest = (edit: { remove: bigint[]; add: WireRange[] }) =>
		new SubscribeUpdate({
			wantedGroup: (1n << 64n) - 2n,
			priority: -128,
			maxDelay: Number.MAX_SAFE_INTEGER,
			order: "ascending",
			remove: edit.remove,
			ranges: edit.add,
		});
	const encode = (update: SubscribeUpdate) => Writer.collect(version, (w) => update.encode(w, version));
	expect((await encode(widest({ remove: [], add: [] }))).byteLength).toBeLessThanOrEqual(UPDATE_FIELDS);

	// Sets of many short ranges and of fewer long ones, alternately, so the IDs one retires outweigh
	// what the other's ranges leave.
	const rng = seeded(0x65646974);
	const wide = (next: bigint, bits: number) => {
		const ranges: WireRange[] = Array.from({ length: 10_000 }, () => {
			const start = rng.bits(bits);
			return { start: { time: start }, end: { time: start } };
		});
		return [...fit(ranges, [], [], budgetOf(0, next, ranges.length)).ranges];
	};
	const live = new LiveRanges(version, wide(0n, 14));
	for (let round = 0; round < 4; round++) {
		const next = wide(live.next(), [50, 14][round % 2]);
		await encode(widest(live.edit(next)));
	}
});

// The range fitAfter takes past a full budget still fits the message beside the fields a SUBSCRIBE
// carries, however much of it those take. Mirrors the Rust
// `a_range_past_a_full_budget_fits_the_message`.
test("a range past a full budget fits the message", () => {
	const small: WireRange = { start: { absolute: 0n }, end: { absolute: 1n } };
	const probe: WireRange = { start: { absolute: (1n << 64n) - 3n }, end: { absolute: (1n << 64n) - 2n } };
	for (const fields of [0, Math.floor(MAX_MESSAGE_SIZE / 2), MAX_MESSAGE_SIZE - 100]) {
		const limit = budgetOf(fields, 0n, 1_000);
		const cost = (ranges: readonly WireRange[]) => total(limit, ranges, []);
		// As many as the budget holds.
		const spent = Array.from({ length: Math.floor(limit.bytes / cost([small])) }, () => small);
		const taken = fitAfter(limit, spent, [probe]);
		expect(taken).toEqual([probe]);
		expect(fields + cost(spent) + cost(taken)).toBeLessThanOrEqual(MAX_MESSAGE_SIZE);
	}
});

// The ranges with no start collapse to the one range covering them once nothing else fits.
// Mirrors the Rust `collapses_ranges_with_no_gap_to_close`.
test("fit collapses ranges with no gap to close", () => {
	const unstarted: WireRange[] = Array.from({ length: 200 }, (_, n) => ({
		end: { absolute: 1_000n + BigInt(n) },
	}));
	expect(fit(unstarted, [], [], budget(100)).ranges).toEqual([{ end: { absolute: 1_199n } }]);
	expect(() => fit(unstarted, [], [], budget(2))).toThrow("too many ranges");
});

// The narrowest gaps close first, so the fewest unwanted groups are asked for. Mirrors the Rust
// `closes_the_narrowest_gaps_first`.
test("fit closes the narrowest gaps first", () => {
	const runs = (...pairs: [number, number][]): Run[] =>
		pairs.map(([start, end]) => ({ start: BigInt(start), end: BigInt(end) }));
	const fills = runs([0, 1], [2, 3], [100, 101], [103, 104], [1_000, 1_001]);
	const fits = total(budget(0), [], runs([0, 3], [100, 104], [1_000, 1_001]));
	expect(fit([], fills, [], budget(fits)).fills).toEqual(runs([0, 3], [100, 104], [1_000, 1_001]));
	// A gap the fills avoid closes last, however narrow.
	expect(fit([], fills, runs([1, 2]), budget(fits)).fills).toEqual(runs([0, 1], [2, 104], [1_000, 1_001]));
});
