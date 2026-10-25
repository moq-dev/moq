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

test("Timestamp.as converts within the safe range", () => {
	expect(Timestamp.fromMillis(2000).as(Timescale.MICRO)).toBe(2_000_000);
	expect(Timestamp.fromMillis(2000).as(Timescale.MILLI)).toBe(2000);
	expect(Timestamp.fromMicros(2_000_000).as(Timescale.MILLI)).toBe(2000);
});

test("Timestamp.as rejects conversions past the safe integer range", () => {
	// Source value and destination scale are each legal; the converted wire value is not.
	expect(() => Timestamp.fromMillis(2000).as(Timescale(Number.MAX_SAFE_INTEGER))).toThrow(RangeError);
});

test("Timestamp.isBefore orders across scales exactly", () => {
	expect(Timestamp.fromMillis(1).isBefore(Timestamp.fromMicros(1_001))).toBe(true);
	expect(Timestamp.fromMillis(1).isBefore(Timestamp.fromMicros(1_000))).toBe(false);
	expect(Timestamp.fromMicros(1_000).isBefore(Timestamp.fromMillis(1))).toBe(false);
	// Products past 2^53 still compare exactly.
	const micros = new Timestamp(Number.MAX_SAFE_INTEGER, Timescale.MICRO);
	const millis = new Timestamp(Number.MAX_SAFE_INTEGER - 1, Timescale.MILLI);
	expect(micros.isBefore(millis)).toBe(true);
	expect(millis.isBefore(micros)).toBe(false);
	expect(new Timestamp(0.5, Timescale.SECOND).isBefore(Timestamp.fromMillis(501))).toBe(true);
});
