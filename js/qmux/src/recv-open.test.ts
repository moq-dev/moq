import { describe, expect, test } from "bun:test";
import { RecvOpen } from "./recv-open.ts";

describe("RecvOpen", () => {
	test("an unseen lower index stays openable while seen ones close", () => {
		const open = new RecvOpen();
		open.record(5n);
		expect(open.isClosed(5n)).toBe(true);
		for (const index of [0n, 4n, 6n]) expect(open.isClosed(index)).toBe(false);

		open.record(2n);
		expect(open.isClosed(2n)).toBe(true);
		for (const index of [0n, 1n, 3n, 4n]) expect(open.isClosed(index)).toBe(false);

		for (const index of [0n, 1n, 3n, 4n]) open.record(index);
		for (let index = 0n; index <= 5n; index++) expect(open.isClosed(index)).toBe(true);
		expect(open.holes).toBe(0);
	});

	test("a jump to a high index tracks one range", () => {
		const open = new RecvOpen();
		open.record((1n << 40n) - 1n);
		expect(open.holes).toBe(1);
		expect(open.isClosed(7n)).toBe(false);
	});

	test("a pinned unseen index doesn't grow the state as later streams close", () => {
		// The peer leaves stream 0 unseen and keeps opening (and closing) the next
		// one. Each stays seen, but none of them may cost a lasting entry.
		const open = new RecvOpen();
		for (let index = 1n; index <= 10_000n; index++) open.record(index);
		expect(open.holes).toBe(1);
		expect(open.isClosed(0n)).toBe(false);
		expect(open.isClosed(10_000n)).toBe(true);
	});
});
