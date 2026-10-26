import { expect, test } from "bun:test";
import { Instant, Milli, Nano } from "@moq/clock";
import { Ledger } from "./ledger.ts";

const at = (ms: number) => Instant(Nano.fromMilli(Milli(ms)));

test("overlapping and adjacent ranges merge without changing surviving gap ages", () => {
	const ledger = new Ledger();
	ledger.account(3, 4, at(0));
	ledger.account(8, 10, at(10));
	ledger.account(15, 16, at(20));
	ledger.account(5, 6, at(30));
	ledger.account(6, 15, at(40));
	ledger.account(7, 8, at(50));
	expect(ledger.gaps(0, 18)).toEqual([
		{ start: 0, end: 3, since: at(0) },
		{ start: 4, end: 5, since: at(10) },
		{ start: 16, end: 18 },
	]);
	expect(ledger.covers(5, 16)).toBe(true);
	expect(ledger.covers(4, 16)).toBe(false);
	expect(ledger.covers(16, 17)).toBe(false);
	expect(ledger.covers(18, 18)).toBe(true);
	expect(ledger.gaps(6, 9)).toEqual([]);
	expect(ledger.gaps(4, 6)).toEqual([{ start: 4, end: 5, since: at(10) }]);
	ledger.expire(at(110), Milli(100));
	expect(ledger.covers(3, 16)).toBe(true);
	expect(ledger.first()).toBe(3);
	ledger.account(16, 17, at(110));
	expect(ledger.covers(3, 17)).toBe(true);
});

test("demand changes age order and restarts the entire intersecting gap", () => {
	const ledger = new Ledger();
	ledger.account(0, 1, at(0));
	ledger.account(4, 5, at(0));
	ledger.account(8, 9, at(25));
	ledger.demand(2, 3, at(50));
	ledger.account(2, 3, at(75));
	ledger.expire(at(125), Milli(100));
	expect(ledger.gaps(0, 9)).toEqual([
		{ start: 1, end: 2, since: at(50) },
		{ start: 3, end: 4, since: at(50) },
	]);
	ledger.expire(at(150), Milli(100));
	expect(ledger.covers(0, 9)).toBe(true);
	expect(ledger.hasGaps()).toBe(false);
});

test("a leading gap keeps its age until a lower arrival makes it foldable", () => {
	const ledger = new Ledger();
	ledger.account(10, 11, at(0));
	ledger.account(15, 16, at(10));
	ledger.demand(0, 5, at(100));
	ledger.expire(at(110), Milli(100));
	expect(ledger.gaps(0, 16)).toEqual([{ start: 0, end: 10, since: at(100) }]);
	ledger.account(0, 1, at(150));
	ledger.expire(at(199), Milli(100));
	expect(ledger.covers(0, 16)).toBe(false);
	ledger.expire(at(200), Milli(100));
	expect(ledger.covers(0, 16)).toBe(true);
});

test("changing grace reuses ages without reopening folded gaps", () => {
	const ledger = new Ledger();
	ledger.account(0, 1, at(0));
	ledger.account(2, 3, at(0));
	ledger.expire(at(100), Milli(200));
	expect(ledger.hasGaps()).toBe(true);
	ledger.expire(at(100), Milli(50));
	expect(ledger.hasGaps()).toBe(false);
	ledger.account(4, 5, at(100));
	ledger.expire(at(200), Milli(200));
	expect(ledger.gaps(0, 5)).toEqual([{ start: 3, end: 4, since: at(100) }]);
	ledger.account(2, 4, at(200));
	ledger.expire(at(300), Milli(0));
	expect(ledger.covers(0, 5)).toBe(true);
});

test.each([0, 1, 4, 32, 128])("rolling expiration keeps only %i pending gaps", (retained) => {
	const ledger = new Ledger();
	for (let arrival = 0; arrival < retained * 4 + 8; arrival++) {
		ledger.account(arrival * 2, arrival * 2 + 1, at(arrival));
		ledger.expire(at(arrival), Milli(retained));
		const folded = Math.max(0, arrival - retained) * 2 + 1;
		expect(ledger.covers(0, folded)).toBe(true);
		expect(ledger.gaps(0, arrival * 2 + 1).length).toBe(Math.min(arrival, retained));
	}
});

test("shuffled arrivals and bulk merges agree with the accounted sequence set", () => {
	const ledger = new Ledger();
	const sequences = new Set<number>();
	let seed = 42;
	for (let i = 0; i < 1024; i++) {
		seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0;
		const start = seed % 4096;
		const end = start + (i < 512 ? 1 : 32);
		ledger.account(start, end, at(i));
		for (let n = start; n < end; n++) sequences.add(n);
		ledger.expire(at(i), Milli(2000));
	}
	const gaps = ledger.gaps(0, 4128);
	for (let i = 0; i < 4128; i++) {
		expect(ledger.covers(i, i + 1)).toBe(sequences.has(i));
		expect(gaps.some((gap) => gap.start <= i && i < gap.end)).toBe(!sequences.has(i));
	}
	ledger.account(0, 4128, at(1024));
	ledger.expire(at(4000), Milli(2000));
	expect(ledger.hasGaps()).toBe(false);
	expect(ledger.covers(0, 4128)).toBe(true);
});
