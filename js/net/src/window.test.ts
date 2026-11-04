import { expect, test } from "bun:test";
import { Timestamp } from "./time.ts";
import type { Anchor, Range } from "./track.ts";
import {
	clamped,
	type Held,
	type Holdings,
	isWire,
	joinsLive,
	rangesOf,
	union,
	Window,
	wireRanges,
	wireSpan,
} from "./window.ts";

const abs = (start: bigint, end?: bigint): Range =>
	end === undefined ? { start: { absolute: start } } : { start: { absolute: start }, end: { absolute: end } };

test("union merges absolute spans and drops what an open range covers", () => {
	expect(union([abs(4n, 6n), abs(0n, 2n), abs(2n, 3n), abs(5n, 9n)])).toEqual([abs(0n, 3n), abs(4n, 9n)]);
	// The open absolute span drops a relative-ended range starting inside it.
	expect(union([abs(3n), { start: { absolute: 5n }, end: { relative: 0n } }])).toEqual([abs(3n)]);
	// The farthest open relative range drops relative ranges starting no farther back.
	expect(
		union([
			{ start: { relative: 2n } },
			{ start: { relative: 5n } },
			{ start: { relative: 1n }, end: { relative: 0n } },
		]),
	).toEqual([{ start: { relative: 5n } }]);
	// A relative range above the live edge never hides an absolute one, nor the reverse.
	expect(union([abs(10n), { start: { relative: 0n } }])).toEqual([abs(10n), { start: { relative: 0n } }]);
	// The oldest start covers everything, and empty ranges vanish.
	expect(union([abs(2n, 4n), {}])).toEqual([{}]);
	expect(union([abs(4n, 4n), { end: { absolute: 0n } }])).toEqual([]);
});

test("a window reads only its ranges, counting relative anchors back from the latest group", () => {
	const window = new Window(
		[abs(2n, 4n), { start: { relative: 1n } }, { start: { absolute: 0n }, end: { relative: 8n } }],
		10n,
	);
	expect([0n, 1n, 2n, 3n, 4n, 8n, 9n, 10n, 11n].map((sequence) => window.contains(sequence))).toEqual([
		true,
		true,
		true,
		true,
		false,
		false,
		true,
		true,
		true,
	]);
	expect(window.floor()).toBe(0n);
	expect(window.start()).toBe(0n);
	expect(window.end()).toBeUndefined();
	expect(window.next(4n)).toBe(9n);
	expect(window.runs(1n, 12n)).toEqual([
		{ start: 1n, end: 4n },
		{ start: 9n, end: 12n },
	]);
	expect(window.resolved()).toEqual([{ start: 2n, end: 4n }, { start: 9n }, { start: 0n, end: 3n }]);
});

test("a relative anchor clamps to group 0 and waits for a first group", () => {
	expect(new Window([{ start: { relative: 50n }, end: { relative: 40n } }], 10n).runs()).toEqual([
		{ start: 0n, end: 1n },
	]);

	const window = new Window([{ start: { relative: 0n } }], undefined);
	expect(window.pending).toBe(true);
	expect(window.contains(3n)).toBe(true);
	expect(window.floor()).toBeUndefined();
	expect(window.settle(7n)).toBe(true);
	expect(window.pending).toBe(false);
	expect(window.contains(6n)).toBe(false);
	expect(window.floor()).toBe(7n);
	// Resolved once: a later group never moves it.
	expect(window.settle(9n)).toBe(false);
});

test("a bounded window ends past its last group, and nothing in range ends at 0", () => {
	expect(new Window([abs(2n, 4n), abs(6n, 9n)], undefined).end()).toBe(9n);
	expect(new Window([], undefined).end()).toBe(0n);
	// The oldest start names no floor, so a wire without ranges asks with none.
	expect(new Window([{ end: { absolute: 5n } }], undefined).start()).toBeUndefined();
});

test("a wire without ranges carries one span covering them all", () => {
	expect(wireSpan([abs(4n, 6n), abs(2n, 3n)])).toEqual({ start: 2n, end: 6n });
	expect(wireSpan([{ start: { relative: 0n } }])).toEqual({ start: undefined, end: undefined });
	// No single start covers both an absolute and a relative one.
	expect(wireSpan([abs(9n), { start: { relative: 0n } }])).toEqual({ start: 0n, end: undefined });
	// A relative end goes out open, since the copy resolves it against what it receives.
	expect(wireRanges([{ start: { relative: 3n }, end: { relative: 1n } }, abs(5n, 5n)])).toEqual([
		{ start: { relative: 3n } },
	]);
});

test("clamping raises starts to the floor and caps ends without widening", () => {
	const ranges: Range[] = [abs(1n, 9n), {}, { start: { relative: 2n }, end: { relative: 0n } }];
	expect(clamped(ranges, 4n, 6n, [undefined, undefined, { start: 8n, end: 11n }])).toEqual([
		abs(4n, 6n),
		abs(4n, 6n),
		abs(8n, 6n),
	]);
	// A relative start not resolved keeps its anchor for the caller to floor.
	expect(clamped([{ start: { relative: 2n } }], 4n, undefined, [])).toEqual([{ start: { relative: 2n } }]);
});

