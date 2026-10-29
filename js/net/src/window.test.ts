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

const abs = (start: number, end?: number): Range =>
	end === undefined ? { start: { absolute: start } } : { start: { absolute: start }, end: { absolute: end } };

test("union merges absolute spans and drops what an open range covers", () => {
	expect(union([abs(4, 6), abs(0, 2), abs(2, 3), abs(5, 9)])).toEqual([abs(0, 3), abs(4, 9)]);
	// The open absolute span drops a relative-ended range starting inside it.
	expect(union([abs(3), { start: { absolute: 5 }, end: { relative: 0 } }])).toEqual([abs(3)]);
	// The farthest open relative range drops relative ranges starting no farther back.
	expect(
		union([
			{ start: { relative: 2 } },
			{ start: { relative: 5 } },
			{ start: { relative: 1 }, end: { relative: 0 } },
		]),
	).toEqual([{ start: { relative: 5 } }]);
	// A relative range above the live edge never hides an absolute one, nor the reverse.
	expect(union([abs(10), { start: { relative: 0 } }])).toEqual([abs(10), { start: { relative: 0 } }]);
	// The oldest start covers everything, and empty ranges vanish.
	expect(union([abs(2, 4), {}])).toEqual([{}]);
	expect(union([abs(4, 4), { end: { absolute: 0 } }])).toEqual([]);
});

