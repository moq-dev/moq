import { expect, test } from "bun:test";
import { sendOrder } from "./priority.ts";

// The last position that still fits below the track priority.
const MAX_POSITION = 2 ** 45 - 1;

// The transport sends higher values first, so a track the subscriber cares about more has to
// outrank one it cares about less, wherever their groups sit in their own queues.
test("track priority dominates the position", () => {
	expect(sendOrder({ priority: 2, position: 1_000_000 })).toBeGreaterThan(sendOrder({ priority: 1, position: 0 }));
	expect(sendOrder({ priority: 255, position: MAX_POSITION })).toBeGreaterThan(
		sendOrder({ priority: 254, position: 0 }),
	);
});

// Position 0 is the group its subscription wants sent next, so it goes first.
test("an earlier position outranks a later one", () => {
	expect(sendOrder({ priority: 1, position: 0 })).toBeGreaterThan(sendOrder({ priority: 1, position: 1 }));
	expect(sendOrder({ priority: 1, position: 1 })).toBeGreaterThan(sendOrder({ priority: 1, position: 2 }));
});

// The reason positions are used instead of sequences: two tracks at the same priority each get
// their next group out, rather than the one with larger group numbers starving the other.
test("equal priorities tie at the same position", () => {
	expect(sendOrder({ priority: 3, position: 0 })).toBe(sendOrder({ priority: 3, position: 0 }));
	expect(sendOrder({ priority: 3 })).toBe(sendOrder({ priority: 3, position: 0 }));
});

// Every send order stays an exact integer, so two distinct ranks never collapse into one.
// The top of the range lands exactly on the largest integer a double can represent.
test("send orders stay exact integers", () => {
	expect(sendOrder({ priority: 255, position: 0 })).toBe(Number.MAX_SAFE_INTEGER);
	expect(sendOrder({ priority: 0, position: MAX_POSITION })).toBe(0);
});

// A position past the space left for it must never carry into the track's bits and outrank a
// higher-priority track.
test("out of range ranks stay inside their own bits", () => {
	expect(sendOrder({ priority: 1, position: Number.MAX_SAFE_INTEGER })).toBeLessThan(
		sendOrder({ priority: 2, position: 0 }),
	);
	expect(sendOrder({ priority: 300, position: 0 })).toBe(sendOrder({ priority: 255, position: 0 }));
	expect(sendOrder({ priority: -1, position: -1 })).toBe(sendOrder({ priority: 0, position: 0 }));
});
