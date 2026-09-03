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

// The fourth field bounds the last object in the end group. Dropping it on decode would
// let a publisher deliver past the Location the subscriber asked for.
test("draft-20 keeps the end object", () => {
	const bounded: Filter.Filter = {
		kind: "absolute",
		startGroup: 7n,
		startObject: 3n,
		endGroup: 9n,
		endObject: 4n,
	};
	expect([...Filter.encode(bounded, NEW)]).toEqual([0x07, 0x03, 0x02, 0x04]);
	expect(roundTrip(bounded, NEW)).toEqual(bounded);

	// Three fields leave the end group whole, which is a different filter.
	const whole: Filter.Filter = { kind: "absolute", startGroup: 7n, startObject: 3n, endGroup: 9n };
	expect([...Filter.encode(whole, NEW)]).toEqual([0x07, 0x03, 0x02]);
});

// Draft-19's AbsoluteRange ends on a group, so an object bound cannot be expressed.
test("draft-19 cannot bound the end object", () => {
	expect(() =>
		Filter.encode({ kind: "absolute", startGroup: 7n, startObject: 0n, endGroup: 9n, endObject: 4n }, OLD),
	).toThrow();
});

// OBJECTID_FILTER has an even id but is written with an explicit Length, so parity would
// read its length byte as the whole value and desync everything after it.
test("fill skips a length-prefixed range filter", () => {
	// count=2, 0x26 len=3 AABBCC, delta 1 -> 0x27 len=1 DD
	const encoded = new Uint8Array([0x02, 0x26, 0x03, 0xaa, 0xbb, 0xcc, 0x01, 0x01, 0xdd]);
	// No LOCATION_FILTER, so the fill inherits the subscription's; a Range Filter's presence
	// is what makes the publisher refuse it.
	expect(Filter.decodeFill(encoded, NEW)).toEqual({ filter: undefined, rangeFilters: true });
});

test("rejects too many fields", () => {
	expect(() => Filter.decode(new Uint8Array([0, 0, 0, 0, 0]), NEW)).toThrow();
});

// The canonical current-group join: an empty subscription filter paired with a fill that
// starts one group back.
test("fill encodes the current group join", () => {
	const encoded = Filter.encodeFill({ filter: { kind: "relative", groups: 1n }, rangeFilters: false }, NEW);
	// count=1, type=0x21, len=1, StartGroup=1
	expect([...encoded]).toEqual([0x01, 0x21, 0x01, 0x01]);
	expect(Filter.decodeFill(encoded, NEW)).toEqual({ filter: { kind: "relative", groups: 1n }, rangeFilters: false });
});

test("fill round trips", () => {
	const fills: Filter.Fill[] = [
		// An omitted filter inherits the subscription's, which is a different request from an
		// explicit unfiltered one.
		{ filter: undefined, rangeFilters: false },
		{ filter: { kind: "unfiltered" }, rangeFilters: false },
		{ filter: { kind: "relative", groups: 3n }, rangeFilters: false },
		{ filter: { kind: "absolute", startGroup: 4n, startObject: 0n, endGroup: 9n }, rangeFilters: false },
	];
	for (const fill of fills) {
		expect(Filter.decodeFill(Filter.encodeFill(fill, NEW), NEW)).toEqual(fill);
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
// Draft-17 replaced QUIC's varint with a leading-1-bits one. They agree below 64, so a
// wrong codec is invisible until group or object ids grow past that, and then Rust and JS
// stop understanding each other's filters.
test("draft-20 uses leading-ones varints above 63", () => {
	// 128 is 0x80_80 with leading ones, and 0x40_80 with the QUIC form.
	expect([...Filter.encode({ kind: "relative", groups: 128n }, NEW)]).toEqual([0x80, 0x80]);
	expect(roundTrip({ kind: "relative", groups: 128n }, NEW)).toEqual({ kind: "relative", groups: 128n });

	const wide: Filter.Filter = { kind: "absolute", startGroup: 300n, startObject: 64n };
	expect(roundTrip(wide, NEW)).toEqual({ ...wide, endGroup: undefined, endObject: undefined });
});

// A uint8 parameter is one raw byte, so a priority of 128 or more would be read as a
// multi-byte varint prefix and swallow the parameter after it.
test("fill skips a uint8 parameter whose value has a leading one", () => {
	// count=2, 0x20 (uint8) = 0x80, delta 1 -> 0x21 len=1 StartGroup=1
	const encoded = new Uint8Array([0x02, 0x20, 0x80, 0x01, 0x01, 0x01]);
	expect(Filter.decodeFill(encoded, NEW)).toEqual({ filter: { kind: "relative", groups: 1n }, rangeFilters: false });
});

test("fill skips allowed parameters it ignores", () => {
	// count=2, 0x20 (even, one varint) = 42, delta 1 -> 0x21 (odd, length prefixed)
	const encoded = new Uint8Array([0x02, 0x20, 0x2a, 0x01, 0x01, 0x02]);
	expect(Filter.decodeFill(encoded, NEW)).toEqual({ filter: { kind: "relative", groups: 2n }, rangeFilters: false });
});