test("ranges are checked rather than served widened", () => {
	expect(() => rangesOf([{ start: { absolute: -1n } }])).toThrow(TypeError);
	expect(() => rangesOf([{ start: { absolute: 1n, relative: 2n } as never }])).toThrow(TypeError);
	expect(() => rangesOf([{ end: { time: 5 } as never }])).toThrow(TypeError);
	expect(joinsLive([{}])).toBe(true);
	expect(joinsLive([abs(3n)])).toBe(false);
});

const time = (millis: number): Anchor => ({ time: Timestamp.fromMillis(millis) });
const started = (millis: number): Held => ({ started: Timestamp.fromMillis(millis) });

// A hop's groups by sequence, the runs that can no longer arrive, and how many queries a walk made.
class Groups implements Holdings {
	held = new Map<bigint, Held>();
	gones: { start: bigint; end: bigint }[] = [];
	done = false;
	asked = 0;

	constructor(held: [bigint, Held][] = []) {
		for (const [sequence, state] of held) this.held.set(sequence, state);
	}

	#sorted(from: bigint): [bigint, Held][] {
		return [...this.held]
			.filter(([sequence]) => sequence >= from)
			.sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0));
	}

	containing(pts: Timestamp) {
		const starts = this.#sorted(0n).filter(([, held]) => typeof held === "object");
		if (starts.length === 0) return "unknown" as const;
		const at = starts.filter(([, held]) => !pts.isBefore((held as { started: Timestamp }).started)).at(-1);
		return at ? { group: at[0] } : ("before" as const);
	}

	nextStart(from: bigint) {
		this.asked++;
		const next = this.#sorted(from).find(([, held]) => typeof held === "object");
		return next && { sequence: next[0], start: (next[1] as { started: Timestamp }).started };
	}

	nextHeld(from: bigint) {
		this.asked++;
		const next = this.#sorted(from)[0];
		return next && { sequence: next[0], held: next[1] };
	}

	gone(sequence: bigint) {
		return this.gones.find((run) => run.start <= sequence && sequence < run.end)?.end;
	}

	complete() {
		return this.done;
	}
}

const covered = (window: Window, upto: number) =>
	Array.from({ length: upto }, (_, sequence) => BigInt(sequence)).filter((sequence) => window.contains(sequence));

test("a time start is the group containing it, and an end stops past it once a later group starts after it", () => {
	const groups = new Groups([
		[2n, started(2000)],
		[3n, started(3000)],
		[4n, started(4000)],
		[5n, started(5000)],
		[6n, "pending"],
	]);
	let window = new Window([{ start: time(3500), end: time(4500) }], 6n, groups);
	expect(covered(window, 8)).toEqual([3n, 4n]);
	expect(window.settled).toBe(true);
	expect(window.resolved()).toEqual([{ start: 3n, end: 5n }]);

	// Group 6 may yet start at 5000 too: read on speculation, the end left open.
	window = new Window([{ start: time(3500), end: time(5000) }], 6n, groups);
	expect(window.settled).toBe(false);
	expect(covered(window, 9)).toEqual([3n, 4n, 5n, 6n]);
	expect(window.resolved()).toEqual([{ start: 3n, end: undefined }]);
	expect(window.end()).toBeUndefined();

	// Before every start held: reach back to the oldest.
	expect(covered(new Window([{ start: time(1000), end: time(2500) }], 6n, groups), 8)).toEqual([0n, 1n, 2n]);

	// Nothing started yet: a time start waits on a first group, as a relative one does, then on
	// one starting, reading none meanwhile, since a group arrives before the frame that shows
	// where it starts.
	window = new Window([{ start: time(1000) }], undefined, new Groups());
	expect([window.pending, window.floor(), window.settled]).toEqual([true, undefined, false]);
	expect(window.settle(4n, new Groups([[4n, "pending"]]))).toBe(true);
	expect([window.pending, window.contains(4n), window.settled]).toEqual([false, false, false]);
	expect(window.settle(4n, new Groups([[4n, started(500)]]))).toBe(true);
	expect([window.floor(), window.settled]).toEqual([4n, true]);
	// Nothing can start any more: it reaches back to the oldest.
	const complete = new Groups([[4n, "never"]]);
	complete.done = true;
	expect(new Window([{ start: time(1000) }], 4n, complete).floor()).toBe(0n);
});

test("a waiting time start keeps the relative end it was asked with", () => {
	for (const latest of [undefined, 9n]) {
		const groups = new Groups();
		const window = new Window([{ start: time(2500), end: { relative: 0n } }], latest, groups);
		expect(window.pending).toBe(latest === undefined);
		for (let sequence = 2n; sequence <= 9n; sequence++) groups.held.set(sequence, "pending");
		window.settle(9n, groups);
		expect(window.pending).toBe(false);
		groups.held.set(10n, "pending");
		expect(window.settle(10n, groups)).toBe(false);

		groups.held.set(2n, started(2000));
		expect(window.settle(10n, groups)).toBe(true);
		expect(window.resolved()).toEqual([{ start: 2n, end: 10n }]);
	}
});

