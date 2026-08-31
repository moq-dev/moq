import { expect, test } from "bun:test";
import * as Filter from "./filter.ts";
import { Version } from "./version.ts";

const OLD = Version.DRAFT_19;
const NEW = Version.DRAFT_20;

function roundTrip(filter: Filter.Filter, version: typeof OLD | typeof NEW): Filter.Filter {
	return Filter.decode(Filter.encode(filter, version), version);
}

test("draft-20 selects the meaning by field count", () => {
	expect([...Filter.encode({ kind: "nextObject" }, NEW)]).toEqual([0x00, 0x00]);
	expect([...Filter.encode({ kind: "relative", groups: 0n }, NEW)]).toEqual([0x00]);
	expect([...Filter.encode({ kind: "relative", groups: 1n }, NEW)]).toEqual([0x01]);
	expect([...Filter.encode({ kind: "absolute", startGroup: 7n, startObject: 3n }, NEW)]).toEqual([0x07, 0x03]);
	// End is a delta from the start group, not an absolute group id.
	expect([...Filter.encode({ kind: "absolute", startGroup: 7n, startObject: 3n, endGroup: 9n }, NEW)]).toEqual([
		0x07, 0x03, 0x02,
	]);
});

test("draft-20 round trips", () => {
	const filters: Filter.Filter[] = [
		{ kind: "unfiltered" },
		{ kind: "nextObject" },
		{ kind: "relative", groups: 0n },
		{ kind: "relative", groups: 5n },
		{ kind: "absolute", startGroup: 12n, startObject: 0n, endGroup: undefined },
		{ kind: "absolute", startGroup: 12n, startObject: 4n, endGroup: 20n },
	];
	for (const filter of filters) {
		expect(roundTrip(filter, NEW)).toEqual(filter);
	}
});

// An open ended absolute {0, 0} is defined as equivalent to unfiltered, so it must not
// collide with the two-zero-field spelling of Next Object.
test("draft-20 absolute origin is unfiltered", () => {
	const origin: Filter.Filter = { kind: "absolute", startGroup: 0n, startObject: 0n };
	expect(Filter.encode(origin, NEW).length).toBe(0);
	expect(roundTrip(origin, NEW)).toEqual({ kind: "unfiltered" });
});

test("draft-19 uses tags", () => {
	expect([...Filter.encode({ kind: "nextObject" }, OLD)]).toEqual([0x02]);
	expect([...Filter.encode({ kind: "relative", groups: 0n }, OLD)]).toEqual([0x01]);

	const filters: Filter.Filter[] = [
		{ kind: "nextObject" },
		{ kind: "relative", groups: 0n },
		{ kind: "absolute", startGroup: 12n, startObject: 4n, endGroup: undefined },
		{ kind: "absolute", startGroup: 12n, startObject: 4n, endGroup: 20n },
	];
	for (const filter of filters) {
		expect(roundTrip(filter, OLD)).toEqual(filter);
	}
});

// Draft-19 has a tag per case and none of them mean "two groups back", so refuse rather
// than silently sending a nearby filter the peer would honor.
test("draft-19 cannot name a relative group", () => {
	expect(() => Filter.encode({ kind: "relative", groups: 2n }, OLD)).toThrow();
});

test("rejects a backwards range", () => {
	for (const version of [OLD, NEW] as const) {
		expect(() =>
			Filter.encode({ kind: "absolute", startGroup: 9n, startObject: 0n, endGroup: 4n }, version),
		).toThrow();
	}
});

test("rejects too many fields", () => {
	expect(() => Filter.decode(new Uint8Array([0, 0, 0, 0, 0]), NEW)).toThrow();
});

// The canonical current-group join: an empty subscription filter paired with a fill that
// starts one group back.
test("fill encodes the current group join", () => {
	const encoded = Filter.encodeFill({ kind: "relative", groups: 1n }, NEW);
	// count=1, type=0x21, len=1, StartGroup=1
	expect([...encoded]).toEqual([0x01, 0x21, 0x01, 0x01]);
	expect(Filter.decodeFill(encoded, NEW)).toEqual({ kind: "relative", groups: 1n });
});

test("fill round trips", () => {
	const filters: Filter.Filter[] = [
		{ kind: "unfiltered" },
		{ kind: "relative", groups: 3n },
		{ kind: "absolute", startGroup: 4n, startObject: 0n, endGroup: 9n },
	];
	for (const filter of filters) {
		expect(Filter.decodeFill(Filter.encodeFill(filter, NEW), NEW)).toEqual(filter);
	}
});

// A parameter the draft does not allow inside the scope is a violation, not something to
// skip past.
test("fill rejects a disallowed parameter", () => {
	// count=1, type=0x10 (FORWARD), value=0
	expect(() => Filter.decodeFill(new Uint8Array([0x01, 0x10, 0x00]), NEW)).toThrow();
});

// An allowed parameter we ignore still has to be consumed, or the keys after it desync.
// Parity decides how many bytes that is.
test("fill skips allowed parameters it ignores", () => {
	// count=2, 0x20 (even, one varint) = 42, delta 1 -> 0x21 (odd, length prefixed)
	const encoded = new Uint8Array([0x02, 0x20, 0x2a, 0x01, 0x01, 0x02]);
	expect(Filter.decodeFill(encoded, NEW)).toEqual({ kind: "relative", groups: 2n });
});
