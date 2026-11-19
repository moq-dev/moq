import { describe, expect, test } from "bun:test";
import type { Anchor, Range } from "./track.ts";
import { UNBOUNDED } from "./util/key.ts";
import {
	AT_KEYS,
	type Containing,
	clamped,
	EMPTY,
	type Holdings,
	isEmpty,
	isWire,
	joinsLive,
	LIVE,
	type Run,
	rangesEnd,
	rangesOf,
	rangesStart,
	span,
	UNHELD,
	type Unplaced,
	union,
	Window,
	wireRanges,
	wireSpan,
} from "./window.ts";

const time = (start: bigint): Anchor => ({ time: start });
const relative = (back: bigint): Anchor => ({ relative: back });

/** A range from `start` through `end`, an omitted start being the oldest and an omitted end open. */
const range = (start: Anchor | undefined, end?: Anchor): Range => {
	const out: Range = {};
	if (start !== undefined) out.start = start;
	if (end !== undefined) out.end = end;
	return out;
};

/** Groups from `start` up to the exclusive `end`, or onward for undefined. */
const groups = (start: bigint, end?: bigint): Range =>
	range(time(start), end === undefined ? undefined : time(end - 1n));

const ascending = (a: bigint, b: bigint) => (a < b ? -1 : a > b ? 1 : 0);

/**
 * The group starts a hop holds, the key runs its producer skipped, whether a handler serves its
 * history, and where the dropped prefix ends, which places a time below it before every group.
 */
class Keys implements Holdings {
	held: Set<bigint>;
	skipped: Run[] = [];
	done = false;
	looks = false;
	dropped = 0n;

	constructor(held: Iterable<bigint> = []) {
		this.held = new Set(held);
	}

	#sorted(): bigint[] {
		return [...this.held].sort(ascending);
	}

	resolves(): boolean {
		return true;
	}

	containing(at: bigint): Containing {
		const sorted = this.#sorted();
		const group = sorted.filter((key) => key <= at).at(-1);
		if (group !== undefined) return { group };
		return sorted.length === 0 && at >= this.dropped ? "unknown" : "before";
	}

	unplaced(): Unplaced {
		return { runs: this.held.size === 0 ? [{ start: this.dropped, end: UNBOUNDED }] : [], mark: 0 };
	}

	nextStart(from: bigint): bigint | undefined {
		return this.#sorted().find((key) => key >= from);
	}

	back(latest: bigint, back: bigint): bigint | undefined {
		if (back === 0n) return latest;
		const below = this.#sorted()
			.filter((key) => key < latest)
			.reverse();
		return back - 1n < BigInt(below.length) ? below[Number(back - 1n)] : undefined;
	}

	complete(): boolean {
		return this.done;
	}

	looksUp(): boolean {
		return this.looks;
	}

	neverProduced(start: bigint, end: bigint): boolean {
		return end <= start || this.skipped.some((run) => run.start <= start && end <= run.end);
	}
}

/** A hop holding every group from 0 through the latest, as contiguous starts do. */
const CONTIGUOUS: Holdings = {
	resolves: () => true,
	containing: (at) => ({ group: at }),
	nextStart: (from) => from,
	back: (latest, back) => (latest >= back ? latest - back : undefined),
	complete: () => false,
};

const keys = (from: bigint, to: bigint): bigint[] => {
	const out: bigint[] = [];
	for (let key = from; key < to; key++) out.push(key);
	return out;
};

const covered = (window: Window, of: Iterable<bigint>): bigint[] => [...of].filter((key) => window.contains(key));

