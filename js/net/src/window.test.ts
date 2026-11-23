import { describe, expect, test } from "bun:test";
import { Duration } from "@moq/clock";
import { NO_MAX_DELAY } from "./internal.ts";
import type { Live, Range } from "./track.ts";
import { UNBOUNDED } from "./util/key.ts";
import {
	AT_KEYS,
	asked,
	type Containing,
	clamped,
	EMPTY,
	type Found,
	type Holdings,
	isEmpty,
	type Join,
	joinsLive,
	type Run,
	rangeKey,
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

const time = (start: bigint): bigint => start;

/** A range from `start` through `end`, an omitted start being the oldest group, and an omitted end open. */
const range = (start: bigint | undefined, end?: bigint): Range => {
	const out: Range = {};
	if (start !== undefined) out.start = start;
	if (end !== undefined) out.end = end;
	return out;
};

/** Groups from `start` up to the exclusive `end`, or onward for undefined. */
const groups = (start: bigint, end?: bigint): Range =>
	range(time(start), end === undefined ? undefined : time(end - 1n));

/** A live join `ms` behind the live edge, through `end` or onward for undefined. */
const live = (ms: number, end?: bigint): Range => {
	const out: Range = { start: { live: { min: Duration.millis(ms), max: Duration.ZERO } } };
	if (end !== undefined) out.end = end;
	return out;
};

const ascending = (a: bigint, b: bigint) => (a < b ? -1 : a > b ? 1 : 0);

/**
 * The group starts a hop holds, the key runs its producer skipped, whether a handler serves its
 * history, where the dropped prefix ends, which places a time below it before every group, and
 * where a live join by its lag starts.
 */
class Keys implements Holdings {
	held: Set<bigint>;
	skipped: Run[] = [];
	done = false;
	looks = false;
	dropped = 0n;
	joins?: (min: Duration, end: bigint | undefined, budget: number, since: number) => Join | undefined;
	// The tick of the upstream's answer to every live join, which alone places one when set.
	answered?: number;

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

	join(min: Duration, end: bigint | undefined, budget: number, since: number): Join | undefined {
		if (this.answered !== undefined && this.answered <= since) return undefined;
		return this.joins?.(min, end, budget, since);
	}

	answersJoins(): boolean {
		return this.answered !== undefined;
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

	test("a range with no start reaches back to the oldest group, and a live join by its lag", () => {
		const held = new Keys([0n, 33n, 66n, 99n]);
		held.joins = (min) => ({ time: 99n - BigInt(min.asMillis()) });
		// The oldest group, naming no floor, needing no latest group.
		let window = new Window([{}], undefined, held);
		expect([window.floor(), window.start(), window.settled]).toEqual([0n, undefined, true]);
		// A live join: a time start at the live edge minus its lag, zero being the latest group.
		window = new Window([live(0)], 99n, held);
		expect([covered(window, held.held), window.start()]).toEqual([[99n], 99n]);
		window = new Window([live(40)], 99n, held);
		expect([covered(window, held.held), window.start()]).toEqual([[33n, 66n, 99n], 33n]);
	});

	test("a live join looks its time start up, unlike a join to a group", () => {
		const held = new Keys([4000n, 9000n]);
		held.looks = true;
		held.joins = (min) => ({ time: 9000n - BigInt(min.asMillis()) });
		const window = new Window([live(500)], 9000n, held);
		expect(window.lookups()).toEqual([{ time: 8500n, floor: 4001n }]);
		expect(window.found(8500n, { kind: "group", group: 8000n }, held)).toBe(true);
		expect(window.resolved()).toEqual([{ start: 8000n, end: undefined, floored: true }]);

		held.joins = () => ({ group: 4000n });
		const group = new Window([live(500)], 9000n, held);
		expect([group.lookups(), group.resolved()]).toEqual([[], [{ start: 4000n, end: undefined, floored: true }]]);
	});

	test("a live join waits on a first group, then on the holdings", () => {
		const held = new Keys();
		let edge: bigint | undefined;
		held.joins = () => (edge === undefined ? undefined : { group: edge });
		const window = new Window([live(1)], undefined, held);
		// Before any group: everything is in range.
		expect([window.pending, window.contains(3n), window.floor()]).toEqual([true, true, undefined]);
		expect(window.settle(undefined, held)).toBe(false);
		// Groups held that can't tell where it starts: nothing is in range.
		held.held.add(5n);
		expect(window.settle(5n, held)).toBe(true);
		expect([window.pending, window.settled, window.contains(5n), window.start()]).toEqual([
			false,
			false,
			false,
			undefined,
		]);
		expect(window.unsettled()).toEqual(new Set([0n]));
		edge = 5n;
		expect(window.settle(5n, held)).toBe(true);
		expect(covered(window, keys(0n, 8n))).toEqual([5n, 6n, 7n]);
		expect(window.settled).toBe(true);
		// Resolved once: a later group never moves it.
		held.held.add(9n);
		edge = 9n;
		expect(window.settle(9n, held)).toBe(false);
		expect(window.start()).toBe(5n);
	});

	// Mirrors the Rust `a_copy_places_a_live_join_only_from_a_fresh_answer`.
	test("a copy places a live join only from an answer recorded after its range was added", () => {
		const held = new Keys([0n, 33n, 66n, 99n]);
		held.joins = () => ({ group: 33n });
		for (const lag of [0, 40]) {
			held.answered = 3;
			const window = Window.keyed([[0n, live(lag)]], 99n, held, () => ({ since: 3 }));
			expect([window.settled, window.contains(99n)]).toEqual([false, false]);
			expect(window.joins()).toEqual([[lag, undefined, 0, 3]]);

			// A newer answer places it, at exactly the group upstream answered.
			held.answered = 4;
			expect(window.settle(99n, held)).toBe(true);
			expect(covered(window, held.held)).toEqual([33n, 66n, 99n]);
			expect(window.joins()).toEqual([]);
		}

		// An answer of nothing places a join with an end past it, and keeps one without waiting.
		held.answered = 3;
		held.joins = () => "empty";
		const bounded = Window.keyed([[0n, live(40, 50n)]], 99n, held, () => ({ since: 3 }));
		const open = Window.keyed([[0n, live(40)]], 99n, held, () => ({ since: 3 }));
		held.answered = 4;
		expect(bounded.settle(99n, held)).toBe(true);
		expect(covered(bounded, held.held)).toEqual([]);
		expect(bounded.settled).toBe(true);
		expect(open.settle(99n, held)).toBe(false);
		expect(open.joins()).toEqual([[40, undefined, 0, 3]]);
	});

	// A live join already placed starts at that group on any hop, asking nothing and waiting on
	// nothing, and its groups still go stale past its max. Mirrors the Rust
	// `a_placed_join_starts_where_it_was_placed`.
	test("a placed join starts where it was placed", () => {
		const held = new Keys([0n, 33n, 66n, 99n]);
		held.joins = () => ({ group: 66n });
		held.answered = 3;
		const max = Duration.secs(2);
		const join: Range = { start: { live: { min: Duration.millis(40), max } } };
		const placed = () => ({ at: 33n });
		for (const window of [
			Window.keyed([[0n, join]], 99n, held, placed),
			Window.keyed([[0n, join]], undefined, UNHELD, placed),
		]) {
			expect(window.settled).toBe(true);
			expect(window.joins()).toEqual([]);
			expect(window.start()).toBe(33n);
			const budget = window.budget();
			expect([budget.at(66n), budget.widest()]).toEqual([2000, 2000]);
		}
		expect(covered(Window.keyed([[0n, join]], 99n, held, placed), held.held)).toEqual([33n, 66n, 99n]);

		// Placed elsewhere by an edit, it moves there; retuned in place, it stays.
		const window = Window.keyed([[0n, join]], 99n, held, placed);
		window.edit([0n], [[0n, join]], 99n, held, () => ({ at: 66n }));
		expect(window.start()).toBe(66n);
		const wider: Range = { start: { live: { min: Duration.millis(40), max: null } } };
		window.edit([0n], [[0n, wider]], 99n, held, () => ({ at: 66n }));
		expect([window.start(), window.budget().at(66n)]).toEqual([66n, NO_MAX_DELAY]);
	});

	// A group takes the widest budget among the ranges covering it, not among every range: a finite
	// live join alone keeps its max beside an unlimited one placed later, and a join still reaching
	// may cover any group up to its end. Mirrors the Rust `a_group_takes_the_widest_budget_covering_it`.
	test("a group takes the widest budget covering it", () => {
		const held = new Keys(keys(0n, 100n));
		held.answered = 0;
		const join = (min: number, max: Duration | null, end?: bigint): Range => ({
			start: { live: { min: Duration.millis(min), max } },
			...(end === undefined ? {} : { end }),
		});
		const ranges = new Map<bigint, Range>([
			[0n, join(40, Duration.millis(100))],
			[1n, join(20, null, 59n)],
			[2n, range(80n, 89n)],
			[3n, join(60, Duration.millis(500), 20n)],
		]);
		const placed = (key: bigint) => (key === 0n ? { at: 10n } : key === 1n ? { at: 50n } : { since: 0 });
		const window = Window.keyed(ranges, 99n, held, placed);
		expect(window.reaching).toBe(true);
		for (const budget of [window.budget(), window.budgetOf(ranges)]) {
			expect(budget.runs(0n, 100n)).toEqual([
				[{ start: 0n, end: 21n }, 500],
				[{ start: 21n, end: 50n }, 100],
				[{ start: 50n, end: 60n }, NO_MAX_DELAY],
				[{ start: 60n, end: 80n }, 100],
				[{ start: 80n, end: 90n }, NO_MAX_DELAY],
				[{ start: 90n, end: 100n }, 100],
			]);
			expect(budget.stretch(30n)).toEqual([100, 50n]);
			expect([budget.widest(), budget.isUniform()]).toEqual([NO_MAX_DELAY, false]);
			const clamped = budget.clamped(200);
			expect([clamped.at(55n), clamped.at(30n)]).toEqual([200, 100]);
		}
	});

	// A history range a hop holding nothing leaves to its copies keeps every group it may cover from
	// going stale before they place it, beside a live join judged by its max. Mirrors the Rust
	// `a_pending_history_range_keeps_what_it_may_cover`.
	test("a pending history range keeps what it may cover", () => {
		const history = range(5001n, 8999n);
		const join: Range = { start: { live: { min: Duration.ZERO, max: Duration.secs(1) } } };
		const window = new Window([history, join], undefined, UNHELD);
		expect(window.pending).toBe(true);
		for (const budget of [
			window.budget(),
			window.budgetOf(
				new Map([
					[0n, history],
					[1n, join],
				]),
			),
		]) {
			expect(budget.at(8000n)).toBe(NO_MAX_DELAY);
			// The copies may place it at any group.
			expect(budget.at(0n)).toBe(NO_MAX_DELAY);
			expect(budget.at(9000n)).toBe(1000);
		}
	});

	test("a zero lag starts a live join at the latest group, once there is one", () => {
		const held = new Keys(keys(0n, 8n));
		held.joins = () => ({ group: 2n });
		const window = new Window([live(0)], undefined, held);
		expect(window.pending).toBe(true);
		expect(window.settle(7n, held)).toBe(true);
		expect(window.start()).toBe(7n);
	});

	test("a hop that leaves its ranges to the copies it follows reaches a live join back", () => {
		// The copy it follows placed the join already, so every group it delivers is in range.
		const window = new Window([live(500)], 7n, AT_KEYS);
		expect([window.pending, window.floor(), window.contains(0n), window.contains(7n)]).toEqual([
			false,
			0n,
			true,
			true,
		]);
	});

	test("the named start is the lowest unless one reaches back", () => {
		const held = new Keys(keys(0n, 10n));
		held.joins = () => ({ group: 9n });
		const start = (ranges: Range[], latest: bigint | undefined) => new Window(ranges, latest, held).start();
		expect(start([groups(4n), live(0)], 9n)).toBe(4n);
		held.joins = () => ({ group: 2n });
		expect(start([groups(4n), live(1)], 2n)).toBe(2n);
		// Time 0 names group 0 whatever is held.
		expect(start([groups(0n)], undefined)).toBe(0n);
		// Waiting on a first group.
		held.joins = () => undefined;
		expect(start([groups(4n), live(0)], undefined)).toBeUndefined();
		// The oldest group names no start.
		expect(new Window([groups(4n), {}], 9n, held).start()).toBeUndefined();
	});

	test("a time start waits for a group", () => {
		let held = new Keys();
		const ranges = [range(time(2500n)), range(time(0n), time(9n))];
		let window = new Window(ranges, 9n, held);
		expect([window.pending, window.settled]).toEqual([false, false]);
		// Only the other range reads below it.
		expect(covered(window, keys(0n, 22n))).toEqual(keys(0n, 10n));
		// The groups from the time are in range whatever contains it.
		expect(covered(window, keys(2498n, 2502n))).toEqual([2500n, 2501n]);
		expect([window.floor(), window.start(), window.end()]).toEqual([undefined, undefined, undefined]);
		expect(window.resolved()).toEqual([undefined, { start: 0n, end: 10n, floored: true }]);
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

	test("a waiting time start keeps its time end", () => {
		const held = new Keys();
		const window = new Window([range(time(2500n), time(3500n))], undefined, held);
		expect(window.pending).toBe(false);
		held.held.add(2000n);
		held.held.add(3000n);
		expect(window.settle(3000n, held)).toBe(true);
		held.held.add(4000n);
		// Resolved already.
		expect(window.settle(4000n, held)).toBe(false);
		expect(window.resolved()[0]).toEqual({ start: 2000n, end: 3501n, floored: true });
	});

	test("a time start the groups can't place is looked up", () => {
		const held = new Keys([4000n, 9000n]);
		held.looks = true;
		let window = new Window([range(time(8500n))], 9000n, held);
		expect(window.lookups()).toEqual([{ time: 8500n, floor: 4001n }]);
		expect([window.settled, window.contains(9000n), window.resolved()]).toEqual([false, false, [undefined]]);
		held.held.add(8000n);
		expect(window.found(8500n, { kind: "group", group: 8000n }, held)).toBe(true);
		expect(window.resolved()).toEqual([{ start: 8000n, end: undefined, floored: true }]);
		expect(window.settled).toBe(true);

		// Something may start between 8000 and 8500.
		expect(new Window([range(time(8500n))], 9000n, held).lookups()).toHaveLength(1);
		// 8000 starts exactly there.
		expect(new Window([range(time(8000n))], 9000n, held).lookups()).toHaveLength(0);

		window = new Window([range(time(500n))], 9000n, held);
		expect(window.lookups()).toEqual([{ time: 500n, floor: 0n }]);
		// A drop from 0 says no group starts at or before it, so it reads from the time on.
		expect(window.found(500n, { kind: "dropped", floor: 0n }, held)).toBe(true);
		expect(window.resolved()).toEqual([{ start: 500n, end: undefined, floored: true }]);
		expect(window.contains(4000n)).toBe(true);
		expect(window.missable(0n).some((run) => run.start < 500n)).toBe(false);
		// A group below the time held since is none of the answer's.
		held.held.add(200n);
		expect(window.contains(200n)).toBe(false);
		held.held.delete(200n);

		// A lookup nobody asked for is ignored.
		expect(window.found(500n, { kind: "unanswered" }, held)).toBe(false);
	});

	test("a dropped lookup starts below its floor", () => {
		const history = (keys: bigint[]) => {
			const held = new Keys(keys);
			held.looks = true;
			return held;
		};
		// 4000 evicted while the lookup was out, or 8000 arrived between, as another reader's fill
		// brings it.
		for (const held of [[9000n], [4000n, 8000n, 9000n]]) {
			const window = new Window([range(time(8500n))], 9000n, history([4000n, 9000n]));
			expect(window.lookups()).toEqual([{ time: 8500n, floor: 4001n }]);
			expect(window.found(8500n, { kind: "dropped", floor: 4001n }, history(held))).toBe(true);
			// Answered, not looked up again.
			expect(window.lookups()).toHaveLength(0);
			expect(window.resolved()).toEqual([{ start: 4000n, end: undefined, floored: true }]);
		}
	});

	test("an eviction during an unanswered lookup looks it up again", () => {
		const held = new Keys([4000n, 9000n]);
		held.looks = true;
		let window = new Window([range(time(8500n))], 9000n, held);
		expect(window.lookups()).toEqual([{ time: 8500n, floor: 4001n }]);
		held.held.delete(4000n);
		expect(window.found(8500n, { kind: "unanswered" }, held)).toBe(true);
		// Looked up from below, reading nothing meanwhile.
		expect(window.lookups()).toEqual([{ time: 8500n, floor: 0n }]);
		expect(window.contains(9000n)).toBe(false);
		held.held.add(4000n);
		expect(window.found(8500n, { kind: "group", group: 4000n }, held)).toBe(true);
		expect(window.resolved()[0]?.start).toBe(4000n);

		// Even with the cache emptied.
		window = new Window([range(time(8500n))], 9000n, held);
		const all = held.held;
		held.held = new Set();
		expect(window.found(8500n, { kind: "unanswered" }, held)).toBe(true);
		expect(window.lookups()).toEqual([{ time: 8500n, floor: 0n }]);
		held.held = all;

		// With nothing evicted, it starts where the groups held say.
		window = new Window([range(time(8500n))], 9000n, held);
		expect(window.found(8500n, { kind: "unanswered" }, held)).toBe(true);
		expect(window.lookups()).toHaveLength(0);
		expect(window.resolved()[0]?.start).toBe(4000n);
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
		expect(window.lookups()).toEqual([{ time: 500n, floor: 0n }]);
		expect(window.found(500n, { kind: "unanswered" }, held)).toBe(true);
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

	test("live joins resolve where the holdings start them", () => {
		const joining: Holdings = {
			...CONTIGUOUS,
			join: (min) => {
				const back = BigInt(min.asMillis());
				return { time: back < 7n ? 7n - back : 0n };
			},
		};
		const floor = (ranges: Range[], latest: bigint | undefined) => new Window(ranges, latest, joining).floor();

		expect(floor([live(0)], 7n)).toBe(7n);
		expect(floor([live(3)], 7n)).toBe(4n);
		// A lag past the start clamps.
		expect(floor([live(30)], 7n)).toBe(0n);

		expect(new Window([{}], undefined, CONTIGUOUS).floor()).toBe(0n);

		// The lowest range wins.
		expect(floor([groups(4n), live(4)], 3n)).toBe(3n);
		expect(floor([groups(4n), live(0)], 9n)).toBe(4n);

		// Nothing asked for floors nowhere, and an empty range asks for nothing.
		expect(floor([], undefined)).toBe(0n);
		expect(floor([groups(5n, 5n), groups(10n)], undefined)).toBe(10n);
	});

	test("a hop holding no groups leaves time starts to the copies, and one at keys names them", () => {
		// Before any group, as a live join waits.
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
		expect(bounds([groups(2n, 4n), {}])).toEqual([undefined, undefined]);
		expect(bounds([range(undefined, time(6n))])).toEqual([undefined, 7n]);
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
		expect(union([groups(2n, 5n), {}, range(undefined, time(3n))])).toEqual([{}]);

		// A live join reaches back to no history, so time ranges stay beside it, joins last, each lag
		// and end a range of its own.
		expect(union([live(2), groups(4n), live(2), live(2, 3n)])).toEqual([groups(4n), live(2), live(2, 3n)]);
		// A range from the oldest group with an end is kept as it is, beside the time ranges.
		expect(union([groups(4n), range(undefined, time(3n))])).toEqual([range(undefined, time(3n)), groups(4n)]);

		// Joins of one lag land on the same group whatever their max, so they merge under the widest,
		// and no limit is the widest of all. Mirrors the Rust `combined_ranges_are_the_union`.
		const join = (max: Duration | null, end?: bigint): Range => {
			const out: Range = { start: { live: { min: Duration.millis(2), max } } };
			if (end !== undefined) out.end = end;
			return out;
		};
		expect(union([join(Duration.millis(5)), live(2)])).toEqual([join(Duration.millis(5))]);
		expect(union([join(Duration.millis(5)), join(null)])).toEqual([join(null)]);
		// A join's end keeps it apart from an open one of the same lag.
		expect(union([join(Duration.millis(5), 9n), live(2)])).toEqual([live(2), join(Duration.millis(5), 9n)]);

		// Empty ranges ask for nothing.
		expect(union([groups(5n, 5n)])).toEqual([]);
		expect(union([groups(1n, 2n)])).toEqual([groups(1n, 2n)]);
		expect(union([groups(4n, 4n), range(time(3n), time(1n))])).toEqual([]);
	});

	test("the union is order-independent and settles", () => {
		const a = [groups(2n, 5n), live(3)];
		const b = [groups(4n, 9n), live(1, 7n)];
		const combined = union([...a, ...b]);
		expect(union([...b, ...a])).toEqual(combined);
		// A subset adds nothing.
		expect(union([...combined, ...a])).toEqual(combined);
	});

	test("a wire without ranges carries one span covering them all", () => {
		expect(wireSpan([groups(4n, 6n), groups(2n, 3n)])).toEqual({ start: 2n, end: 6n });
		expect(wireSpan([{}])).toEqual({ start: undefined, end: undefined });
		// No single start covers both a time and none.
		expect(wireSpan([{}, groups(5n, 6n)])).toEqual({ start: 0n, end: undefined });
	});

	test("wire ranges drop empty ones", () => {
		expect(wireRanges([groups(5n, 6n)])).toEqual([groups(5n, 6n)]);
		expect(wireRanges([range(time(5n), time(9n))])).toEqual([range(time(5n), time(9n))]);
		// An end before its start asks for nothing.
		expect(wireRanges([range(time(5n), time(4n)), {}])).toEqual([{}]);
	});

	test("clamping raises starts to the floor and caps ends without widening", () => {
		const ranges: Range[] = [groups(1n, 9n), {}, range(undefined, time(12n))];
		expect(clamped(ranges, 4n, 6n, [undefined, undefined, { start: 8n, end: 11n, floored: true }])).toEqual([
			groups(4n, 6n),
			// The oldest start keeps none: the floor may lie before it.
			range(undefined, time(5n)),
			range(time(8n), time(5n)),
		]);
		// Resolved starts become their groups; an unresolved time end is capped apart.
		expect(clamped([range(time(1n), time(5n))], 2n, 9n, [{ start: 3n, end: 7n, floored: true }])).toEqual([
			range(time(3n), time(6n)),
		]);
		expect(clamped([range(time(1n), time(5n))], 2n, 9n, [])).toEqual([range(time(2n), time(5n))]);
		// A cap of 0, or a resolved end of 0, holds nothing.
		expect(clamped([groups(1n)], undefined, 0n, [])).toEqual([EMPTY]);
		expect(clamped([range(undefined, time(4n))], 0n, undefined, [{ start: 0n, end: 0n, floored: false }])).toEqual([
			EMPTY,
		]);
		// Nothing to clamp to returns the ranges as given.
		expect(clamped(ranges, undefined, undefined, [])).toEqual(ranges);
	});

	// A copy asked from a floor gets a resolved live join with a max as that join, placed at the
	// group it resolved to, raised to the floor, so its groups still go stale there; one with no
	// max, and history, are asked from where they resolved. Mirrors the Rust
	// `a_resolved_live_join_is_asked_placed_with_its_max`.
	test("a resolved live join is asked placed with its max", () => {
		const max = Duration.secs(2);
		const join = (max: Duration | null, end?: bigint): Range => {
			const out: Range = { start: { live: { min: Duration.millis(2), max } } };
			if (end !== undefined) out.end = end;
			return out;
		};
		const at = (start: bigint, end?: bigint) => [{ start, end, floored: true }];
		const placed = (group?: bigint) => new Map(group === undefined ? [] : [[0, group]]);
		// Placed at the floor it is raised to, still the join it was.
		expect(asked([join(max)], 10n, undefined, at(8n))).toEqual({ ranges: [join(max)], placed: placed(10n) });
		expect(asked([join(max)], 6n, undefined, at(8n))).toEqual({ ranges: [join(max)], placed: placed(8n) });
		// Its end where it resolved, as a placed one's.
		expect(asked([join(max, 500n)], 10n, undefined, at(8n, 12n))).toEqual({
			ranges: [join(max, 11n)],
			placed: placed(10n),
		});
		expect(asked([join(null)], 10n, undefined, at(8n))).toEqual({ ranges: [groups(10n)], placed: placed() });
		expect(asked([groups(8n)], 10n, undefined, at(8n))).toEqual({ ranges: [groups(10n)], placed: placed() });
		// Not yet resolved: asked as it is, for the copy to place.
		expect(asked([join(max)], 10n, undefined, [])).toEqual({ ranges: [join(max)], placed: placed() });
		// Capped at where it resolved: nothing, whatever its max.
		expect(isEmpty(asked([join(max)], undefined, 8n, at(8n)).ranges[0])).toBe(true);
		// Placed already: no lower than where it was, whatever the floor, and placed still.
		expect(asked([join(max)], 4n, undefined, [], placed(6n))).toEqual({ ranges: [join(max)], placed: placed(6n) });
		expect(asked([join(max)], undefined, undefined, [], placed(6n))).toEqual({
			ranges: [join(max)],
			placed: placed(6n),
		});
	});

	test("ranges are checked rather than served widened", () => {
		expect(rangesOf([range(time(1n))])).toEqual([range(time(1n))]);
		expect(() => rangesOf([{ start: -1n }])).toThrow(TypeError);
		// The anchors an earlier API took are refused, not served as a range with no start.
		expect(() => rangesOf([{ start: { time: 1n } as unknown as bigint }])).toThrow(TypeError);
		expect(() => rangesOf([{ start: { relative: 0n } as unknown as bigint }])).toThrow(TypeError);
		expect(() => rangesOf([{ end: 5 as unknown as bigint }])).toThrow(TypeError);
		expect(() => rangesOf({} as unknown as Range[])).toThrow(TypeError);
		expect(() => rangesOf([null as unknown as Range])).toThrow(TypeError);
	});

	test("an open range with no time start joins live", () => {
		expect(joinsLive([live(0)])).toBe(true);
		expect(joinsLive([groups(3n), live(500)])).toBe(true);
		// From the oldest group, it follows the live edge too.
		expect(joinsLive([{}])).toBe(true);
		// A time start, or any range with an end, doesn't.
		expect(joinsLive([groups(3n)])).toBe(false);
		expect(joinsLive([range(undefined, time(2n))])).toBe(false);
		expect(joinsLive([live(0, 2n)])).toBe(false);
	});

	test("a lag is checked and rounded up to whole milliseconds", () => {
		expect(rangesOf([live(5)])).toEqual([live(5)]);
		expect(() => rangesOf([{ start: { live: 5 as unknown as Live } }])).toThrow(TypeError);
		expect(() => rangesOf([{ start: { live: Duration.millis(5) as unknown as Live } }])).toThrow(TypeError);
		const sub = { start: { live: { min: Duration.micros(1500), max: Duration.ZERO } } };
		expect(rangeKey(sub)).toBe(rangeKey(live(2)));
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
		join: (min, end, budget, since) => keys.join(min, end, budget, since),
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
		// A range with no start is a live join by one lag, or the oldest group when none is drawn.
		const lag = random(3) === 0 ? undefined : random(10);
		const ranges = Array.from({ length: 1 + random(11) }, () => {
			const start = random(5) === 0 ? undefined : time(BigInt(random(40)));
			const end = random(3) === 0 ? undefined : time(BigInt(random(48)));
			return start === undefined && lag !== undefined ? live(lag, end) : range(start, end);
		});
		const keys = new Keys();
		keys.looks = random(2) === 0;
		// A live join starts by the newest group held, as a time start or at a group.
		const byTime = random(2) === 0;
		keys.joins = (min) => {
			const newest = [...keys.held].sort(ascending).at(-1);
			if (newest === undefined) return undefined;
			const at = newest > BigInt(min.asMillis()) ? newest - BigInt(min.asMillis()) : 0n;
			return byTime ? { time: at } : { group: newest };
		};
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
				const answer: Found =
					value % 3n === 0n
						? { kind: "group", group: value }
						: value % 3n === 1n
							? { kind: "dropped", floor: lookup.floor }
							: { kind: "unanswered" };
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
		const kind = random(8);
		const end = random(3) === 0 ? undefined : time(BigInt(random(48)));
		if (kind === 0) return range(undefined, end);
		if (kind === 1) return live(random(10), end);
		return range(time(BigInt(random(40))), end);
	};
	const seen = (window: Window) => ({
		ranges: window.ranges,
		runs: window.runs(),
		missable: window.missable(),
		bounds: [window.floor(), window.start(), window.end()],
		resolved: window.resolved(),
		lookups: window.lookups().map((lookup) => lookup.time),
		state: [window.settled, window.pending],
	});
	for (let round = 0; round < 2000; round++) {
		// Fixed holdings, so ranges left alone resolve as a fresh build resolves them.
		const keys = new Keys(Array.from({ length: random(8) }, () => BigInt(random(48))));
		keys.looks = random(2) === 0;
		const latest = [...keys.held].sort(ascending).at(-1);
		keys.joins = (age) => {
			const back = BigInt(age.asMillis());
			return latest === undefined ? undefined : { time: latest > back ? latest - back : 0n };
		};
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
