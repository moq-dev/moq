import { expect, test } from "bun:test";
import { Milli } from "@moq/clock";
import { ProtocolViolation } from "../error.ts";
import { Reader } from "../stream.ts";
import * as Filter from "./filter.ts";
import { joinFilter } from "./subscribe.ts";
import { Version } from "./version.ts";

const OLD = Version.DRAFT_18;
const NEW = Version.DRAFT_22;

test("legacy live joins do not exclude the prefix without a joining FETCH", () => {
	for (const version of [Version.DRAFT_18]) {
		expect([...Filter.encode(joinFilter(version), version)]).toEqual([3, 0, 0]);
	}
	expect([...Filter.encode(joinFilter(NEW), NEW)]).toEqual([0x01, 0x01]);
});

function roundTrip(filter: Filter.Filter, version: typeof OLD | typeof NEW): Filter.Filter {
	return Filter.decode(Filter.encode(filter, version), version);
}

test("draft-18 uses tags", () => {
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

// Draft-18 has a tag per case and none of them mean "two groups back", so refuse rather
// than silently sending a nearby filter the peer would honor.
test("draft-18 cannot name a relative group", () => {
	expect(() => Filter.encode({ kind: "relative", groups: 2n }, OLD)).toThrow();
});

test("rejects a backwards range", () => {
	for (const version of [OLD, NEW] as const) {
		expect(() =>
			Filter.encode({ kind: "absolute", startGroup: 9n, startObject: 0n, endGroup: 4n }, version),
		).toThrow();
	}
});

// Draft-18's AbsoluteRange ends on a group, so an object bound cannot be expressed.
test("draft-18 cannot bound the end object", () => {
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

// The canonical current-group join: an empty subscription filter paired with a fill that
// starts one group back.
test("fill encodes the current group join", () => {
	const encoded = Filter.encodeFill({ filter: { kind: "relative", groups: 1n }, rangeFilters: false }, NEW);
	// count=1, type=0x21, Relative Start, StartGroup=1
	expect([...encoded]).toEqual([0x01, 0x21, 0x01, 0x01]);
	expect(Filter.decodeFill(encoded, NEW)).toEqual({ filter: { kind: "relative", groups: 1n }, rangeFilters: false });
});

test("fill round trips", () => {
	const fills: Filter.Fill[] = [
		// An omitted filter inherits the subscription's, which is a different request from an
		// explicit unfiltered one.
		{ filter: undefined, rangeFilters: false },
		{ fillTimeout: Milli(0), filter: undefined, rangeFilters: false },
		{
			fillTimeout: Milli(250),
			filter: { kind: "relative", groups: 1n },
			groupOrder: "ascending",
			rangeFilters: false,
		},
		{ filter: { kind: "unfiltered" }, rangeFilters: false },
		{ filter: { kind: "relative", groups: 3n }, rangeFilters: false },
		{ filter: { kind: "absolute", startGroup: 4n, startObject: 0n, endGroup: 9n }, rangeFilters: false },
	];
	for (const fill of fills) {
		expect(Filter.decodeFill(Filter.encodeFill(fill, NEW), NEW)).toEqual(fill);
	}
});

// FILL_TIMEOUT sorts first, so the parameters after it are deltas from its id.
test("fill encodes its FILL_TIMEOUT first", () => {
	const fill: Filter.Fill = {
		fillTimeout: Milli(100),
		filter: { kind: "relative", groups: 1n },
		rangeFilters: false,
	};
	// count=2, 0x0A=100, delta 0x17 -> 0x21 Relative Start, StartGroup=1
	const encoded = Filter.encodeFill(fill, NEW);
	expect([...encoded]).toEqual([0x02, 0x0a, 0x64, 0x17, 0x01, 0x01]);
	expect(Filter.decodeFill(encoded, NEW)).toEqual(fill);
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
test("draft-22 uses leading-ones varints above 63", () => {
	// 128 is 0x80_80 with leading ones, and 0x40_80 with the QUIC form.
	expect([...Filter.encode({ kind: "relative", groups: 128n }, NEW)]).toEqual([0x01, 0x80, 0x80]);
	expect(roundTrip({ kind: "relative", groups: 128n }, NEW)).toEqual({ kind: "relative", groups: 128n });

	const wide: Filter.Filter = { kind: "absolute", startGroup: 300n, startObject: 64n };
	expect(roundTrip(wide, NEW)).toEqual({ ...wide, endGroup: undefined, endObject: undefined });
});

// A uint8 parameter is one raw byte, so a priority of 128 or more would be read as a
// multi-byte varint prefix and swallow the parameter after it.
test("fill skips a uint8 parameter whose value has a leading one", () => {
	// count=2, 0x20 (uint8) = 0x80, delta 1 -> 0x21 Relative Start, StartGroup=1
	const encoded = new Uint8Array([0x02, 0x20, 0x80, 0x01, 0x01, 0x01]);
	expect(Filter.decodeFill(encoded, NEW)).toEqual({ filter: { kind: "relative", groups: 1n }, rangeFilters: false });
});

test("fill skips allowed parameters it ignores", () => {
	// count=2, 0x20 (even, one varint) = 42, delta 1 -> 0x21 Relative Start, StartGroup=2
	const encoded = new Uint8Array([0x02, 0x20, 0x2a, 0x01, 0x01, 0x02]);
	expect(Filter.decodeFill(encoded, NEW)).toEqual({ filter: { kind: "relative", groups: 2n }, rangeFilters: false });
});

// Draft-22 section 9.20.9: a Location Filter Type, then only the fields it names.
test("draft-22 carries the Location Filter Type", () => {
	const cases: [Filter.Filter, number[]][] = [
		[{ kind: "unfiltered" }, [0x00]],
		[{ kind: "relative", groups: 1n }, [0x01, 0x01]],
		[{ kind: "absolute", startGroup: 7n, startObject: 3n }, [0x02, 0x07, 0x03]],
		[{ kind: "absolute", startGroup: 7n, startObject: 3n, endGroup: 9n }, [0x03, 0x07, 0x03, 0x02]],
		[
			{ kind: "absolute", startGroup: 7n, startObject: 3n, endGroup: 9n, endObject: 4n },
			[0x04, 0x07, 0x03, 0x02, 0x04],
		],
		[{ kind: "nextObject" }, [0x05]],
		// The type says absolute, so {0, 0} no longer has to borrow the unfiltered spelling.
		[{ kind: "absolute", startGroup: 0n, startObject: 0n }, [0x02, 0x00, 0x00]],
	];
	for (const [filter, bytes] of cases) {
		expect([...Filter.encode(filter, NEW)]).toEqual(bytes);
		// No Length: the parameter value is the type and its fields.
		expect([...Filter.encodeParam(filter, NEW)]).toEqual(bytes);
		expect(Filter.decode(new Uint8Array(bytes), NEW)).toEqual(filter);
	}
});

// With no Length, the type alone says where the value ends, so the bytes after it are left
// for the next parameter.
test("draft-22 reads only the named fields", async () => {
	const r = new Reader(undefined, new Uint8Array([0x05, 0x01, 0x02]), NEW);
	expect(await Filter.decodeParam(r, NEW)).toEqual({ kind: "nextObject" });
	expect(await r.u62()).toBe(1n);
	expect(await r.u62()).toBe(2n);
});

test("draft-22 rejects an unknown Location Filter Type", async () => {
	for (const type of [0x06, 0x3f]) {
		expect(() => Filter.decode(new Uint8Array([type, 0x00, 0x00]), NEW)).toThrow(ProtocolViolation);
		const r = new Reader(undefined, new Uint8Array([type, 0x00, 0x00]), NEW);
		await expect(Filter.decodeParam(r, NEW)).rejects.toThrow(ProtocolViolation);
	}
});

// The nested LOCATION_FILTER carries its type too, and the whole-track form the spec calls
// zero-length is type 0x00.
test("draft-22 fill carries the Location Filter Type", () => {
	// A two-byte group, so a Length (4) would differ from the type (3).
	const range: Filter.Filter = { kind: "absolute", startGroup: 128n, startObject: 0n, endGroup: 130n };
	expect([...Filter.encodeFill({ filter: range, rangeFilters: false }, NEW)]).toEqual([
		0x01, 0x21, 0x03, 0x80, 0x80, 0x00, 0x02,
	]);
	expect([...Filter.encodeFill({ filter: { kind: "nextObject" }, rangeFilters: false }, NEW)]).toEqual([
		0x01, 0x21, 0x05,
	]);
	expect([...Filter.encodeFill({ filter: { kind: "unfiltered" }, rangeFilters: false }, NEW)]).toEqual([
		0x01, 0x21, 0x00,
	]);

	const fills: Filter.Fill[] = [
		{ filter: undefined, rangeFilters: false },
		{ filter: { kind: "unfiltered" }, rangeFilters: false },
		{ filter: { kind: "nextObject" }, rangeFilters: false },
		{ filter: { kind: "relative", groups: 1n }, rangeFilters: false },
		{ filter: range, rangeFilters: false },
	];
	for (const fill of fills) {
		expect(Filter.decodeFill(Filter.encodeFill(fill, NEW), NEW)).toEqual(fill);
	}

	// A uint8, a typed filter, then another uint8: the filter must stop at its own fields.
	expect(Filter.decodeFill(new Uint8Array([0x03, 0x20, 0x80, 0x01, 0x05, 0x01, 0x01]), NEW)).toEqual({
		filter: { kind: "nextObject" },
		groupOrder: "ascending",
		rangeFilters: false,
	});
});

for (const version of [Version.DRAFT_22]) {
	test(`fill rejects invalid group order on ${version}`, () => {
		for (const value of [0, 3, 255]) {
			expect(() => Filter.decodeFill(new Uint8Array([1, 0x22, value]), version)).toThrow(/group order/);
		}
		for (const value of [1, 2]) {
			expect(Filter.decodeFill(new Uint8Array([1, 0x22, value]), version)).toEqual({
				filter: undefined,
				groupOrder: value === 1 ? "ascending" : "descending",
				rangeFilters: false,
			});
		}
	});
}

test("fill preserves its group order with inherited and explicit filters", () => {
	for (const [groupOrder, wire] of [
		["ascending", 1],
		["descending", 2],
	] as const) {
		for (const [filter, prefix] of [
			[undefined, [1, 0x22]],
			[{ kind: "relative", groups: 1n }, [2, 0x21, 1, 1, 1]],
		] as const) {
			const fill: Filter.Fill = { filter, groupOrder, rangeFilters: false };
			const encoded = new Uint8Array([...prefix, wire]);
			expect(Filter.encodeFill(fill, NEW)).toEqual(encoded);
			expect(Filter.decodeFill(encoded, NEW)).toEqual(fill);
		}
	}
});

test("fill retains its group order between ignored parameter values", () => {
	// Priority is a raw uint8; the range filter after GROUP_ORDER is length-prefixed.
	const encoded = new Uint8Array([3, 0x20, 0x80, 2, 1, 3, 0]);
	expect(Filter.decodeFill(encoded, NEW)).toEqual({
		filter: undefined,
		groupOrder: "ascending",
		rangeFilters: true,
	});
});

test("fill refuses repeated group order even when the values agree", () => {
	for (const repeated of [1, 2]) {
		const encoded = new Uint8Array([2, 0x22, 1, 0, repeated]);
		expect(() => Filter.decodeFill(encoded, NEW)).toThrow(/duplicate GROUP_ORDER/);
	}
});