describe("Window", () => {
	test("disjoint ranges cover only their groups; the gap between them is out", () => {
		const held = new Keys(keys(0n, 10n));
		const window = new Window([groups(2n, 4n), groups(7n)], 9n, held);
		expect(covered(window, keys(0n, 10n))).toEqual([2n, 3n, 7n, 8n, 9n]);
		expect(window.floor()).toBe(2n);
		expect(window.end()).toBeUndefined();
		expect(window.runs(3n)).toEqual([
			{ start: 3n, end: 4n },
			{ start: 7n, end: UNBOUNDED },
		]);
		expect(window.runs(0n, 10n)).toEqual([
			{ start: 2n, end: 4n },
			{ start: 7n, end: 10n },
		]);
		expect(window.runs(4n, 7n)).toEqual([]);
		expect(window.next(4n)).toBe(7n);
		expect(window.resolved()).toEqual([
			{ start: 2n, end: 4n, floored: true },
			{ start: 7n, end: undefined, floored: true },
		]);
		// Delivery starting inside a range raises its start; past its end leaves it none.
		expect(window.starts(3n)).toEqual([3n, 7n]);
		expect(window.starts(5n)).toEqual([undefined, 7n]);

		// Nothing asked for is nothing in range, floored nowhere.
		const empty = new Window([], undefined, held);
		expect(covered(empty, keys(0n, 4n))).toEqual([]);
		expect([empty.floor(), empty.end()]).toEqual([0n, 0n]);
	});

	test("time anchors name groups by their starts", () => {
		let held = new Keys([0n, 33n, 66n, 99n]);
		let window = new Window([range(time(50n), time(70n))], 99n, held);
		expect(covered(window, held.held)).toEqual([33n, 66n]);
		expect(window.settled).toBe(true);
		expect(window.resolved()).toEqual([{ start: 33n, end: 71n, floored: true }]);

		// A start exactly on a group names that group, and an end on one includes it.
		window = new Window([range(time(33n), time(66n))], 99n, held);
		expect(covered(window, held.held)).toEqual([33n, 66n]);

		// An end before its start is empty.
		window = new Window([range(time(66n), time(33n))], 99n, held);
		expect(covered(window, held.held)).toEqual([]);

		// Before every start held: reach back to the group containing it, naming no floor.
		held = new Keys([10n, 20n]);
		window = new Window([range(time(5n))], 20n, held);
		expect(covered(window, held.held)).toEqual([10n, 20n]);
		expect([window.floor(), window.start()]).toEqual([0n, undefined]);
		// Nothing below its time.
		expect(window.missable(0n)[0]).toEqual({ start: 5n, end: UNBOUNDED });
	});

	test("a reported start moves a relative start, never a relative end", () => {
		const held = new Keys(keys(0n, 10n));
		// The publisher reported starting the range counting back 2 from 9 at 8, past the 7 counted.
		const reported: Holdings = {
			resolves: () => true,
			containing: (at) => held.containing(at),
			nextStart: (from) => held.nextStart(from),
			back: (latest, back) => held.back(latest, back),
			reportedStart: (latest, back) => (latest === 9n && back === 2n ? 8n : undefined),
			complete: () => false,
		};
		expect(new Window([range(relative(2n))], 9n, reported).start()).toBe(8n);
		const ended = new Window([range(time(0n), relative(2n))], 9n, reported);
		expect(covered(ended, held.held)).toEqual(keys(0n, 8n));
	});

	test("a relative start past every group asks from the oldest group held when placed", () => {
		const held = new Keys([33n, 66n, 99n]);
		const window = new Window([range(relative(10n))], 99n, held);
		expect(window.missable()).toEqual([{ start: 33n, end: UNBOUNDED }]);
		// Neither an older group arriving nor the oldest evicted moves it, as in Rust.
		held.held.add(20n);
		expect(window.settle(99n, held)).toBe(false);
		held.held.delete(20n);
		held.held.delete(33n);
		expect(window.settle(99n, held)).toBe(false);
		expect(window.missable()).toEqual([{ start: 33n, end: UNBOUNDED }]);
	});

	test("relative anchors count groups back from the latest", () => {
		let held = new Keys([0n, 33n, 66n, 99n]);
		let window = new Window([range(relative(2n), relative(1n))], 99n, held);
		expect(covered(window, held.held)).toEqual([33n, 66n]);
		expect(window.start()).toBe(33n);

		// Counting back past every group held reaches back to the oldest as a start, and ends
		// before them all as an end.
		window = new Window([range(relative(10n))], 99n, held);
		expect(covered(window, held.held)).toEqual([0n, 33n, 66n, 99n]);
		expect(window.start()).toBeUndefined();
		window = new Window([range(undefined, relative(10n))], 99n, held);
		expect(covered(window, held.held)).toEqual([]);

		window = new Window([LIVE], undefined, new Keys());
		expect([window.pending, window.contains(3n), window.floor()]).toEqual([true, true, undefined]);
		expect(window.settle(undefined, new Keys())).toBe(false);
		held = new Keys([5n]);
		expect(window.settle(5n, held)).toBe(true);
		expect(covered(window, keys(0n, 8n))).toEqual([5n, 6n, 7n]);
		expect(window.settled).toBe(true);
		// Resolved once: a later group never moves it.
		held.held.add(9n);
		expect(window.settle(9n, held)).toBe(false);

		// The oldest start needs no latest group, and names no floor.
		window = new Window([{}], undefined, held);
		expect([window.floor(), window.start()]).toEqual([0n, undefined]);

		// Any count back, however far, waits on a latest group.
		for (const back of [UNBOUNDED - 1n, UNBOUNDED - 2n]) {
			const open = range(relative(back));
			expect(isWire(open)).toBe(false);
			expect(new Window([open], undefined, held).pending).toBe(true);
			expect(new Window([open], 5n, held).start()).toBeUndefined();
		}
	});

	test("the named start is the lowest unless one reaches back", () => {
		const held = new Keys(keys(0n, 10n));
		const start = (ranges: Range[], latest: bigint | undefined) => new Window(ranges, latest, held).start();
		expect(start([groups(4n), LIVE], 9n)).toBe(4n);
		expect(start([groups(4n), LIVE], 2n)).toBe(2n);
		// Time 0 names group 0 whatever is held.
		expect(start([groups(0n)], undefined)).toBe(0n);
		expect(start([groups(4n), LIVE], undefined)).toBeUndefined();
		expect(start([groups(4n), {}], 9n)).toBeUndefined();
	});

	test("a time start waits for a group", () => {
		let held = new Keys();
		const ranges = [range(time(2500n)), range(relative(0n), relative(0n))];
		let window = new Window(ranges, 9n, held);
		expect([window.pending, window.settled]).toEqual([false, false]);
		// Only the other range reads below it.
		expect(covered(window, keys(0n, 22n))).toEqual([9n]);
		// The groups from the time are in range whatever contains it.
		expect(covered(window, keys(2498n, 2502n))).toEqual([2500n, 2501n]);
		expect([window.floor(), window.start(), window.end()]).toEqual([undefined, undefined, undefined]);
		expect(window.resolved()).toEqual([undefined, { start: 9n, end: 10n, floored: true }]);
		expect(window.settle(9n, held)).toBe(false);

		// 3000 starts past it.
		held.held.add(3000n);
		expect(window.settle(3000n, held)).toBe(true);
		expect(window.resolved()[0]).toEqual({ start: 0n, end: undefined, floored: false });
		held.held.add(2000n);
		window = new Window(ranges, 3000n, held);
		expect(window.resolved()[0]).toEqual({ start: 2000n, end: undefined, floored: true });
		expect(window.settled).toBe(true);

		// Nothing can arrive any more: it reaches back to the oldest.
		held = new Keys();
		held.done = true;
		window = new Window(ranges, 9n, held);
		expect(window.resolved()[0]).toEqual({ start: 0n, end: undefined, floored: false });
	});

	test("a waiting time start keeps its relative end", () => {
		for (const latest of [undefined, 9n]) {
			const held = new Keys();
			const window = new Window([range(time(2500n), relative(0n))], latest, held);
			expect(window.pending).toBe(latest === undefined);
			held.held.add(2000n);
			held.held.add(3000n);
			expect(window.settle(3000n, held)).toBe(true);
			expect(window.pending).toBe(false);
			held.held.add(4000n);
			// Resolved already.
			expect(window.settle(4000n, held)).toBe(false);
			const resolved = window.resolved()[0];
			expect([resolved?.start, resolved?.end]).toEqual([2000n, latest === undefined ? 3001n : latest + 1n]);
		}
	});

	test("a time start the groups can't place is looked up", () => {
		const held = new Keys([4000n, 9000n]);
		held.looks = true;
		let window = new Window([range(time(8500n))], 9000n, held);
		expect(window.lookups()).toEqual([{ time: 8500n, below: 9000n }]);
		expect([window.settled, window.contains(9000n), window.resolved()]).toEqual([false, false, [undefined]]);
		held.held.add(8000n);
		expect(window.found(8500n, 8000n, held)).toBe(true);
		expect(window.resolved()).toEqual([{ start: 8000n, end: undefined, floored: true }]);
		expect(window.settled).toBe(true);

		// Something may start between 8000 and 8500.
		expect(new Window([range(time(8500n))], 9000n, held).lookups()).toHaveLength(1);
		// 8000 starts exactly there.
		expect(new Window([range(time(8000n))], 9000n, held).lookups()).toHaveLength(0);

		window = new Window([range(time(500n))], 9000n, held);
		expect(window.lookups()).toEqual([{ time: 500n, below: 4000n }]);
		expect(window.found(500n, undefined, held)).toBe(true);
		expect(window.resolved()).toEqual([{ start: 0n, end: undefined, floored: false }]);
		expect(window.contains(4000n)).toBe(true);
		expect(window.missable(0n).some((run) => run.start < 500n)).toBe(false);

		// A lookup nobody asked for is ignored.
		expect(window.found(500n, undefined, held)).toBe(false);
	});

	test("a skipped gap needs no lookup", () => {
		const held = new Keys([0n, 33n, 66n]);
		held.looks = true;
		held.skipped = [
			{ start: 1n, end: 33n },
			{ start: 34n, end: 66n },
		];
		const window = new Window([range(time(50n))], 66n, held);
		expect(window.lookups()).toHaveLength(0);
		expect(window.resolved()[0]?.start).toBe(33n);
	});

	test("time 0 names group 0 without a lookup", () => {
		const held = new Keys([5n]);
		held.looks = true;
		const window = new Window([range(time(0n))], 5n, held);
		expect(window.lookups()).toEqual([]);
		expect(window.resolved()).toEqual([{ start: 0n, end: undefined, floored: true }]);
	});

	test("an unanswered lookup settles with its time end", () => {
		const held = new Keys([4000n, 9000n]);
		held.looks = true;
		const window = new Window([range(time(500n), time(3000n))], 9000n, held);
		expect(window.lookups()).toEqual([{ time: 500n, below: 4000n }]);
		expect(window.found(500n, undefined, held)).toBe(true);
		expect(window.settled).toBe(true);
		expect(window.resolved()).toEqual([{ start: 0n, end: 3001n, floored: false }]);
	});

	test("a time start is not looked up without a history", () => {
		const held = new Keys([4000n, 9000n]);
		const window = new Window([range(time(8500n)), range(time(500n))], 9000n, held);
		expect(window.lookups()).toHaveLength(0);
		expect(window.resolved()).toEqual([
			{ start: 4000n, end: undefined, floored: true },
			{ start: 0n, end: undefined, floored: false },
		]);
	});

	test("relative starts resolve against the latest group", () => {
		const floor = (ranges: Range[], latest: bigint | undefined) => new Window(ranges, latest, CONTIGUOUS).floor();

		expect(floor([LIVE], 7n)).toBe(7n);
		expect(floor([LIVE], undefined)).toBeUndefined();

		expect(floor([range(relative(3n))], 7n)).toBe(4n);
		// An offset past the start clamps.
		expect(floor([range(relative(3n))], 1n)).toBe(0n);

		expect(floor([{}], undefined)).toBe(0n);

		// The lowest range wins, and one unresolved range leaves the whole floor unresolved.
		expect(floor([groups(4n), LIVE], 3n)).toBe(3n);
		expect(floor([groups(4n), LIVE], 9n)).toBe(4n);
		expect(floor([groups(4n), LIVE], undefined)).toBeUndefined();

		// Nothing asked for floors nowhere, and an empty range asks for nothing.
		expect(floor([], undefined)).toBe(0n);
		expect(floor([groups(5n, 5n), groups(10n)], undefined)).toBe(10n);
	});

	test("a hop holding no groups leaves time anchors to the copies, and one at keys names them", () => {
		// Before any group, as a relative anchor waits.
		expect(new Window([groups(4n)], undefined, UNHELD).pending).toBe(true);
		expect(new Window([groups(4n)], undefined).pending).toBe(true);

		// A bounded window ends past its last group, and nothing in range ends at 0.
		expect(new Window([groups(2n, 4n), groups(6n, 9n)], undefined, AT_KEYS).end()).toBe(9n);
		expect(new Window([], undefined, AT_KEYS).end()).toBe(0n);
		// The oldest start names no floor.
		expect(new Window([range(undefined, time(4n))], undefined, AT_KEYS).start()).toBeUndefined();
	});
});

