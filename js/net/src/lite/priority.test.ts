import { expect, test } from "bun:test";
import { sendOrder } from "./priority.ts";

// The transport sends higher values first, so a track the subscriber cares about more has to
// outrank one it cares about less, whatever their group sequences are.
test("track priority dominates the group sequence", () => {
	expect(sendOrder({ priority: 2, sequence: 0 })).toBeGreaterThan(sendOrder({ priority: 1, sequence: 1_000_000 }));
	expect(sendOrder({ priority: 255, sequence: 0 })).toBeGreaterThan(
		sendOrder({ priority: 254, sequence: Number.MAX_SAFE_INTEGER }),
	);
});

// Newest-first within a track: a fresh group preempts one that is already falling behind.
test("a newer group outranks an older one on the same track", () => {
	expect(sendOrder({ priority: 1, sequence: 5 })).toBeGreaterThan(sendOrder({ priority: 1, sequence: 4 }));
	expect(sendOrder({ priority: 0, sequence: 1 })).toBeGreaterThan(sendOrder({ priority: 0, sequence: 0 }));
});

// An ordered subscription is playing through in sequence, so the oldest group in flight is the
// one it needs next.
test("ordered sends the oldest group first", () => {
	const ordered = true;
	expect(sendOrder({ priority: 1, sequence: 4, ordered })).toBeGreaterThan(
		sendOrder({ priority: 1, sequence: 5, ordered }),
	);
	expect(sendOrder({ priority: 1, sequence: 0, ordered })).toBeGreaterThan(
		sendOrder({ priority: 1, sequence: 1_000_000, ordered }),
	);
});

// Ordering within a track never costs it the track priority it was given.
test("ordered stays inside its priority", () => {
	expect(sendOrder({ priority: 2, sequence: 999, ordered: true })).toBeGreaterThan(
		sendOrder({ priority: 1, sequence: 0, ordered: true }),
	);
	expect(sendOrder({ priority: 1, sequence: 0, ordered: true })).toBeLessThan(
		sendOrder({ priority: 2, sequence: 0, ordered: false }),
	);
});

// Every send order stays an exact integer, so two distinct ranks never collapse into one.
// The top of the range lands exactly on the largest integer a double can represent.
test("send orders stay exact integers", () => {
	expect(sendOrder({ priority: 255, sequence: 2 ** 45 - 1 })).toBe(Number.MAX_SAFE_INTEGER);
	expect(sendOrder({ priority: 0, sequence: 0 })).toBe(0);
	expect(sendOrder({ priority: 255, sequence: 0, ordered: true })).toBe(Number.MAX_SAFE_INTEGER);
});

// A group sequence is a u53 on the wire, so a relay preserving an upstream sequence (or a
// producer numbering groups off a clock) still gets newest-first ordering.
test("large sequences keep their order", () => {
	const epoch = 1_770_000_000_000; // milliseconds, well past a 32-bit sequence
	expect(sendOrder({ priority: 1, sequence: epoch + 1 })).toBeGreaterThan(
		sendOrder({ priority: 1, sequence: epoch }),
	);
	expect(sendOrder({ priority: 1, sequence: 2 ** 40 + 1 })).toBeGreaterThan(
		sendOrder({ priority: 1, sequence: 2 ** 40 }),
	);
});

// A sequence past the group span must never carry into the track's bits and outrank a
// higher-priority track.
test("out of range ranks stay inside their own bits", () => {
	expect(sendOrder({ priority: 1, sequence: Number.MAX_SAFE_INTEGER })).toBeLessThan(
		sendOrder({ priority: 2, sequence: 0 }),
	);
	expect(sendOrder({ priority: 300, sequence: 0 })).toBe(sendOrder({ priority: 255, sequence: 0 }));
	expect(sendOrder({ priority: -1, sequence: -1 })).toBe(0);
});