test("a window reads only its ranges, counting relative anchors back from the latest group", () => {
	const window = new Window(
		[abs(2, 4), { start: { relative: 1 } }, { start: { absolute: 0 }, end: { relative: 8 } }],
		10,
	);
	expect([0, 1, 2, 3, 4, 8, 9, 10, 11].map((sequence) => window.contains(sequence))).toEqual([
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
	expect(window.floor()).toBe(0);
	expect(window.start()).toBe(0);
	expect(window.end()).toBeUndefined();
	expect(window.next(4)).toBe(9);
	expect(window.runs(1, 12)).toEqual([
		{ start: 1, end: 4 },
		{ start: 9, end: 12 },
	]);
	expect(window.resolved()).toEqual([{ start: 2, end: 4 }, { start: 9 }, { start: 0, end: 3 }]);
});

test("a relative anchor clamps to group 0 and waits for a first group", () => {
	expect(new Window([{ start: { relative: 50 }, end: { relative: 40 } }], 10).runs()).toEqual([{ start: 0, end: 1 }]);

	const window = new Window([{ start: { relative: 0 } }], undefined);
	expect(window.pending).toBe(true);
	expect(window.contains(3)).toBe(true);
	expect(window.floor()).toBeUndefined();
	expect(window.settle(7)).toBe(true);
	expect(window.pending).toBe(false);
	expect(window.contains(6)).toBe(false);
	expect(window.floor()).toBe(7);
	// Resolved once: a later group never moves it.
	expect(window.settle(9)).toBe(false);
});

test("a bounded window ends past its last group, and nothing in range ends at 0", () => {
	expect(new Window([abs(2, 4), abs(6, 9)], undefined).end()).toBe(9);
	expect(new Window([], undefined).end()).toBe(0);
	// The oldest start names no floor, so a wire without ranges asks with none.
	expect(new Window([{ end: { absolute: 5 } }], undefined).start()).toBeUndefined();
});

test("a wire without ranges carries one span covering them all", () => {
	expect(wireSpan([abs(4, 6), abs(2, 3)])).toEqual({ start: 2, end: 6 });
	expect(wireSpan([{ start: { relative: 0 } }])).toEqual({ start: undefined, end: undefined });
	// No single start covers both an absolute and a relative one.
	expect(wireSpan([abs(9), { start: { relative: 0 } }])).toEqual({ start: 0, end: undefined });
	// A relative end goes out open, since the copy resolves it against what it receives.
	expect(wireRanges([{ start: { relative: 3 }, end: { relative: 1 } }, abs(5, 5)])).toEqual([
		{ start: { relative: 3 } },
	]);
});

test("clamping raises starts to the floor and caps ends without widening", () => {
	const ranges: Range[] = [abs(1, 9), {}, { start: { relative: 2 }, end: { relative: 0 } }];
	expect(clamped(ranges, 4, 6, [undefined, undefined, { start: 8, end: 11 }])).toEqual([
		abs(4, 6),
		abs(4, 6),
		abs(8, 6),
	]);
	// A relative start not resolved keeps its anchor for the caller to floor.
	expect(clamped([{ start: { relative: 2 } }], 4, undefined, [])).toEqual([{ start: { relative: 2 } }]);
});

test("ranges are checked rather than served widened", () => {
	expect(() => rangesOf([{ start: { absolute: -1 } }])).toThrow(TypeError);
	expect(() => rangesOf([{ start: { absolute: 1, relative: 2 } as never }])).toThrow(TypeError);
	expect(() => rangesOf([{ end: { time: 5 } as never }])).toThrow(TypeError);
	expect(joinsLive([{}])).toBe(true);
	expect(joinsLive([abs(3)])).toBe(false);
});

const time = (millis: number): Anchor => ({ time: Timestamp.fromMillis(millis) });
const started = (millis: number): Held => ({ started: Timestamp.fromMillis(millis) });

// A hop's groups by sequence, the runs that can no longer arrive, and how many queries a walk made.
class Groups implements Holdings {
	held = new Map<number, Held>();
	gones: { start: number; end: number }[] = [];
	done = false;
	asked = 0;

	constructor(held: [number, Held][] = []) {
		for (const [sequence, state] of held) this.held.set(sequence, state);
	}

	#sorted(from: number): [number, Held][] {
		return [...this.held].filter(([sequence]) => sequence >= from).sort((a, b) => a[0] - b[0]);
	}

	containing(pts: Timestamp) {
		const starts = this.#sorted(0).filter(([, held]) => typeof held === "object");
		if (starts.length === 0) return "unknown" as const;
		const at = starts.filter(([, held]) => !pts.isBefore((held as { started: Timestamp }).started)).at(-1);
		return at ? { group: at[0] } : ("before" as const);
	}

	nextStart(from: number) {
		this.asked++;
		const next = this.#sorted(from).find(([, held]) => typeof held === "object");
		return next && { sequence: next[0], start: (next[1] as { started: Timestamp }).started };
	}

	nextHeld(from: number) {
		this.asked++;
		const next = this.#sorted(from)[0];
		return next && { sequence: next[0], held: next[1] };
	}

	gone(sequence: number) {
		return this.gones.find((run) => run.start <= sequence && sequence < run.end)?.end;
	}

	complete() {
		return this.done;
	}
}

const covered = (window: Window, upto: number) =>
	Array.from({ length: upto }, (_, sequence) => sequence).filter((sequence) => window.contains(sequence));

test("a time start is the group containing it, and an end stops past it once a later group starts after it", () => {
	const groups = new Groups([
		[2, started(2000)],
		[3, started(3000)],
		[4, started(4000)],
		[5, started(5000)],
		[6, "pending"],
	]);
	let window = new Window([{ start: time(3500), end: time(4500) }], 6, groups);
	expect(covered(window, 8)).toEqual([3, 4]);
	expect(window.settled).toBe(true);
	expect(window.resolved()).toEqual([{ start: 3, end: 5 }]);

	// Group 6 may yet start at 5000 too: read on speculation, the end left open.
	window = new Window([{ start: time(3500), end: time(5000) }], 6, groups);
	expect(window.settled).toBe(false);
	expect(covered(window, 9)).toEqual([3, 4, 5, 6]);
	expect(window.resolved()).toEqual([{ start: 3, end: undefined }]);
	expect(window.end()).toBeUndefined();

	// Before every start held: reach back to the oldest.
	expect(covered(new Window([{ start: time(1000), end: time(2500) }], 6, groups), 8)).toEqual([0, 1, 2]);

	// Nothing started yet: a time start waits for a group that has, reading none meanwhile, since a
	// group arrives before the frame that shows where it starts.
	window = new Window([{ start: time(1000) }], undefined, new Groups());
	expect([window.contains(3), window.floor(), window.settled]).toEqual([false, undefined, false]);
	expect(window.settle(4, new Groups([[4, "pending"]]))).toBe(false);
	expect(window.settle(4, new Groups([[4, started(500)]]))).toBe(true);
	expect([window.floor(), window.settled]).toEqual([4, true]);
	// Nothing can start any more: it reaches back to the oldest.
	const complete = new Groups([[4, "never"]]);
	complete.done = true;
	expect(new Window([{ start: time(1000) }], 4, complete).floor()).toBe(0);
});

test("a waiting time start keeps the relative end it was asked with", () => {
	const groups = new Groups(Array.from({ length: 9 }, (_, index): [number, Held] => [index + 2, "pending"]));
	const window = new Window([{ start: time(2500), end: { relative: 0 } }], 9, groups);
	expect(window.settle(10, groups)).toBe(false);

	groups.held.set(2, started(2000));
	expect(window.settle(10, groups)).toBe(true);
	expect(window.resolved()).toEqual([{ start: 2, end: 10 }]);
});

test("a time end settles once nothing below its end can arrive", () => {
	const groups = new Groups([
		[3, started(3000)],
		[6, started(6000)],
	]);
	const window = new Window([{ start: time(3000), end: time(4000) }], 6, groups);
	// 4 and 5 may still arrive inside the end: only 4 is read, on speculation, and never 6.
	expect(window.settled).toBe(false);
	expect(covered(window, 8)).toEqual([3, 4]);

	// 4 arrives yet to start: still the one read on speculation, so 5 waits.
	groups.held.set(4, "pending");
	expect(window.settle(6, groups)).toBe(false);
	groups.held.set(4, started(3500));
	expect(window.settle(6, groups)).toBe(true);
	expect(window.settled).toBe(false);
	groups.gones.push({ start: 5, end: 6 });
	expect(window.settle(6, groups)).toBe(true);
	expect(window.settled).toBe(true);
	expect(covered(window, 8)).toEqual([3, 4, 5]);

	// A group that never starts proves nothing, so it never holds the end open.
	const never = new Groups([
		[3, started(3000)],
		[4, "never"],
		[5, started(5000)],
	]);
	const settled = new Window([{ start: time(3000), end: time(4000) }], 5, never);
	expect(settled.settled).toBe(true);
	expect(covered(settled, 8)).toEqual([3, 4]);
});

test("an unsettled time end reads one group on speculation", () => {
	const groups = new Groups([
		[3, "pending"],
		[4, "pending"],
		[5, "pending"],
	]);
	const window = new Window([{ start: { absolute: 3 }, end: time(3500) }], 5, groups);
	expect(covered(window, 8)).toEqual([3]);

	groups.held.set(3, started(3000));
	expect(window.settle(5, groups)).toBe(true);
	expect(covered(window, 8)).toEqual([3, 4]);

	// 4 never starts, so 5 is the one read on speculation.
	groups.held.set(4, "never");
	expect(window.settle(5, groups)).toBe(true);
	expect(covered(window, 8)).toEqual([3, 4, 5]);

	// 5 stalls and 6 starts inside the end: 5 is in range then, and 7 is speculative.
	groups.held.set(6, started(3400));
	groups.held.set(7, "pending");
	expect(window.settle(7, groups)).toBe(true);
	expect(covered(window, 9)).toEqual([3, 4, 5, 6, 7]);

	// 7 starts past the end, which settles with 5 still stalled below it.
	groups.held.set(7, started(3600));
	expect(window.settle(7, groups)).toBe(true);
	expect(window.settled).toBe(true);
	expect(covered(window, 9)).toEqual([3, 4, 5, 6]);

	// Nothing more can arrive or start: uncapped.
	const complete = new Groups([
		[3, started(3000)],
		[4, "pending"],
		[5, "pending"],
	]);
	complete.done = true;
	expect(covered(new Window([{ start: time(3000), end: time(3500) }], 5, complete), 7)).toEqual([3, 4, 5, 6]);
});

test("a time end walks each group once behind a missing sequence", () => {
	const groups = new Groups([[0, started(0)]]);
	const window = new Window([{ start: time(0), end: time(1_000_000) }], 0, groups);
	const count = 2_000;
	for (let sequence = 2; sequence < count; sequence++) {
		groups.held.set(sequence, started(sequence * 10));
		window.settle(sequence, groups);
	}
	// Group 1 never arrived.
	expect(window.settled).toBe(false);
	expect(window.contains(count - 1)).toBe(true);
	expect(groups.asked).toBeLessThan(8 * count);
});

test("a datagram is judged by its own timestamp, one range at a time", () => {
	const window = new Window([{ start: { absolute: 0 }, end: time(3500) }], undefined, new Groups());
	expect(window.admitsDatagram(9, Timestamp.fromMillis(3500))).toBe(true);
	expect(window.admitsDatagram(9, Timestamp.fromMillis(3600))).toBe(false);

	// A start still waiting judges a datagram by its time alone.
	expect(window.admitsDatagram(0, Timestamp.fromMillis(0))).toBe(true);
	const waiting = new Window([{ start: time(3000), end: time(3500) }], undefined, new Groups());
	expect(waiting.admitsDatagram(0, Timestamp.fromMillis(2900))).toBe(false);
	expect(waiting.admitsDatagram(1, Timestamp.fromMillis(3000))).toBe(true);
	expect(waiting.admitsDatagram(3, Timestamp.fromMillis(3600))).toBe(false);

	// Group 1 arrived only as a datagram: the end settles before 2 by sequence, but 1 presents past it.
	const groups = new Groups([
		[0, started(0)],
		[2, started(10_000)],
	]);
	groups.gones.push({ start: 1, end: 2 });
	const settled = new Window([{ start: time(0), end: time(5000) }], undefined, groups);
	expect(settled.resolved()).toEqual([{ start: 0, end: 2 }]);
	expect(settled.admitsDatagram(1, Timestamp.fromMillis(4000))).toBe(true);
	expect(settled.admitsDatagram(1, Timestamp.fromMillis(7000))).toBe(false);

	// The sequence and the time must pass the same range.
	const both = new Window([{ start: { absolute: 6 } }, { start: time(5500) }], 5, new Groups([[5, started(5000)]]));
	expect(both.admitsDatagram(5, Timestamp.fromMillis(5000))).toBe(false);
	expect(both.admitsDatagram(5, Timestamp.fromMillis(5500))).toBe(true);
	expect(both.admitsDatagram(6, Timestamp.fromMillis(5000))).toBe(true);
});

test("a time anchor needs moq-lite-07, is resolved by the copy, and clamps to its group", () => {
	expect(isWire({ start: time(1) })).toBe(false);
	expect(isWire({ start: { absolute: 1 }, end: time(1) })).toBe(false);
	// The copy resolves a time end itself, so it asks through the live edge.
	expect(wireRanges([{ start: time(1), end: time(5) }])).toEqual([{ start: time(1) }]);
	expect(joinsLive([{ start: time(1) }])).toBe(false);
	// A time can't be placed against the others without the hop's groups.
	expect(union([{ start: time(5) }, {}])).toEqual([{}]);
	expect(union([{ start: time(5) }, { start: { relative: 2 } }])).toEqual([
		{ start: { relative: 2 } },
		{ start: time(5) },
	]);
	// Resolved anchors become their groups; an unresolved time end stays so the cap applies apart.
	expect(clamped([{ start: time(1), end: time(5) }], 2, 9, [{ start: 3, end: 7 }])).toEqual([
		{ start: { absolute: 3 }, end: { absolute: 7 } },
	]);
	expect(clamped([{ start: time(1), end: time(5) }], 2, 9, [])).toEqual([{ start: time(1), end: time(5) }]);
	expect(rangesOf([{ start: time(1) }])).toEqual([{ start: time(1) }]);
	expect(() => rangesOf([{ start: { time: 1 } as unknown as Anchor }])).toThrow(TypeError);
});