describe("ranges", () => {
	test("legacy spans become inclusive time ranges", () => {
		expect(span(2n, 6n)).toEqual(range(time(2n), time(5n)));
		expect(span(undefined, 6n)).toEqual(range(undefined, time(5n)));
		expect(span(2n, undefined)).toEqual(range(time(2n)));
		expect(span(undefined, undefined)).toEqual({});
		expect(span(0n, 0n)).toBeUndefined();
		expect(span(undefined, 0n)).toBeUndefined();
		expect(span(5n, 5n)).toBeUndefined();
		expect(span(0n, 1n)).toEqual(range(time(0n), time(0n)));
		expect(isEmpty(EMPTY)).toBe(true);
	});

	test("bounds project the ranges for older wires", () => {
		const bounds = (ranges: Range[]) => [rangesStart(ranges), rangesEnd(ranges)];
		expect(bounds([{}])).toEqual([undefined, undefined]);
		expect(bounds([groups(2n, 4n), groups(7n, 9n)])).toEqual([2n, 9n]);
		// An explicit start at 0 is a floor, unlike the oldest.
		expect(bounds([groups(0n)])).toEqual([0n, undefined]);
		expect(bounds([groups(2n, 4n), LIVE])).toEqual([undefined, undefined]);
		expect(bounds([range(time(2n), relative(0n))])).toEqual([2n, undefined]);
		// Nothing asked for is the empty range.
		expect(rangesEnd([])).toBe(0n);
	});

	test("the union merges time ranges and drops what an open range covers", () => {
		expect(union([groups(2n, 5n), groups(4n, 8n)])).toEqual([groups(2n, 8n)]);
		// Adjacent ranges merge.
		expect(union([groups(2n, 5n), groups(5n, 8n)])).toEqual([groups(2n, 8n)]);
		// A gap stays a gap.
		expect(union([groups(2n, 5n), groups(6n, 8n)])).toEqual([groups(2n, 5n), groups(6n, 8n)]);
		expect(union([groups(2n, 5n), groups(9n), groups(4n)])).toEqual([groups(2n)]);
		expect(union([groups(4n, 6n), groups(0n, 2n), groups(2n, 3n), groups(5n, 9n)])).toEqual([
			groups(0n, 3n),
			groups(4n, 9n),
		]);
		// The oldest start covers everything.
		expect(union([groups(2n, 5n), {}])).toEqual([{}]);

		// Relative ranges count back from the same latest group, so the widest open one covers
		// every narrower one, whatever its end.
		expect(union([LIVE, range(relative(3n)), range(relative(1n), relative(0n))])).toEqual([range(relative(3n))]);

		// The open time span drops a relative-ended range starting inside it.
		expect(union([groups(3n), range(time(5n), relative(0n))])).toEqual([groups(3n)]);

		// A time floor above the live edge does not hide the live edge: both stay, relative first.
		expect(union([groups(4n), LIVE])).toEqual([LIVE, groups(4n)]);

		// Empty ranges ask for nothing.
		expect(union([groups(5n, 5n)])).toEqual([]);
		expect(union([groups(1n, 2n)])).toEqual([groups(1n, 2n)]);
		expect(union([groups(4n, 4n), range(time(3n), time(1n))])).toEqual([]);
	});

	test("the union is order-independent and settles", () => {
		const a = [groups(2n, 5n), LIVE];
		const b = [groups(4n, 9n)];
		const combined = union([...a, ...b]);
		expect(union([...b, ...a])).toEqual(combined);
		// A subset adds nothing.
		expect(union([...combined, ...a])).toEqual(combined);
	});

	test("a wire without ranges carries one span covering them all", () => {
		expect(wireSpan([groups(4n, 6n), groups(2n, 3n)])).toEqual({ start: 2n, end: 6n });
		expect(wireSpan([LIVE])).toEqual({ start: undefined, end: undefined });
		expect(wireSpan([range(relative(1n))]).start).toBeUndefined();
		// No single start covers both a time and a relative one.
		expect(wireSpan([LIVE, groups(5n, 6n)])).toEqual({ start: 0n, end: undefined });
	});

	test("wire ranges drop empty ones and ask a relative end through the live edge", () => {
		expect(wireRanges([groups(5n, 6n)])).toEqual([groups(5n, 6n)]);
		expect(wireRanges([range(time(5n), time(9n))])).toEqual([range(time(5n), time(9n))]);
		// An end before its start asks for nothing.
		expect(wireRanges([range(time(5n), time(4n))])).toEqual([]);
		// A relative end goes out open, since the copy resolves it against what it receives.
		expect(wireRanges([range(relative(3n), relative(1n)), groups(5n, 5n)])).toEqual([range(relative(3n))]);
		expect(wireRanges([range(undefined, relative(1n))])).toEqual([{}]);
	});

	test("only time anchors and the live and oldest starts survive a wire without ranges", () => {
		expect(isWire({})).toBe(true);
		expect(isWire(LIVE)).toBe(true);
		expect(isWire(groups(1n, 5n))).toBe(true);
		expect(isWire(range(relative(1n)))).toBe(false);
		expect(isWire(range(time(1n), relative(0n)))).toBe(false);
	});

	test("clamping raises starts to the floor and caps ends without widening", () => {
		const ranges: Range[] = [groups(1n, 9n), {}, range(relative(2n), relative(0n))];
		expect(clamped(ranges, 4n, 6n, [undefined, undefined, { start: 8n, end: 11n, floored: true }])).toEqual([
			groups(4n, 6n),
			// The oldest start keeps its anchor: the floor may lie before it.
			range(undefined, time(5n)),
			range(time(8n), time(5n)),
		]);
		// A relative start not resolved keeps its anchor for the caller to floor.
		expect(clamped([range(relative(2n))], 4n, undefined, [])).toEqual([range(relative(2n))]);
		// Resolved anchors become their groups; an unresolved time end is capped apart.
		expect(clamped([range(time(1n), time(5n))], 2n, 9n, [{ start: 3n, end: 7n, floored: true }])).toEqual([
			range(time(3n), time(6n)),
		]);
		expect(clamped([range(time(1n), time(5n))], 2n, 9n, [])).toEqual([range(time(2n), time(5n))]);
		// A cap of 0, or a resolved end of 0, holds nothing.
		expect(clamped([groups(1n)], undefined, 0n, [])).toEqual([EMPTY]);
		expect(
			clamped([range(relative(5n), relative(4n))], 0n, undefined, [{ start: 0n, end: 0n, floored: false }]),
		).toEqual([EMPTY]);
		// Nothing to clamp to returns the ranges as given.
		expect(clamped(ranges, undefined, undefined, [])).toEqual(ranges);
	});

	test("ranges are checked rather than served widened", () => {
		expect(rangesOf([range(time(1n))])).toEqual([range(time(1n))]);
		expect(() => rangesOf([{ start: { time: -1n } }])).toThrow(TypeError);
		expect(() => rangesOf([{ start: { absolute: 1n } as unknown as Anchor }])).toThrow(TypeError);
		expect(() => rangesOf([{ start: { time: 1n, relative: 2n } as unknown as Anchor }])).toThrow(TypeError);
		expect(() => rangesOf([{ end: { time: 5 } as unknown as Anchor }])).toThrow(TypeError);
		expect(() => rangesOf({} as unknown as Range[])).toThrow(TypeError);
		expect(() => rangesOf([null as unknown as Range])).toThrow(TypeError);
	});

	test("a range counting back and following the live edge joins live", () => {
		expect(joinsLive(undefined)).toBe(true);
		expect(joinsLive([{}])).toBe(true);
		expect(joinsLive([LIVE])).toBe(true);
		expect(joinsLive([groups(3n)])).toBe(false);
		expect(joinsLive([range(relative(2n), relative(0n))])).toBe(false);
	});
});

