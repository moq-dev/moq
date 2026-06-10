import { expect, test } from "bun:test";
import { deepEqual, diff, merge } from "./diff.ts";

// Applying the patch to old should reproduce new (RFC 7396 semantics).
function assertRoundtrip(oldVal: unknown, newVal: unknown) {
	const result = diff(oldVal, newVal);
	expect(result.forcedSnapshot).toBe(false);
	expect(merge(oldVal, result.patch)).toEqual(newVal as object);
}

test("changed scalar", () => {
	assertRoundtrip({ a: 1, b: 2 }, { a: 1, b: 3 });
});

test("added key", () => {
	const result = diff({ a: 1 }, { a: 1, b: 2 });
	expect(result.forcedSnapshot).toBe(false);
	expect(result.patch).toEqual({ b: 2 });
});

test("removed key is null", () => {
	const result = diff({ a: 1, b: 2 }, { a: 1 });
	expect(result.forcedSnapshot).toBe(false);
	expect(result.patch).toEqual({ b: null });
	assertRoundtrip({ a: 1, b: 2 }, { a: 1 });
});

test("nested object only includes changed keys", () => {
	const result = diff({ o: { x: 1, y: 2 } }, { o: { x: 1, y: 9 } });
	expect(result.forcedSnapshot).toBe(false);
	expect(result.patch).toEqual({ o: { y: 9 } });
});

test("changed array is a wholesale delta", () => {
	const result = diff({ a: [1, 2] }, { a: [1, 2, 3] });
	expect(result.forcedSnapshot).toBe(false);
	expect(result.patch).toEqual({ a: [1, 2, 3] });
	assertRoundtrip({ a: [1, 2] }, { a: [1, 2, 3] });
});

test("added array is a delta", () => {
	const result = diff({ a: 1 }, { a: 1, b: [1] });
	expect(result.forcedSnapshot).toBe(false);
	expect(result.patch).toEqual({ b: [1] });
});

test("nested array is a delta", () => {
	const result = diff({ o: { x: 1 } }, { o: { x: 1, list: [1] } });
	expect(result.forcedSnapshot).toBe(false);
	expect(result.patch).toEqual({ o: { list: [1] } });
	assertRoundtrip({ o: { x: 1 } }, { o: { x: 1, list: [1] } });
});

test("set to null forces snapshot", () => {
	expect(diff({ a: 1 }, { a: null }).forcedSnapshot).toBe(true);
});

test("replacing object with scalar", () => {
	assertRoundtrip({ a: { x: 1 } }, { a: 5 });
});

test("non-object root forces snapshot", () => {
	expect(diff(1, 2).forcedSnapshot).toBe(true);
});

test("deepEqual", () => {
	expect(deepEqual({ a: [1, { b: 2 }] }, { a: [1, { b: 2 }] })).toBe(true);
	expect(deepEqual({ a: 1 }, { a: 1, b: 2 })).toBe(false);
	expect(deepEqual([1, 2], [1, 2, 3])).toBe(false);
});
