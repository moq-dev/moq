import { expect, test } from "bun:test";
import { U64 } from "./u64.ts";

test("U64 converts numbers at the 32-bit and 53-bit boundaries", () => {
	for (const n of [0, 2 ** 30, 2 ** 32 - 1, 2 ** 32, Number.MAX_SAFE_INTEGER]) {
		const v = U64.fromNumber(n);
		expect(v.toNumber()).toBe(n);
		expect(v.toBigInt()).toBe(BigInt(n));
		expect(v.toString()).toBe(String(n));
	}
	expect(U64.fromNumber(2 ** 32 + 5)).toEqual(new U64(1, 5));
});

test("U64.fromNumber rejects anything but a non-negative safe integer", () => {
	for (const n of [-1, 1.5, Number.NaN, Number.POSITIVE_INFINITY, 2 ** 53]) {
		expect(() => U64.fromNumber(n)).toThrow(RangeError);
	}
});

test("U64 holds 64 bits but converts to number only up to 2^53 - 1", () => {
	const max = U64.fromBigInt(2n ** 64n - 1n);
	expect(max).toEqual(U64.MAX);
	expect(max.toBigInt()).toBe(2n ** 64n - 1n);
	expect(max.toString()).toBe("18446744073709551615");
	expect(() => max.toNumber()).toThrow(/larger than 53-bits: 18446744073709551615/);
	expect(() => U64.fromBigInt(2n ** 53n).toNumber()).toThrow(RangeError);

	expect(() => U64.fromBigInt(2n ** 64n)).toThrow(RangeError);
	expect(() => U64.fromBigInt(-1n)).toThrow(RangeError);
	expect(() => new U64(2 ** 32, 0)).toThrow(RangeError);
	expect(() => new U64(0, 2 ** 32)).toThrow(RangeError);
	expect(() => new U64(0, -1)).toThrow(RangeError);
});

test("U64 compares and adds without converting", () => {
	const a = U64.fromNumber(2 ** 32 - 1);
	const b = a.add(1);
	expect(b).toEqual(new U64(1, 0));
	expect(a.compare(b)).toBeLessThan(0);
	expect(b.compare(a)).toBeGreaterThan(0);
	expect(a.compare(U64.fromNumber(2 ** 32 - 1))).toBe(0);
	expect(U64.MAX.compare(U64.ZERO)).toBeGreaterThan(0);
	expect(a.equals(b)).toBe(false);
	expect(b.equals(new U64(1, 0))).toBe(true);

	expect(U64.ZERO.add(Number.MAX_SAFE_INTEGER).toNumber()).toBe(Number.MAX_SAFE_INTEGER);
	expect(U64.fromBigInt(2n ** 64n - 2n).add(1)).toEqual(U64.MAX);
	expect(() => U64.MAX.add(1)).toThrow(RangeError);
	expect(() => a.add(-1)).toThrow(RangeError);
});