/** {@link Keys} keeping its unplaced times to itself, so a window tries every start on each change. */
function blind(keys: Keys): Holdings {
	return {
		resolves: () => true,
		looksUp: () => keys.looksUp(),
		neverProduced: (start, end) => keys.neverProduced(start, end),
		complete: () => keys.complete(),
		containing: (at) => keys.containing(at),
		nextStart: (from) => keys.nextStart(from),
		back: (latest, back) => keys.back(latest, back),
	};
}

test("window: settling by steps matches trying every start and rebuilding from the spans", () => {
	let seed = 11;
	const random = (below: number) => {
		seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0;
		// The high bits: an LCG's low bits cycle.
		return (seed >>> 16) % below;
	};
	const seen = (window: Window) => ({
		runs: window.runs(),
		missable: window.missable(),
		bounds: [window.floor(), window.start(), window.end()],
		resolved: window.resolved(),
		lookups: window.lookups().map((lookup) => lookup.time),
		state: [window.settled, window.pending],
	});
	for (let round = 0; round < 3000; round++) {
		const ranges = Array.from({ length: 1 + random(11) }, () => {
			const start = random(5) === 0 ? relative(BigInt(random(6))) : time(BigInt(random(40)));
			return range(start, random(3) === 0 ? undefined : time(BigInt(random(48))));
		});
		const keys = new Keys();
		keys.looks = random(2) === 0;
		const fast = new Window(ranges, undefined, keys);
		const full = new Window(ranges, undefined, blind(keys));
		for (let step = random(24); step > 0; step--) {
			const value = BigInt(random(48));
			const op = random(5);
			if (op === 0) keys.held.add(value);
			else if (op === 1) {
				if (value > keys.dropped) keys.dropped = value;
			} else if (op === 2) {
				const lookup = fast.lookups()[0];
				if (lookup === undefined) continue;
				const answer = value % 2n === 0n ? value : undefined;
				fast.found(lookup.time, answer, keys);
				full.found(lookup.time, answer, blind(keys));
			} else if (op === 3) keys.done = true;
			else if (op === 4) keys.held.delete(keys.nextStart(0n) ?? 0n);
			const latest = [...keys.held].sort(ascending).at(-1);
			fast.settle(latest, keys);
			full.settle(latest, blind(keys));
			expect(seen(fast)).toEqual(seen(full));
			// A window waiting on a latest group reads everything, with no spans yet.
			if (fast.pending) continue;
			const fresh = fast.fresh();
			expect(fast.runs()).toEqual(fresh.runs);
			expect(fast.missable()).toEqual(fresh.missable);
			expect(fast.named()).toBe(fresh.named);
		}
	}
});

