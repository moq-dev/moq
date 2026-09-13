import { expect, test } from "bun:test";
import { Timescale, Timestamp } from "./time.ts";

test("Timescale accepts the safe integer range", () => {
	expect(Timescale(1)).toBe(Timescale.SECOND);
	expect(Timescale(Number.MAX_SAFE_INTEGER)).toBe(Number.MAX_SAFE_INTEGER as Timescale);
});

test("Timescale rejects values that cannot encode losslessly", () => {
	for (const value of [
		0,
		-1,
		1.5,
		Number.NaN,
		Number.POSITIVE_INFINITY,
		Number.NEGATIVE_INFINITY,
		Number.MAX_SAFE_INTEGER + 1,
		2 ** 62,
		2 ** 62 + 1,
		1e100,
	]) {
		expect(() => Timescale(value)).toThrow(RangeError);
	}
});

test("Timestamp rejects negative values", () => {
	expect(() => new Timestamp(-1, Timescale.MILLI)).toThrow();
	expect(() => Timestamp.fromMicros(-1)).toThrow();
});

test("Timestamp rejects non-finite values", () => {
	expect(() => new Timestamp(Number.NaN, Timescale.MILLI)).toThrow();
	expect(() => Timestamp.fromMillis(Number.POSITIVE_INFINITY)).toThrow();
});

test("Timestamp accepts fractional values in the safe range", () => {
	expect(new Timestamp(1.5, Timescale.MILLI).value).toBe(1.5);
	expect(new Timestamp(0, Timescale.MILLI).value).toBe(0);
	expect(new Timestamp(Number.MAX_SAFE_INTEGER, Timescale.MILLI).value).toBe(Number.MAX_SAFE_INTEGER);
});

test("Timestamp rejects values past the safe integer range", () => {
	expect(() => new Timestamp(Number.MAX_SAFE_INTEGER + 1, Timescale.MILLI)).toThrow(RangeError);
	expect(() => new Timestamp(2 ** 62, Timescale.MILLI)).toThrow(RangeError);
});
