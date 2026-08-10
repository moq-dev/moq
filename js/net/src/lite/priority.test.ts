import { expect, test } from "bun:test";
import { sendOrder } from "./priority.ts";

// The transport sends higher values first, so a track the subscriber cares about more has to
// outrank one it cares about less, whatever their group sequences are.
test("track priority dominates the group sequence", () => {
	expect(sendOrder(2, 0)).toBeGreaterThan(sendOrder(1, 1_000_000));
	expect(sendOrder(255, 0)).toBeGreaterThan(sendOrder(254, Number.MAX_SAFE_INTEGER));
});

// Newest-first within a track: a fresh group preempts one that is already falling behind.
test("a newer group outranks an older one on the same track", () => {
	expect(sendOrder(1, 5)).toBeGreaterThan(sendOrder(1, 4));
	expect(sendOrder(0, 1)).toBeGreaterThan(sendOrder(0, 0));
});

// Every send order stays an exact integer, so two distinct ranks never collapse into one.
// The top of the range lands exactly on the largest integer a double can represent.
test("send orders stay exact integers", () => {
	expect(sendOrder(255, 2 ** 45 - 1)).toBe(Number.MAX_SAFE_INTEGER);
	expect(sendOrder(0, 0)).toBe(0);
});

// A group sequence is a u53 on the wire, so a relay preserving an upstream sequence (or a
// producer numbering groups off a clock) still gets newest-first ordering.
test("large sequences keep their order", () => {
	const epoch = 1_770_000_000_000; // milliseconds, well past a 32-bit sequence
	expect(sendOrder(1, epoch + 1)).toBeGreaterThan(sendOrder(1, epoch));
	expect(sendOrder(1, 2 ** 40 + 1)).toBeGreaterThan(sendOrder(1, 2 ** 40));
});

// A sequence past the group span must never carry into the track's bits and outrank a
// higher-priority track.
test("out of range ranks stay inside their own bits", () => {
	expect(sendOrder(1, Number.MAX_SAFE_INTEGER)).toBeLessThan(sendOrder(2, 0));
	expect(sendOrder(300, 0)).toBe(sendOrder(255, 0));
	expect(sendOrder(-1, -1)).toBe(0);
});