test("window: an edit by key matches building the edited ranges afresh", () => {
	let seed = 23;
	const random = (below: number) => {
		seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0;
		// The high bits: an LCG's low bits cycle.
		return (seed >>> 16) % below;
	};
	const any = (): Range => {
		const kind = random(6);
		const start = kind === 0 ? relative(BigInt(random(6))) : kind === 1 ? undefined : time(BigInt(random(40)));
		const end =
			random(3) === 0 ? undefined : random(5) === 0 ? relative(BigInt(random(4))) : time(BigInt(random(48)));
		return range(start, end);
	};
	const seen = (window: Window) => ({
		ranges: window.ranges,
		runs: window.runs(),
		missable: window.missable(),
		bounds: [window.floor(), window.start(), window.end()],
		resolved: window.resolved(),
		lookups: window.lookups().map((lookup) => lookup.time),
		state: [window.settled, window.pending, window.wired],
	});
	for (let round = 0; round < 2000; round++) {
		// Fixed holdings, so ranges left alone resolve as a fresh build resolves them.
		const keys = new Keys(Array.from({ length: random(8) }, () => BigInt(random(48))));
		keys.looks = random(2) === 0;
		const latest = [...keys.held].sort(ascending).at(-1);
		const held = new Map<bigint, Range>();
		let next = 0n;
		for (let count = random(8); count > 0; count--) held.set(next++, any());
		const window = Window.keyed(held, latest, keys);
		for (let step = random(16); step > 0; step--) {
			const retire: bigint[] = [];
			const add: [bigint, Range][] = [];
			for (const key of held.keys()) if (random(4) === 0) retire.push(key);
			for (const key of held.keys()) if (random(6) === 0) add.push([key, any()]);
			for (let count = random(3); count > 0; count--) add.push([next++, any()]);
			const readded = new Set(add.map(([key]) => key));
			for (const key of retire) if (!readded.has(key)) held.delete(key);
			for (const [key, range] of add) held.set(key, range);
			window.edit(retire, add, latest, keys);
			expect(seen(window)).toEqual(seen(Window.keyed(held, latest, keys)));
		}
	}
});