test("a time end settles once nothing below its end can arrive", () => {
	const groups = new Groups([
		[3n, started(3000)],
		[6n, started(6000)],
	]);
	const window = new Window([{ start: time(3000), end: time(4000) }], 6n, groups);
	// 4 and 5 may still arrive inside the end: only 4 is read, on speculation, and never 6.
	expect(window.settled).toBe(false);
	expect(covered(window, 8)).toEqual([3n, 4n]);

	// 4 arrives yet to start: still the one read on speculation, so 5 waits.
	groups.held.set(4n, "pending");
	expect(window.settle(6n, groups)).toBe(false);
	groups.held.set(4n, started(3500));
	expect(window.settle(6n, groups)).toBe(true);
	expect(window.settled).toBe(false);
	groups.gones.push({ start: 5n, end: 6n });
	expect(window.settle(6n, groups)).toBe(true);
	expect(window.settled).toBe(true);
	expect(covered(window, 8)).toEqual([3n, 4n, 5n]);

	// A group that never starts proves nothing, so it never holds the end open.
	const never = new Groups([
		[3n, started(3000)],
		[4n, "never"],
		[5n, started(5000)],
	]);
	const settled = new Window([{ start: time(3000), end: time(4000) }], 5n, never);
	expect(settled.settled).toBe(true);
	expect(covered(settled, 8)).toEqual([3n, 4n]);
});

test("an unsettled time end reads one group on speculation", () => {
	const groups = new Groups([
		[3n, "pending"],
		[4n, "pending"],
		[5n, "pending"],
	]);
	const window = new Window([{ start: { absolute: 3n }, end: time(3500) }], 5n, groups);
	expect(covered(window, 8)).toEqual([3n]);

	groups.held.set(3n, started(3000));
	expect(window.settle(5n, groups)).toBe(true);
	expect(covered(window, 8)).toEqual([3n, 4n]);

	// 4 never starts, so 5 is the one read on speculation.
	groups.held.set(4n, "never");
	expect(window.settle(5n, groups)).toBe(true);
	expect(covered(window, 8)).toEqual([3n, 4n, 5n]);

	// 5 stalls and 6 starts inside the end: 5 is in range then, and 7 is speculative.
	groups.held.set(6n, started(3400));
	groups.held.set(7n, "pending");
	expect(window.settle(7n, groups)).toBe(true);
	expect(covered(window, 9)).toEqual([3n, 4n, 5n, 6n, 7n]);

	// 7 starts past the end, which settles with 5 still stalled below it.
	groups.held.set(7n, started(3600));
	expect(window.settle(7n, groups)).toBe(true);
	expect(window.settled).toBe(true);
	expect(covered(window, 9)).toEqual([3n, 4n, 5n, 6n]);

	// Nothing more can arrive or start: uncapped.
	const complete = new Groups([
		[3n, started(3000)],
		[4n, "pending"],
		[5n, "pending"],
	]);
	complete.done = true;
	expect(covered(new Window([{ start: time(3000), end: time(3500) }], 5n, complete), 7)).toEqual([3n, 4n, 5n, 6n]);
});

test("a time end walks each group once behind a missing sequence", () => {
	const groups = new Groups([[0n, started(0)]]);
	const window = new Window([{ start: time(0), end: time(1_000_000) }], 0n, groups);
	const count = 2_000;
	for (let sequence = 2n; sequence < BigInt(count); sequence++) {
		groups.held.set(sequence, started(Number(sequence) * 10));
		window.settle(sequence, groups);
	}
	// Group 1 never arrived.
	expect(window.settled).toBe(false);
	expect(window.contains(BigInt(count - 1))).toBe(true);
	expect(groups.asked).toBeLessThan(8 * count);
});

test("a time anchor needs moq-lite-07, is resolved by the copy, and clamps to its group", () => {
	expect(isWire({ start: time(1) })).toBe(false);
	expect(isWire({ start: { absolute: 1n }, end: time(1) })).toBe(false);
	// The copy resolves a time end itself, so it asks through the live edge.
	expect(wireRanges([{ start: time(1), end: time(5) }])).toEqual([{ start: time(1) }]);
	expect(joinsLive([{ start: time(1) }])).toBe(false);
	// A time can't be placed against the others without the hop's groups.
	expect(union([{ start: time(5) }, {}])).toEqual([{}]);
	expect(union([{ start: time(5) }, { start: { relative: 2n } }])).toEqual([
		{ start: { relative: 2n } },
		{ start: time(5) },
	]);
	// Resolved anchors become their groups; an unresolved time end stays so the cap applies apart.
	expect(clamped([{ start: time(1), end: time(5) }], 2n, 9n, [{ start: 3n, end: 7n }])).toEqual([
		{ start: { absolute: 3n }, end: { absolute: 7n } },
	]);
	expect(clamped([{ start: time(1), end: time(5) }], 2n, 9n, [])).toEqual([{ start: time(1), end: time(5) }]);
	expect(rangesOf([{ start: time(1) }])).toEqual([{ start: time(1) }]);
	expect(() => rangesOf([{ start: { time: 1 } as unknown as Anchor }])).toThrow(TypeError);
});
