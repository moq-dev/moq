import { expect, test } from "bun:test";
import { Instant, Milli, Nano } from "@moq/clock";
import { Ledger } from "./ledger.ts";

const at = (ms: number) => Instant(Nano.fromMilli(Milli(ms)));

test("overlapping and adjacent ranges merge without changing surviving gap ages", () => {
	const ledger = new Ledger();
	ledger.account(3n, 4n, at(0));
	ledger.account(8n, 10n, at(10));
	ledger.account(15n, 16n, at(20));
	ledger.account(5n, 6n, at(30));
	ledger.account(6n, 15n, at(40));
	ledger.account(7n, 8n, at(50));
	expect(ledger.gaps(0n, 18n)).toEqual([
		{ start: 0n, end: 3n, since: at(0) },
		{ start: 4n, end: 5n, since: at(10) },
		{ start: 16n, end: 18n },
	]);
	expect(ledger.covers(5n, 16n)).toBe(true);
	expect(ledger.covers(4n, 16n)).toBe(false);
	expect(ledger.covers(16n, 17n)).toBe(false);
	expect(ledger.covers(18n, 18n)).toBe(true);
	expect(ledger.gaps(6n, 9n)).toEqual([]);
	expect(ledger.gaps(4n, 6n)).toEqual([{ start: 4n, end: 5n, since: at(10) }]);
	ledger.expire(at(110), Milli(100));
	expect(ledger.covers(3n, 16n)).toBe(true);
	expect(ledger.first()).toBe(3n);
	ledger.account(16n, 17n, at(110));
	expect(ledger.covers(3n, 17n)).toBe(true);
});

test("demand changes age order and restarts the entire intersecting gap", () => {
	const ledger = new Ledger();
	ledger.account(0n, 1n, at(0));
	ledger.account(4n, 5n, at(0));
	ledger.account(8n, 9n, at(25));
	ledger.demand(2n, 3n, at(50));
	ledger.account(2n, 3n, at(75));
	ledger.expire(at(125), Milli(100));
	expect(ledger.gaps(0n, 9n)).toEqual([
		{ start: 1n, end: 2n, since: at(50) },
		{ start: 3n, end: 4n, since: at(50) },
	]);
	ledger.expire(at(150), Milli(100));
	expect(ledger.covers(0n, 9n)).toBe(true);
	expect(ledger.hasGaps()).toBe(false);
});

test("a leading gap keeps its age until a lower arrival makes it foldable", () => {
	const ledger = new Ledger();
	ledger.account(10n, 11n, at(0));
	ledger.account(15n, 16n, at(10));
	ledger.demand(0n, 5n, at(100));
	ledger.expire(at(110), Milli(100));
	expect(ledger.gaps(0n, 16n)).toEqual([{ start: 0n, end: 10n, since: at(100) }]);
	ledger.account(0n, 1n, at(150));
	ledger.expire(at(199), Milli(100));
	expect(ledger.covers(0n, 16n)).toBe(false);
	ledger.expire(at(200), Milli(100));
	expect(ledger.covers(0n, 16n)).toBe(true);
});

test("changing grace reuses ages without reopening folded gaps", () => {
	const ledger = new Ledger();
	ledger.account(0n, 1n, at(0));
	ledger.account(2n, 3n, at(0));
	ledger.expire(at(100), Milli(200));
	expect(ledger.hasGaps()).toBe(true);
	ledger.expire(at(100), Milli(50));
	expect(ledger.hasGaps()).toBe(false);
	ledger.account(4n, 5n, at(100));
	ledger.expire(at(200), Milli(200));
	expect(ledger.gaps(0n, 5n)).toEqual([{ start: 3n, end: 4n, since: at(100) }]);
	ledger.account(2n, 4n, at(200));
	ledger.expire(at(300), Milli(0));
	expect(ledger.covers(0n, 5n)).toBe(true);
});

test.each([0, 1, 4, 32, 128])("rolling expiration keeps only %i pending gaps", (retained) => {
	const ledger = new Ledger();
	for (let arrival = 0; arrival < retained * 4 + 8; arrival++) {
		ledger.account(BigInt(arrival * 2), BigInt(arrival * 2 + 1), at(arrival));
		ledger.expire(at(arrival), Milli(retained));
		const folded = Math.max(0, arrival - retained) * 2 + 1;
		expect(ledger.covers(0n, BigInt(folded))).toBe(true);
		expect(ledger.gaps(0n, BigInt(arrival * 2 + 1)).length).toBe(Math.min(arrival, retained));
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
		ledger.account(BigInt(start), BigInt(end), at(i));
		for (let n = start; n < end; n++) sequences.add(n);
		ledger.expire(at(i), Milli(2000));
	}
	const gaps = ledger.gaps(0n, 4128n);
	for (let i = 0; i < 4128; i++) {
		const sequence = BigInt(i);
		expect(ledger.covers(sequence, sequence + 1n)).toBe(sequences.has(i));
		expect(gaps.some((gap) => gap.start <= sequence && sequence < gap.end)).toBe(!sequences.has(i));
	}
	ledger.account(0n, 4128n, at(1024));
	ledger.expire(at(4000), Milli(2000));
	expect(ledger.hasGaps()).toBe(false);
	expect(ledger.covers(0n, 4128n)).toBe(true);
});

test("unaccount reopens a gap for a fresh grace", () => {
	const ledger = new Ledger();
	const grace = Milli(1000);
	ledger.account(0n, 10n, at(0));
	ledger.unaccount(3n, 5n, at(1000));
	expect(ledger.covers(0n, 3n) && ledger.covers(5n, 10n)).toBe(true);
	expect(ledger.covers(3n, 5n)).toBe(false);
	// Its grace runs from the reversal.
	ledger.expire(at(1500), grace);
	expect(ledger.covers(3n, 5n)).toBe(false);
	ledger.expire(at(2000), grace);
	expect(ledger.covers(0n, 10n)).toBe(true);
});
